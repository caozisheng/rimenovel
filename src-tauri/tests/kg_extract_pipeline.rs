use rimenovel_lib::core::kg::{Identity, TextVersion};
use rimenovel_lib::core::kg_extract::{
    decode_stage_a, decode_stage_b, prepare_spans, resolve_local_identities, rewrite_refined_event_keys,
};
use serde_json::json;

fn text() -> TextVersion {
    TextVersion {
        id: 41,
        book_id: 7,
        chapter_idx: 3,
        source: "original".into(),
        content: "晨雾里，阿岚抵达渡口。\n\n阿岚把信交给舟子。".into(),
        content_hash: "fixture-text-v1".into(),
        generated_id: None,
    }
}

#[test]
fn long_cjk_paragraph_never_slices_at_invalid_utf8_boundary() {
    let mut fixture = text();
    fixture.content = "甲乙丙丁戊己庚辛壬癸".into();
    let spans = prepare_spans(&fixture, 5).unwrap();
    assert_eq!(spans.iter().map(|span| span.text.as_str()).collect::<String>(), fixture.content);
    assert!(spans.iter().all(|span| fixture.content.is_char_boundary(span.start_byte)
        && fixture.content.is_char_boundary(span.end_byte)));
}

#[test]
fn recorded_stage_responses_keep_utf8_evidence_and_replay_identity_keys() {
    let text = text();
    let spans = prepare_spans(&text, 18).unwrap();
    assert!(spans.len() >= 2);
    assert!(spans.iter().all(|span| text.content.is_char_boundary(span.start_byte)
        && text.content.is_char_boundary(span.end_byte)));

    let stage_a = decode_stage_a(json!({
        "schema_version": "kg-stage-a/v1",
        "chapter_idx": 3,
        "summary": "抵达并交付信件",
        "open_threads": ["信件去向"],
        "mentions": [{
            "mention_id": "m-aran", "name": "阿岚", "entity_type": "character",
            "candidate_keys": [], "evidence_refs": [spans[0].key]
        }],
        "events": [{
            "key": "local-arrival", "title": "抵达渡口", "category": "journey",
            "chapter_idx": 3, "observed_chapter": 3, "observed_order": 1,
            "temporal_scope": "current", "assertion_kind": "confirmed",
            "participants": ["m-aran"], "location": null, "summary": "抵达",
            "outcome": "开始交付", "cause_keys": [], "evidence_refs": [spans[0].key]
        }]
    }), &text, &spans).unwrap();
    let identities = resolve_local_identities(&text, &stage_a, &[], &[]);
    assert_eq!(identities.identities.len(), 1);
    let replay = resolve_local_identities(&text, &stage_a, &[], &[]);
    assert_eq!(identities.mention_keys, replay.mention_keys);

    let stage_b = decode_stage_b(json!({
        "schema_version": "kg-stage-b/v1", "chapter_idx": 3,
        "facts": [], "relations": [], "changes": [], "preconditions": [], "unresolved": []
    }), &text, &spans).unwrap();
    assert_eq!(stage_b.chapter_idx, text.chapter_idx);
}

#[test]
fn a_single_literal_name_match_is_only_an_identity_candidate() {
    let text = text();
    let spans = prepare_spans(&text, 128).unwrap();
    let stage = decode_stage_a(json!({
        "schema_version":"kg-stage-a/v1", "chapter_idx":3, "summary":"", "open_threads":[],
        "mentions":[{"mention_id":"m", "name":"阿岚", "entity_type":"character", "candidate_keys":[], "evidence_refs":[spans[0].key]}],
        "events":[]
    }), &text, &spans).unwrap();
    let known = vec![Identity {key:"old-aran".into(),entity_type:"character".into(),name:"阿岚".into(),aliases:vec![]}];
    let resolution = resolve_local_identities(&text, &stage, &known, &[]);
    assert_ne!(resolution.mention_keys["m"], "old-aran");
    assert!(!resolution.unresolved.is_empty());
}

