use super::*;
use crate::core::quick_read::{ChapterQuickRead, QuickReadCharacterCard, QuickReadCharacterMention, QuickReadReference, QuickReadSlice, SourceSpan};
use crate::store::{books::{create_book_with_chapters, NewChapter}, Db};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn config() -> RuntimeConfig {
    RuntimeConfig { provider_id:1, model_extract:"extract".into(), context_limit:65536, output_reserve:4096, prompt_version:"kg-lite/v1".into() }
}
fn provider() -> ProviderConfig {
    ProviderConfig { id:1, name:"recorded".into(), protocol:"openai".into(), base_url:"http://recorded".into(), model_extract:Some("extract".into()), model_write:Some("write".into()), model_merge:Some("merge".into()), api_key:String::new() }
}
fn database() -> (Connection, i64) {
    let conn=Connection::open_in_memory().unwrap(); Db::apply_pragmas(&conn).unwrap(); Db::new(&conn).migrate().unwrap();
    conn.execute("INSERT INTO llm_providers(id,name,protocol,base_url,key_ref,model_extract,model_write,model_merge,params_json) VALUES (1,'recorded','openai','http://recorded','unused','extract','write','merge',?1)",[json!({"context_limit":65536,"output_reserve":4096}).to_string()]).unwrap();
    crate::creds::save("unused","test-key").unwrap();
    let chapters=(1..=3).map(|idx|NewChapter { idx, title:format!("章{idx}"), text:format!("原作第{idx}章完整正文：顾岚守城。"), token_est:30 }).collect::<Vec<_>>();
    let book=create_book_with_chapters(&conn,"paired",None,"txt","paired",&chapters).unwrap();
    repo::mark_read(&conn,book,1).unwrap(); (conn,book)
}
fn bootstrap(conn:&Connection,book:i64) {
    use crate::store::quick_read as quick;
    let sources=quick::source_chapters(conn,book).unwrap();
    let spans=sources.iter().map(|source|vec![SourceSpan { chapter_idx:source.chapter_idx, start_char:0, end_char:source.char_count, content_hash:source.content_hash.clone() }]).collect::<Vec<_>>();
    let revision=quick::prepare_revision(conn,book,&sources,"reference",1,"extract","v1","v1",&spans).unwrap();
    let mut slices=Vec::new();
    for (index,source) in sources.iter().enumerate() {
        let slice=QuickReadSlice { slice_idx:index, source_spans:spans[index].clone(), chapter_summaries:vec![ChapterQuickRead { chapter_idx:source.chapter_idx, summary:format!("守城{index}"), character_refs:vec![format!("s{index}:hero")] }], characters:vec![QuickReadCharacterMention { local_id:format!("s{index}:hero"), name:"顾岚".into(), aliases:vec![], chapter_idx:source.chapter_idx, evidence:"守城".into() }], plot_notes:vec![] };
        quick::save_slice(conn,revision,&slice,(10,10)).unwrap(); slices.push(slice);
    }
    let reference=QuickReadReference { schema_version:"v1".into(), source_digest:quick::source_digest(&sources), source_kind:crate::core::quick_read::QuickReadSourceKind::LocalFullText, source_provenance:vec![], provider_id:1, model:"extract".into(), prompt_version:"v1".into(), chapter_summaries:crate::core::quick_read::canonical_chapter_summaries(&slices), characters:vec![QuickReadCharacterCard { id:"hero".into(), name:"顾岚".into(), aliases:vec![], identity_evidence:vec!["守城".into()], motivations:vec!["保护边城".into()], narrative_roles:vec!["主角".into()], first_appearance_chapter:1, revelations:vec![], source_chapters:vec![1,2,3] }], plot_summary:"守城主线".into(), plot_notes:vec![], completed_spans:slices.iter().flat_map(|slice|slice.source_spans.clone()).collect() };
    quick::publish_reference(conn,revision,&reference).unwrap();
    for chapter in 1..=2 {
        let text=repo::text_version(conn,book,chapter,"original",None).unwrap();
        let snapshot=crate::core::kg::ChapterSnapshot { summary:format!("原作记忆{chapter}"), ..Default::default() };
        let (_,extraction_config)=crate::commands::kg::runtime_config(conn,None).unwrap();
        repo::commit_snapshot(conn,&text,&snapshot,"bootstrap",repo::line_version(conn,book).unwrap(),None,&extraction_config).unwrap();
        crate::store::story_projection::save_chapter_memory(conn,&text,&snapshot,"bootstrap").unwrap();
    }
}
fn plan(conn:&Connection,book:i64,chapter:i64) {
    let wiki=NextChapterWiki { chapter_idx:chapter, goals:vec!["继续守城".into()], state_directions:vec![], open_threads:vec![], avoid:vec![], ending_direction:"留下线索".into(), related_entry_ids:vec![] };
    crate::commands::story::save_test_next_wiki(conn,book,chapter,&wiki).unwrap();
}
fn pair(chapter:i64) -> Value {
    json!({"prose":format!("改写第{chapter}章完整正文：顾岚主动安排撤离，保护边城百姓。"),"actual_chapter_wiki":{"chapter_idx":chapter,"chapter":{"headline":"安排撤离","narrative":"顾岚主动安排撤离。","ending":{"open_threads":[]}},"entries":[{"id":"hero","kind":"character","name":"顾岚","aliases":[],"summary":"守护边城","state_items":["安排撤离"],"related_entries":[],"open_threads":[],"source_chapters":[chapter]}]}})
}
struct PairedClient {
    calls:AtomicUsize,
    prompts:parking_lot::Mutex<Vec<Value>>,
    malformed:AtomicBool,
    cancelled:AtomicBool,
    cancel_after_response:AtomicBool,
}
impl PairedClient {
    fn new()->Self { Self { calls:AtomicUsize::new(0), prompts:parking_lot::Mutex::new(vec![]), malformed:AtomicBool::new(false), cancelled:AtomicBool::new(false), cancel_after_response:AtomicBool::new(false) } }
}
#[async_trait::async_trait]
impl LlmClient for PairedClient {
    async fn structured_with_usage(&self,request:StructuredRequest<'_>)->Result<crate::llm::StructuredResponse,crate::llm::LlmError> {
        assert_eq!(request.task,TaskKind::Write);
        let input:Value=serde_json::from_str(&request.messages[1].content).unwrap();
        let chapter=input["chapter_idx"].as_i64().unwrap();
        self.prompts.lock().push(input); self.calls.fetch_add(1,Ordering::SeqCst);
        let mut value=pair(chapter);
        if self.malformed.swap(false,Ordering::SeqCst) { value["actual_chapter_wiki"]["chapter_idx"]=json!(chapter+1); }
        if self.cancel_after_response.load(Ordering::SeqCst) { self.cancelled.store(true,Ordering::SeqCst); }
        Ok(crate::llm::StructuredResponse { value, usage:Some((100,100)) })
    }
    fn check_cancelled(&self)->Result<(),crate::llm::LlmError> {
        if self.cancelled.load(Ordering::SeqCst) { Err(crate::llm::LlmError::Cancelled) } else { Ok(()) }
    }
    async fn stream_text(&self,_request:crate::llm::TextRequest,_delta:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,crate::llm::LlmError> { panic!("paired rewrite cannot stream prose") }
}

#[tokio::test]
async fn paired_generation_requires_quick_read_and_current_original_bootstrap() {
    let (conn,book)=database();
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    assert!(ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.is_err());
    assert_eq!(client.calls.load(Ordering::SeqCst),0);
    db(&state,|conn| { bootstrap(conn,book); plan(conn,book,2); Ok(()) }).unwrap();
    let mut changed=config(); changed.model_extract="changed-extractor".into();
    assert!(ensure_rewritten_chapter(&state,&client,&provider(),&changed,book,2,None).await.is_err());
    assert_eq!(client.calls.load(Ordering::SeqCst),0);
    let generated=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    db(&state,|conn| {
        assert_eq!(repo::selected_text(conn,book,2)?.unwrap().id,generated.id);
        assert!(crate::store::story_projection::text_memory_ready(conn,generated.id)?);
        assert_eq!(repo::progress(conn,book)?,1);
        assert!(crate::store::story_projection::active_chapter_memory(conn,book,2)?.is_none());
        repo::mark_read(conn,book,2)?;
        let actual=crate::store::story_projection::active_chapter_memory(conn,book,2)?.unwrap();
        assert_eq!(actual["text_version_id"],generated.id);
        assert_eq!(actual["chapter"]["narrative"],"顾岚主动安排撤离。");
        Ok(())
    }).unwrap();
}

#[tokio::test]
async fn paired_generation_uses_selected_generated_predecessor_and_hidden_original_target() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    let chapter2=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    db(&state,|conn| { repo::mark_read(conn,book,2)?; plan(conn,book,3); Ok(()) }).unwrap();
    let chapter3=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,3,None).await.unwrap();
    let prompts=client.prompts.lock();
    assert_eq!(prompts[0]["original_target_wiki"]["chapter_idx"],2);
    assert_eq!(prompts[1]["previous_selected_full_text"],chapter2.content);
    assert!(prompts[1]["original_target_chapter"].as_str().unwrap().contains("原作第3章完整正文"));
    assert_eq!(prompts[1]["actual_history"]["entries"][0]["state_items"][0],"安排撤离");
    assert_eq!(prompts[1]["quick_read"]["characters"][0]["name"],"顾岚");
    assert_eq!(chapter3.source,"generated");
}

