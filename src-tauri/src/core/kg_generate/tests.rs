use super::*;
use crate::core::kg::{EditRecord, GraphView, GraphNode};
use serde_json::json;

fn view(purpose: &str, chapter: i64) -> GraphView {
    serde_json::from_value(json!({"nodes":[],"edges":[],"events":[],"purpose":purpose,
        "reader_progress":chapter,"chapter_idx":chapter,"fog":null,"revealed":false,
        "ready":true,"unresolved":[],"revision":null,"drift_score":null,
        "reading_line_version":1,"divergence_idx":null})).unwrap()
}

fn context() -> GenerationContext {
    GenerationContext { book_id:1, chapter_idx:2, reading_line_version:1, digest:String::new(),
        actual:view("actual",1), spec:view("spec",2), previous_text:"已经发生的故事".into(),
        reference_text:Some("原作未来只是参考".into()), style_guide:None,
        directives:vec!["不要伤害信使".into()], drift_pref:0.3 }
}

fn config() -> RuntimeConfig {
    RuntimeConfig { provider_id:1, model_extract:"extract".into(), context_limit:32768,
        output_reserve:4096, prompt_version:"test".into() }
}

#[test]
fn first_chapter_never_diverges_and_inactive_edits_do_not_count() {
    let mut edits=vec![EditRecord { id:1,target_type:"directive".into(),target_id:"1".into(),
        action:"upsert".into(),after_json:Some("{}".into()),effective_from:1,active:true }];
    assert_eq!(divergence_from_edits(&edits),Some(2));
    edits[0].active=false;
    assert_eq!(divergence_from_edits(&edits),None);
}

#[test]
fn revert_revival_digest_ignores_line_and_revision_but_not_actual_content() {
    let original=context();
    let mut reverted=original.clone();
    reverted.reading_line_version=9;
    reverted.actual.reading_line_version=9;
    reverted.spec.reading_line_version=9;
    reverted.actual.revision=Some(88);
    assert_eq!(dependency_digest(&original,&config(),&json!({"actual_hash":"a"})).unwrap(),
        dependency_digest(&reverted,&config(),&json!({"actual_hash":"a"})).unwrap());
    assert_ne!(dependency_digest(&original,&config(),&json!({"actual_hash":"a"})).unwrap(),
        dependency_digest(&original,&config(),&json!({"actual_hash":"b"})).unwrap());
}

#[test]
fn hard_directives_and_actual_state_are_not_truncated_to_fit_reference() {
    let mut ctx=context();
    ctx.reference_text=Some("未来秘密".repeat(10000));
    let messages=write_messages(&ctx,&config(),&json!({"life_status":"dead"}),&view("actual",1)).unwrap();
    let prompt=&messages[1].content;
    assert!(prompt.contains("不要伤害信使"));
    assert!(prompt.contains("dead"));
    assert!(!prompt.contains(&"未来秘密".repeat(10000)));
    let mut small=config(); small.context_limit=5;
    assert!(write_messages(&ctx,&small,&json!({}),&view("actual",1)).is_err());
}