#[test]
fn refinement_preserves_prior_event_only_when_evidence_overlaps() {
    let key = rewrite_refined_event_keys(
        vec![
            ("fresh-overlap".to_owned(), vec!["sp-3-0-10".to_owned()]),
            ("fresh-other".to_owned(), vec!["sp-3-21-33".to_owned()]),
        ],
        vec![("persisted-event".to_owned(), vec!["sp-3-0-10".to_owned()])],
    );
    assert_eq!(key.get("fresh-overlap"), Some(&"persisted-event".to_owned()));
    assert!(!key.contains_key("fresh-other"));
}

#[test]
fn recorded_response_rejects_evidence_not_preallocated_for_text_version() {
    let text = text();
    let spans = prepare_spans(&text, 128).unwrap();
    let err = decode_stage_b(json!({
        "schema_version": "kg-stage-b/v1", "chapter_idx": 3,
        "facts": [{
            "key": "f-1", "subject_key": "person", "predicate": "location",
            "value": {"kind":"scalar", "value":"dock"}, "chapter_idx": 3,
            "observed_chapter": 3, "observed_order": 1, "temporal_scope":"current",
            "assertion_kind":"confirmed", "speaker_key": null,
            "evidence_refs": ["untrusted-offset"]
        }], "relations": [], "changes": [], "preconditions": [], "unresolved": []
    }), &text, &spans).unwrap_err();
    assert!(err.contains("evidence"));
}

#[test]
fn result_fact_cannot_bypass_discontinuous_state_change() {
    use rimenovel_lib::core::kg::{ReducedState,StageB,TypedValue};
    let stage:StageB = serde_json::from_value(json!({
        "schema_version":"kg-stage-b/v1","chapter_idx":3,
        "facts":[{"key":"result","subject_key":"sword","predicate":"owner","value":{"kind":"entity_ref","value":"c"},"chapter_idx":3,"observed_chapter":3,"observed_order":1,"temporal_scope":"current","assertion_kind":"confirmed","speaker_key":null,"evidence_refs":["span"]}],
        "relations":[],"preconditions":[],"unresolved":[],
        "changes":[{"key":"transfer","event_key":"event","subject_key":"sword","predicate":"owner","operation":"set","before":{"kind":"entity_ref","value":"b"},"after":{"kind":"entity_ref","value":"c"},"chapter_idx":3,"observed_chapter":3,"observed_order":1,"temporal_scope":"current","assertion_kind":"confirmed","evidence_refs":["span"]}]
    })).unwrap();
    let mut prior = ReducedState::default();
    prior.values.insert(("sword".into(),"owner".into()),TypedValue {kind:"entity_ref".into(),value:Some("a".into())});
    let clusters = rimenovel_lib::core::kg_extract::semantic_conflict_clusters(&stage,&prior);
    assert_eq!(clusters.len(),1);
    assert!(clusters[0].1.contains(&"transfer".into()));
    assert!(clusters[0].1.contains(&"result".into()));
}