#[tokio::test]
async fn malformed_pair_does_not_publish_text_or_wiki_and_cache_needs_complete_pair() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new(); client.malformed.store(true,Ordering::SeqCst);
    assert!(ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.is_err());
    db(&state,|conn| { assert_eq!(repo::selected_text(conn,book,2)?.unwrap().source,"original"); Ok(()) }).unwrap();
    let saved=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    let cached=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    assert_eq!(saved.id,cached.id); assert_eq!(client.calls.load(Ordering::SeqCst),2);
    db(&state,|conn| { conn.execute("UPDATE story_chapter_revisions SET active=0 WHERE text_version_id=?1",[saved.id]).map_err(|e|e.to_string())?; Ok(()) }).unwrap();
    ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    assert_eq!(client.calls.load(Ordering::SeqCst),3);
}

#[tokio::test]
async fn free_text_without_question_batch_reaches_writer_and_invalidates_cached_pair() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    db(&state,|conn| { story_repo::save_intervention(conn,&crate::core::story::InterventionInput { book_id:book, chapter_idx:2, batch_index:None, question_batch_id:None, selected_option_ids:vec![], free_text:Some("顾岚带百姓离城，不再守城".into()), overridden_option_ids:vec![], reading_line_version:repo::line_version(conn,book)? })?; Ok(()) }).unwrap();
    db(&state,|conn| { assert!(repo::active_generated_text(conn,book,2)?.is_none(),"edited intervention must invalidate the previous active rewrite"); Ok(()) }).unwrap();
    db(&state,|conn| { plan(conn,book,2); Ok(()) }).unwrap();
    ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,None).await.unwrap();
    assert_eq!(client.calls.load(Ordering::SeqCst),2);
    assert!(client.prompts.lock()[1]["next_chapter_wiki"]["user_free_text"].to_string().contains("顾岚带百姓离城"));
}