#[test]
fn actual_digest_cannot_be_polluted_by_original_future_graph() {
    let mut future=view("reference",2);
    future.nodes.push(GraphNode {key:"secret".into(),entity_type:"character".into(),
        name:"未揭晓凶手".into(),aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_original".into()});
    assert_eq!(actual_diff(&view("actual",1),&view("reference",1)),0.0);
    assert_eq!(actual_diff(&view("actual",1),&future),1.0);
    assert!(view("actual",1).nodes.is_empty());
}

#[test]
fn missing_stages_are_not_classified_as_complete_cached_prose() {
    assert_eq!(resume_stage(false,false),ResumeStage::Extract);
    assert_eq!(resume_stage(true,false),ResumeStage::Reconcile);
    assert_eq!(resume_stage(true,true),ResumeStage::Complete);
}

#[test]
fn stale_completion_is_rejected_even_when_text_dependencies_match() {
    assert!(guard_line_version(7,8).is_err());
    assert!(guard_line_version(7,7).is_ok());
}

#[test]
fn same_actual_revision_difference_is_set_based_not_record_count_based() {
    let mut actual=view("actual",2);
    actual.nodes.push(GraphNode {key:"x".into(),entity_type:"character".into(),name:"甲".into(),
        aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_generated".into()});
    let once=actual_diff(&actual,&view("reference",2));
    actual.nodes.push(actual.nodes[0].clone());
    assert_eq!(once,actual_diff(&actual,&view("reference",2)));
}

fn database() -> (rusqlite::Connection,i64) {
    use crate::store::{Db,books::{create_book_with_chapters,NewChapter}};
    let conn=rusqlite::Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap(); Db::new(&conn).migrate().unwrap();
    let chapters=(1..=3).map(|idx|NewChapter {idx,title:format!("章{idx}"),text:format!("原作第{idx}章未来剧情"),token_est:20}).collect::<Vec<_>>();
    let book=create_book_with_chapters(&conn,"生成测试",None,"txt","test",&chapters).unwrap();
    repo::mark_read(&conn,book,1).unwrap();
    (conn,book)
}

#[test]
fn selected_prose_never_uses_original_future_after_divergence() {
    use crate::core::kg::EditInput;
    let (conn,book)=database();
    let edit=repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,
        kind:"directive".into(),action:"upsert".into(),key:None,data:json!({"text":"改变下一章"})}).unwrap();
    let line=repo::line_version(&conn,book).unwrap();
    assert!(repo::selected_text(&conn,book,2).unwrap().is_none());
    assert!(saved_readable_after_stage_error(&conn,book,2,line).unwrap().is_none());
    let generated=repo::save_generated(&conn,book,2,"实际生成版","write","deps",line).unwrap();
    assert_eq!(repo::selected_text(&conn,book,2).unwrap().unwrap().id,generated.id);
    assert!(!repo::ready(&conn,generated.id).unwrap());
    assert_eq!(saved_readable_after_stage_error(&conn,book,2,line).unwrap().unwrap().content,"实际生成版");
    assert!(repo::generation_context(&conn,book,3).is_err());
    repo::revert(&conn,book,edit).unwrap();
    assert!(saved_readable_after_stage_error(&conn,book,2,line).is_err());
    assert_eq!(repo::selected_text(&conn,book,2).unwrap().unwrap().source,"original");
}

#[test]
fn stale_completion_cannot_replace_new_reading_line_selected_text() {
    use crate::core::kg::EditInput;
    let (conn,book)=database();
    let input=EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),
        action:"upsert".into(),key:None,data:json!({"text":"改写"})};
    repo::edit(&conn,&input).unwrap();
    let old=repo::line_version(&conn,book).unwrap();
    repo::edit(&conn,&input).unwrap();
    assert!(repo::save_generated(&conn,book,2,"旧任务正文","write","old",old).is_err());
    assert!(repo::selected_text(&conn,book,2).unwrap().is_none());
    assert!(saved_readable_after_stage_error(&conn,book,2,old).is_err());
}

#[test]
fn only_saved_stage_errors_allow_readable_prose_recovery() {
    assert!(completed_stage_error("[kg:prose-saved:extract] extraction failed"));
    assert!(completed_stage_error("[kg:prose-saved:reconcile] merge failed"));
    assert!(!completed_stage_error("正文生成失败"));
    assert!(!completed_stage_error("阅读线已改变"));
    assert!(!completed_stage_error("provider 未配置 write 模型"));
}

#[test]
fn impossible_dead_owner_and_location_preconditions_are_rejected() {
    let mut ctx=context();
    for (key,kind) in [("actor","character"),("item","item"),("owner","character"),("place","location")] {
        ctx.actual.nodes.push(GraphNode {key:key.into(),entity_type:kind.into(),name:key.into(),aliases:vec![],
            summary:None,chapter_idx:1,provenance:"ai_original".into()});
    }
    let event=GraphEvent {key:"event".into(),title:"行动".into(),chapter_idx:2,order:0,
        participants:vec!["actor".into()],location:None,summary:String::new(),outcome:String::new(),provenance:"ai_adapted".into()};
    for (predicate,subject,value,actual) in [("life_status","actor","alive","dead"),
        ("owner","item","actor","owner"),("location","actor","owner","place")] {
        let kind=if predicate=="life_status" {"scalar"} else {"entity_ref"};
        let nodes=ctx.actual.nodes.iter().cloned().map(|mut n|{n.chapter_idx=2;n.provenance="ai_adapted".into();n}).collect();
        let result=SpecResult {schema_version:"1.0".into(),chapter_idx:2,nodes,edges:vec![],events:vec![event.clone()],
            preconditions:vec![Precondition {key:"pre".into(),event_key:"event".into(),subject_key:subject.into(),predicate:predicate.into(),
                value:TypedValue {kind:kind.into(),value:Some(value.into())},assertion_kind:"confirmed".into(),evidence_refs:vec!["span".into()]}],unresolved:vec![]};
        let hard=json!({format!("{subject}:{predicate}"):{"kind":kind,"value":actual},
            "evidenced_actual_preconditions":[{"evidence_spans":[{"key":"span"}]}]});
        assert!(validate_spec(&result,&ctx,&hard).unwrap_err().contains(predicate));
    }
}