struct RecordedClient {
    calls: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl rimenovel_lib::llm::LlmClient for RecordedClient {
    async fn structured(&self, req:rimenovel_lib::llm::StructuredRequest<'_>) -> Result<serde_json::Value,rimenovel_lib::llm::LlmError> {
        self.calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        let payload:serde_json::Value = serde_json::from_str(&req.messages.last().unwrap().content).unwrap();
        let idx = payload["chapter_idx"].as_i64().unwrap();
        let span = payload["source_spans"][0]["key"].as_str().unwrap();
        match req.schema_name {
            "kg_stage_a" => Ok(json!({
                "schema_version":"kg-stage-a/v1", "chapter_idx":idx, "summary":"阿岚抵达渡口", "open_threads":[],
                "mentions":[{"mention_id":"m-aran","name":"阿岚","entity_type":"character","candidate_keys":[],"evidence_refs":[span]}],
                "events":[{"key":"local-arrival","title":"抵达渡口","category":"journey","chapter_idx":idx,"observed_chapter":idx,"observed_order":1,"temporal_scope":"current","assertion_kind":"confirmed","participants":["m-aran"],"location":null,"summary":"抵达","outcome":"到达","cause_keys":[],"evidence_refs":[span]}]
            })),
            "kg_stage_b" => {
                let identity = payload["mention_identity_map"]["m-aran"].as_str().unwrap();
                Ok(json!({
                    "schema_version":"kg-stage-b/v1","chapter_idx":idx,
                    "facts":[{"key":"name-local","subject_key":identity,"predicate":"name","value":{"kind":"scalar","value":"阿岚"},"chapter_idx":idx,"observed_chapter":idx,"observed_order":1,"temporal_scope":"current","assertion_kind":"confirmed","speaker_key":null,"evidence_refs":[span]}],
                    "relations":[],"changes":[],"preconditions":[],"unresolved":[]
                }))
            }
            other => Err(rimenovel_lib::llm::LlmError::Config(format!("unexpected recorded request {other}"))),
        }
    }

    async fn stream_text(&self, _req:rimenovel_lib::llm::TextRequest, _delta:std::sync::Arc<dyn for<'a> Fn(&'a str)+Send+Sync>) -> Result<String,rimenovel_lib::llm::LlmError> {
        Err(rimenovel_lib::llm::LlmError::Config("extraction must not request prose".into()))
    }
}

#[tokio::test]
async fn recorded_stages_commit_complete_revision_and_replay_without_api_calls() {
    use rimenovel_lib::commands::AppState;
    use rimenovel_lib::core::kg::{GraphQuery,RuntimeConfig};
    use rimenovel_lib::store::books::{create_book_with_chapters,NewChapter};
    let conn = rusqlite::Connection::open_in_memory().unwrap();
    rimenovel_lib::store::Db::apply_pragmas(&conn).unwrap();
    rimenovel_lib::store::Db::new(&conn).migrate().unwrap();
    let book = create_book_with_chapters(&conn,"录制样例",None,"txt","fixture.txt",&[
        NewChapter {idx:1,title:"渡口".into(),text:"阿岚抵达渡口。".into(),token_est:12},
    ]).unwrap();
    let text = rimenovel_lib::store::kg::text_version(&conn,book,1,"original",None).unwrap();
    let state = AppState {db:parking_lot::Mutex::new(Some(conn))};
    let config = RuntimeConfig {provider_id:1,model_extract:"recorded".into(),context_limit:32768,output_reserve:4096,prompt_version:"fixture/v1".into()};
    let client = RecordedClient {calls:std::sync::atomic::AtomicUsize::new(0)};
    let revision = rimenovel_lib::core::kg_extract::extract(&state,&client,text.clone(),&config,None).await.unwrap();
    let replay = rimenovel_lib::core::kg_extract::extract(&state,&client,text.clone(),&config,None).await.unwrap();
    assert_eq!(revision,replay);
    assert_eq!(client.calls.load(std::sync::atomic::Ordering::SeqCst),2);
    let lock = state.db.lock();
    let conn = lock.as_ref().unwrap();
    let bundle = rimenovel_lib::store::kg::active_bundle(conn,text.id).unwrap().unwrap();
    assert_eq!(bundle.stage_a.events.len(),1);
    assert_eq!(bundle.stage_b.facts.len(),1);
    assert_eq!(bundle.stage_a.events[0].participants[0],bundle.stage_b.facts[0].subject_key);
    assert!(rimenovel_lib::store::kg::ready(conn,text.id).unwrap());
    let graph = rimenovel_lib::store::kg::internal_view(conn,&GraphQuery {book_id:book,scope:"global".into(),purpose:"reference".into(),chapter_idx:Some(1)}).unwrap();
    assert_eq!(graph.nodes[0].name,"阿岚");
    assert_eq!(bundle.spans[0].text,"阿岚抵达渡口。");
}