#[tokio::test]
async fn recovery_reuses_staged_complete_pair_without_provider_call() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let prepared=prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap();
    let job=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::Generate,json!({"chapter_idx":2,"paired_draft":{"dependency_digest":prepared.dependency_digest,"result":pair(2)}})).unwrap();
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    let generated=ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,Some(job)).await.unwrap();
    assert_eq!(client.calls.load(Ordering::SeqCst),0);
    db(&state,|conn| { let checkpoint=repo::job_checkpoint(conn,job)?; assert_eq!(checkpoint["paired_commit"]["text_version_id"],generated.id); assert!(crate::store::story_projection::text_memory_ready(conn,generated.id)?); Ok(()) }).unwrap();
}

#[tokio::test]
async fn cancelled_job_cannot_publish_a_staged_pair() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let prepared=prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap();
    let job=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::Generate,json!({"chapter_idx":2,"paired_draft":{"dependency_digest":prepared.dependency_digest,"result":pair(2)}})).unwrap();
    crate::jobs::mark_cancelled(&conn,job).unwrap();
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    assert!(ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,Some(job)).await.is_err());
    assert_eq!(client.calls.load(Ordering::SeqCst),0);
    db(&state,|conn| { assert_eq!(repo::selected_text(conn,book,2)?.unwrap().source,"original"); Ok(()) }).unwrap();
}