#[test]
fn referenced_presence_is_not_assumed_to_be_a_causal_requirement() {
    let mut ctx=context();
    ctx.spec.nodes.push(GraphNode {key:"actor".into(),entity_type:"character".into(),name:"甲".into(),
        aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_original".into()});
    let mut node=ctx.spec.nodes[0].clone();node.provenance="ai_adapted".into();
    let result=SpecResult {schema_version:"1.0".into(),chapter_idx:2,nodes:vec![node],edges:vec![],
        events:vec![GraphEvent {key:"event".into(),title:"出场".into(),chapter_idx:2,order:0,participants:vec!["actor".into()],
            location:None,summary:String::new(),outcome:String::new(),provenance:"ai_adapted".into()}],
        preconditions:vec![],unresolved:vec![]};
    assert!(validate_spec(&result,&ctx,&json!({})).is_ok());
}

#[test]
fn adapted_preconditions_survive_writer_budgeting_as_essential_context() {
    let hard=json!({"working_spec_preconditions":[{"subject_key":"actor","predicate":"life_status",
        "value":{"kind":"scalar","value":"dead"}}]});
    let messages=write_messages(&context(),&config(),&hard,&view("actual",1)).unwrap();
    assert!(messages[1].content.contains("working_spec_preconditions"));
    assert!(messages[1].content.contains("dead"));
}

#[test]
fn newly_adapted_spec_identity_keys_are_opaque_and_remap_every_reference() {
    let mut result=SpecResult {schema_version:"1.0".into(),chapter_idx:2,
        nodes:vec![GraphNode {key:"spec-未来秘密姓名".into(),entity_type:"character".into(),name:"未来秘密姓名".into(),
            aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_adapted".into()}],edges:vec![],
        events:vec![GraphEvent {key:"event".into(),title:"出现".into(),chapter_idx:2,order:0,
            participants:vec!["spec-未来秘密姓名".into()],location:None,summary:String::new(),outcome:String::new(),provenance:"ai_adapted".into()}],
        preconditions:vec![],unresolved:vec![]};
    normalize_spec_keys(&mut result,&context(),"dependency");
    assert!(result.nodes[0].key.starts_with("spec-"));
    assert!(!result.nodes[0].key.contains("秘密"));
    assert_eq!(result.events[0].participants[0],result.nodes[0].key);
}

struct RecordedClient {
    writes:std::sync::atomic::AtomicUsize,
    fail_generated_b:std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl LlmClient for RecordedClient {
    async fn structured(&self,req:StructuredRequest<'_>)->Result<Value,crate::llm::LlmError> {
        let input:Value=serde_json::from_str(&req.messages.last().unwrap().content).unwrap();
        let idx=input["chapter_idx"].as_i64().unwrap();
        match req.schema_name {
            "kg_stage_a"=>Ok(json!({"schema_version":"kg-stage-a/v1","chapter_idx":idx,"summary":"实际章节摘要",
                "open_threads":[],"mentions":[],"events":[]})),
            "kg_stage_b"=> {
                if input["source"]=="generated" && self.fail_generated_b.swap(false,std::sync::atomic::Ordering::SeqCst) {
                    return Err(crate::llm::LlmError::Network("recorded back-extraction outage".into()));
                }
                Ok(json!({"schema_version":"kg-stage-b/v1","chapter_idx":idx,"facts":[],"relations":[],
                    "changes":[],"preconditions":[],"unresolved":[]}))
            },
            "kg_spec"=>Ok(json!({"schema_version":"1.0","chapter_idx":idx,"nodes":[],"edges":[],"events":[],
                "preconditions":[],"unresolved":[]})),
            other=>Err(crate::llm::LlmError::Schema(format!("unexpected recorded request {other}")))
        }
    }
    async fn stream_text(&self,_req:TextRequest,on_delta:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,crate::llm::LlmError> {
        self.writes.fetch_add(1,std::sync::atomic::Ordering::SeqCst);
        on_delta("真实保存的生成文本");Ok("真实保存的生成文本".into())
    }
}

fn provider()->ProviderConfig {
    ProviderConfig {id:1,name:"recorded".into(),protocol:"openai".into(),base_url:"http://recorded".into(),
        model_extract:Some("extract".into()),model_write:Some("write".into()),model_merge:Some("merge".into()),api_key:String::new()}
}

#[tokio::test]
async fn backextract_failure_preserves_exact_text_and_retry_runs_missing_stage_without_rewriting() {
    use crate::core::kg::EditInput;
    let (conn,book)=database();
    repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),
        action:"upsert".into(),key:None,data:json!({"text":"改变剧情"})}).unwrap();
    let state=AppState {db:parking_lot::Mutex::new(Some(conn))};
    let client=RecordedClient {writes:std::sync::atomic::AtomicUsize::new(0),fail_generated_b:std::sync::atomic::AtomicBool::new(true)};
    let error=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap_err();
    assert!(completed_stage_error(&error));
    let saved=db(&state,|c|repo::selected_text(c,book,2)).unwrap().unwrap();
    assert_eq!(saved.content,"真实保存的生成文本");
    assert!(!db(&state,|c|repo::ready(c,saved.id)).unwrap());
    let retried=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_eq!(retried.id,saved.id);
    assert!(db(&state,|c|repo::ready(c,retried.id)).unwrap());
    assert_eq!(client.writes.load(std::sync::atomic::Ordering::SeqCst),1);
    assert!(db(&state,|c|repo::internal_view(c,&query(book,"actual",2,"global"))).unwrap().nodes.is_empty());
}

#[tokio::test]
async fn reverting_temporary_intent_revives_matching_text_and_its_extraction() {
    use crate::core::kg::EditInput;
    let (conn,book)=database();
    let intent=EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),
        action:"upsert".into(),key:None,data:json!({"text":"保留信使"})};
    repo::edit(&conn,&intent).unwrap();
    let state=AppState {db:parking_lot::Mutex::new(Some(conn))};
    let client=RecordedClient {writes:std::sync::atomic::AtomicUsize::new(0),fail_generated_b:std::sync::atomic::AtomicBool::new(false)};
    let first=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    let temporary=db(&state,|c|repo::edit(c,&EditInput {data:json!({"text":"改变运送路线"}),..intent.clone()})).unwrap();
    let changed=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_ne!(first.id,changed.id);
    db(&state,|c|repo::revert(c,book,temporary)).unwrap();
    let revived=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_eq!(first.id,revived.id);
    assert!(db(&state,|c|repo::ready(c,revived.id)).unwrap());
    assert_eq!(client.writes.load(std::sync::atomic::Ordering::SeqCst),2);
}

#[test]
fn actual_state_drift_includes_death_owner_and_location_not_just_visible_node_labels() {
    use crate::core::kg::{Fact,StageA,StageB};
    let make=|value:&str|ChapterBundle {stage_a:StageA {schema_version:"kg-stage-a/v1".into(),chapter_idx:2,
        summary:String::new(),open_threads:vec![],mentions:vec![],events:vec![]},
        stage_b:StageB {schema_version:"kg-stage-b/v1".into(),chapter_idx:2,facts:vec![Fact {key:"f".into(),
            subject_key:"actor".into(),predicate:"life_status".into(),value:TypedValue {kind:"scalar".into(),value:Some(value.into())},
            chapter_idx:2,observed_chapter:2,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),
            speaker_key:None,evidence_refs:vec!["span".into()]}],relations:vec![],changes:vec![],preconditions:vec![],unresolved:vec![]},
        identities:vec![],spans:vec![],identity_links:vec![]};
    assert_eq!(bundle_state_diff(&make("alive"),&make("alive"),2),0.0);
    assert_eq!(bundle_state_diff(&make("dead"),&make("alive"),2),1.0);
    let mut hearsay=make("dead");hearsay.stage_b.facts[0].assertion_kind="reported".into();
    assert_eq!(bundle_state_diff(&hearsay,&make("alive"),2),1.0);
}