#[tokio::test]
async fn cancellation_after_provider_response_preserves_original_selection() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let job=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::Generate,json!({"chapter_idx":2})).unwrap();
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    client.cancel_after_response.store(true,Ordering::SeqCst);
    assert!(ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,Some(job)).await.is_err());
    assert_eq!(client.calls.load(Ordering::SeqCst),1);
    db(&state,|conn| { assert_eq!(repo::selected_text(conn,book,2)?.unwrap().source,"original"); assert!(repo::job_checkpoint(conn,job)?["paired_commit"].is_null()); Ok(()) }).unwrap();
}

#[tokio::test]
async fn explicit_retry_reuses_failed_job_complete_draft() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let prepared=prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap();
    let failed=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::Generate,json!({"chapter_idx":2,"paired_draft":{"dependency_digest":prepared.dependency_digest,"result":pair(2)}})).unwrap();
    crate::jobs::mark_failed(&conn,failed,"activation interrupted").unwrap();
    let retry=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::Generate,json!({"chapter_idx":2})).unwrap();
    let state=AppState { db:parking_lot::Mutex::new(Some(conn)) }; let client=PairedClient::new();
    ensure_rewritten_chapter(&state,&client,&provider(),&config(),book,2,Some(retry)).await.unwrap();
    assert_eq!(client.calls.load(Ordering::SeqCst),0);
}

#[test]
fn paired_digest_tracks_original_reference_revision_and_selected_previous_memory() {
    let (conn,book)=database(); bootstrap(&conn,book); plan(&conn,book,2);
    let baseline=prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap().dependency_digest;
    let original=repo::text_version(&conn,book,2,"original",None).unwrap();
    let snapshot=crate::core::kg::ChapterSnapshot { summary:"更新的隐藏原作记忆".into(), ..Default::default() };
    crate::store::story_projection::save_chapter_memory(&conn,&original,&snapshot,"reference-refresh").unwrap();
    assert!(prepare_rewrite(&conn,book,2,&provider(),&config()).is_err());
    plan(&conn,book,2);
    let refreshed=prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap().dependency_digest;
    assert_ne!(baseline,refreshed);
    let previous=repo::selected_text(&conn,book,1).unwrap().unwrap();
    crate::store::story_projection::save_chapter_memory(&conn,&previous,&crate::core::kg::ChapterSnapshot { summary:"更新的实际前章记忆".into(), ..Default::default() },"actual-refresh").unwrap();
    assert!(prepare_rewrite(&conn,book,2,&provider(),&config()).is_err());
    plan(&conn,book,2);
    assert_ne!(refreshed,prepare_rewrite(&conn,book,2,&provider(),&config()).unwrap().dependency_digest);
}

#[test]
fn paired_decoder_qualifies_links_against_selected_actual_history_only() {
    let mut value=pair(3);
    value["actual_chapter_wiki"]["entries"][0]["related_entries"]=json!(["prior-selected-character"]);
    assert!(decode_rewrite_result(value.clone(),3).is_err());
    let selected=BTreeSet::from(["prior-selected-character".to_owned()]);
    assert!(decode_rewrite_result_with_history(value.clone(),3,&selected).is_ok());
    value["actual_chapter_wiki"]["entries"][0]["related_entries"]=json!(["hidden-original-secret"]);
    assert!(decode_rewrite_result_with_history(value,3,&selected).is_err());
}