#[test]
fn deleted_future_entities_and_events_are_removed_from_reference_not_actual_history() {
    let mut reference=view("spec",2);
    reference.nodes.push(GraphNode {key:"excluded".into(),entity_type:"character".into(),name:"甲".into(),
        aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_original".into()});
    reference.nodes.push(GraphNode {key:"kept".into(),entity_type:"character".into(),name:"乙".into(),
        aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_original".into()});
    reference.events.push(GraphEvent {key:"event".into(),title:"送信".into(),chapter_idx:2,order:0,
        participants:vec!["excluded".into(),"kept".into()],location:None,summary:"甲送信给乙".into(),outcome:String::new(),provenance:"ai_original".into()});
    let actual=reference.clone();
    let intent=json!({"tombstones":[{"kind":"node","key":"excluded","scope":"global","effective_from":2}],"directives":[]});
    apply_tombstones(&mut reference,&intent);
    assert_eq!(reference.nodes.len(),1);
    assert!(reference.events.is_empty());
    assert_eq!(actual.nodes.len(),2);
    assert_eq!(actual.events.len(),1);
}

#[test]
fn delete_and_negative_directive_intents_are_essential_budgeted_prompt_and_digest_dependencies() {
    let intent=json!({"tombstones":[{"kind":"event","key":"forbidden","scope":"chapter","effective_from":2}],
        "directives":[{"polarity":"negative","text":"信使死亡"}]});
    let hard=json!({"intent_constraints":intent});
    let messages=write_messages(&context(),&config(),&hard,&view("actual",1)).unwrap();
    assert!(messages[1].content.contains("forbidden"));
    assert!(messages[1].content.contains("negative"));
    assert_ne!(dependency_digest(&context(),&config(),&hard).unwrap(),
        dependency_digest(&context(),&config(),&json!({})).unwrap());
    let result=SpecResult {schema_version:"1.0".into(),chapter_idx:2,nodes:vec![],edges:vec![],
        events:vec![GraphEvent {key:"forbidden".into(),title:"禁止事件".into(),chapter_idx:2,order:0,participants:vec![],
            location:None,summary:String::new(),outcome:String::new(),provenance:"ai_adapted".into()}],preconditions:vec![],unresolved:vec![]};
    assert!(validate_spec(&result,&context(),&hard).unwrap_err().contains("删除"));
}

#[test]
fn persisted_delete_and_negative_polarity_reach_trusted_generation_constraints() {
    use crate::core::kg::EditInput;
    let (conn,book)=database();
    repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),
        action:"upsert".into(),key:None,data:json!({"name":"不应重现的角色"})}).unwrap();
    let public=repo::get_view(&conn,&query(book,"spec",2,"global")).unwrap().nodes[0].key.clone();
    let internal=repo::internal_view(&conn,&query(book,"spec",2,"global")).unwrap().nodes[0].key.clone();
    repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),
        action:"delete".into(),key:Some(public),data:json!({})}).unwrap();
    repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),
        action:"upsert".into(),key:None,data:json!({"text":"信使死亡","polarity":"negative"})}).unwrap();
    let intent=repo::intent_constraints(&conn,book,2).unwrap();
    assert!(tombstone_keys(&intent,"node").contains(internal.as_str()));
    assert!(intent["directives"].as_array().unwrap().iter().any(|d|d["polarity"]=="negative" && d["text"]=="信使死亡"));
    let prompt=write_messages(&context(),&config(),&json!({"intent_constraints":intent}),&view("actual",1)).unwrap();
    assert!(prompt[1].content.contains(&internal));
    assert!(prompt[1].content.contains("negative"));
}

#[test]
fn renamed_spec_keys_cannot_reintroduce_a_deleted_event_or_entity_snapshot() {
    let mut ctx=context();
    ctx.spec.nodes.push(GraphNode {key:"deleted".into(),entity_type:"character".into(),name:"排除角色".into(),
        aliases:vec!["排除别名".into()],summary:None,chapter_idx:2,provenance:"ai_original".into()});
    let mut intent=json!({"tombstones":[{"kind":"node","key":"deleted"}],"directives":[]});
    enrich_tombstones(&mut intent,&ctx.spec,&ctx.actual);
    apply_tombstones(&mut ctx.spec,&intent);
    let result=SpecResult {schema_version:"1.0".into(),chapter_idx:2,nodes:vec![GraphNode {key:"spec-renamed".into(),
        entity_type:"character".into(),name:"排除别名".into(),aliases:vec![],summary:None,chapter_idx:2,provenance:"ai_adapted".into()}],
        edges:vec![],events:vec![],preconditions:vec![],unresolved:vec![]};
    assert!(validate_spec(&result,&ctx,&json!({"intent_constraints":intent})).unwrap_err().contains("删除"));
}
