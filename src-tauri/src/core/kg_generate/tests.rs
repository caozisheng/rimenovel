use super::*;
use crate::core::kg::{empty_view, EditInput, GraphEvent};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

fn view(purpose:&str,chapter:i64)->GraphView {
    GraphView {chapter_idx:Some(chapter),reader_progress:chapter,ready:true,reading_line_version:1,..empty_view(purpose)}
}
fn node(key:&str,name:&str,chapter:i64,provenance:&str)->GraphNode {
    GraphNode {key:key.into(),entity_type:"character".into(),name:name.into(),aliases:vec![],summary:None,
        life_status:None,location:None,owner:None,chapter_idx:chapter,provenance:provenance.into()}
}
fn event(key:&str,participant:&str)->GraphEvent {
    GraphEvent {key:key.into(),title:"送信".into(),chapter_idx:2,order:0,participants:vec![participant.into()],
        location:None,time:Some("清晨".into()),summary:"送达消息".into(),provenance:"ai_adapted".into()}
}
fn context()->GenerationContext {
    GenerationContext {book_id:1,chapter_idx:2,reading_line_version:1,digest:String::new(),actual:view("actual",1),
        spec:view("spec",2),previous_text:"已发生的选中正文".into(),reference_text:Some("原作未来只是参考".into()),
        style_guide:None,directives:vec!["不要伤害信使".into()],drift_pref:0.3}
}
fn config()->RuntimeConfig {
    RuntimeConfig {provider_id:1,model_extract:"extract".into(),context_limit:32768,output_reserve:4096,prompt_version:"kg-lite/v1".into()}
}
fn database()->(Connection,i64) {
    use crate::store::{Db,books::{create_book_with_chapters,NewChapter}};
    let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
    let chapters=(1..=5).map(|idx|NewChapter {idx,title:format!("章{idx}"),text:format!("原作第{idx}章"),token_est:20}).collect::<Vec<_>>();
    let book=create_book_with_chapters(&conn,"生成测试",None,"txt","test",&chapters).unwrap();
    repo::mark_read(&conn,book,1).unwrap();(conn,book)
}
fn directive(book:i64,text:&str)->EditInput {
    EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:json!({"text":text})}
}
fn provider()->ProviderConfig {
    ProviderConfig {id:1,name:"recorded".into(),protocol:"openai".into(),base_url:"http://recorded".into(),
        model_extract:Some("extract".into()),model_write:Some("write".into()),model_merge:Some("merge".into()),api_key:String::new()}
}
// Fixed, independent chapter replies exercise persistence/resume, not prompt echo behavior.
struct RecordedClient {writes:AtomicUsize,extracts:AtomicUsize,merges:AtomicUsize,fail_extract:AtomicBool,fail_merge:AtomicBool}
impl RecordedClient {
    fn new()->Self {Self {writes:AtomicUsize::new(0),extracts:AtomicUsize::new(0),merges:AtomicUsize::new(0),fail_extract:AtomicBool::new(false),fail_merge:AtomicBool::new(false)}}
}
#[async_trait::async_trait]
impl LlmClient for RecordedClient {
    async fn structured(&self,req:StructuredRequest<'_>)->Result<Value,crate::llm::LlmError> {
        match req.task {
            TaskKind::Extract=>{self.extracts.fetch_add(1,Ordering::SeqCst);if self.fail_extract.swap(false,Ordering::SeqCst){return Err(crate::llm::LlmError::Network("extraction outage".into()));}},
            TaskKind::Merge=>{self.merges.fetch_add(1,Ordering::SeqCst);if self.writes.load(Ordering::SeqCst)>0&&self.fail_merge.swap(false,Ordering::SeqCst){return Err(crate::llm::LlmError::Network("reconciliation outage".into()));}},
            _=>return Err(crate::llm::LlmError::Schema("unexpected structured task".into()))
        }
        Ok(json!({"summary":"已完成章节摘要","entities":[],"relations":[],"events":[],"open_threads":[]}))
    }
    async fn structured_with_usage(&self,req:StructuredRequest<'_>)->Result<crate::llm::StructuredResponse,crate::llm::LlmError> {
        Ok(crate::llm::StructuredResponse {value:self.structured(req).await?,usage:None})
    }
    async fn stream_text(&self,_req:TextRequest,on_delta:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,crate::llm::LlmError> {
        let number=self.writes.fetch_add(1,Ordering::SeqCst)+1;let text=format!("实际生成正文{number}");on_delta(&text);Ok(text)
    }
}

#[test]
fn revert_digest_ignores_runtime_counters_but_tracks_real_state_and_prose() {
    let original=context();let mut revived=original.clone();revived.reading_line_version=9;
    revived.actual.reading_line_version=9;revived.actual.revision=Some(88);revived.actual.ready=false;
    revived.actual.coverage.calls_used=45;revived.spec.coverage.elapsed_ms=480000;
    let baseline=dependency_digest(&original,&config(),&json!({})).unwrap();
    assert_eq!(baseline,dependency_digest(&revived,&config(),&json!({})).unwrap());
    revived.previous_text.push_str("内容改变");assert_ne!(baseline,dependency_digest(&revived,&config(),&json!({})).unwrap());
    revived=original.clone();revived.actual.nodes.push(node("a","甲",1,"ai_original"));
    assert_ne!(baseline,dependency_digest(&revived,&config(),&json!({})).unwrap());
}
#[test]
fn essential_actual_prose_and_negative_intent_survive_reference_budgeting() {
    let mut ctx=context();ctx.reference_text=Some("未来秘密".repeat(10000));
    let hard=json!({"intent_constraints":{"tombstones":[{"kind":"event","key":"forbidden"}],"directives":[{"polarity":"negative","text":"信使死亡"}]}});
    let messages=write_messages(&ctx,&config(),&hard,&view("actual",1)).unwrap();let prompt=&messages[1].content;
    assert!(prompt.contains("已发生的选中正文")&&prompt.contains("negative")&&prompt.contains("forbidden"));
    assert!(!prompt.contains(&"未来秘密".repeat(10000)));
    let mut tiny=config();tiny.context_limit=5;assert!(write_messages(&ctx,&tiny,&hard,&view("actual",1)).is_err());
    assert_ne!(dependency_digest(&ctx,&config(),&hard).unwrap(),dependency_digest(&ctx,&config(),&json!({})).unwrap());
}
#[test]
fn future_tombstones_do_not_delete_actual_history_or_leave_dangling_state() {
    let mut spec=view("spec",2);spec.nodes.push(node("excluded","甲",2,"ai_original"));
    let mut item=node("item","信件",2,"ai_original");item.entity_type="item".into();item.owner=Some("excluded".into());
    spec.nodes.push(item);spec.events.push(event("delivery","excluded"));let actual=spec.clone();
    apply_tombstones(&mut spec,&json!({"tombstones":[{"kind":"node","key":"excluded"}]}));
    assert_eq!(spec.nodes.len(),1);assert!(spec.nodes[0].owner.is_none());assert!(spec.events.is_empty());
    assert_eq!(actual.nodes.len(),2);assert_eq!(actual.events.len(),1);
}
#[test]
fn deleted_alias_cannot_return_under_a_new_future_key() {
    let mut ctx=context();let mut deleted=node("deleted","甲",2,"ai_original");deleted.aliases.push("别名".into());ctx.spec.nodes.push(deleted);
    let mut intent=json!({"tombstones":[{"kind":"node","key":"deleted"}]});enrich_tombstones(&mut intent,&ctx.spec,&ctx.actual);
    apply_tombstones(&mut ctx.spec,&intent);
    let result=ChapterSnapshot {nodes:vec![node("spec-renamed-opaque-id","别名",2,"ai_adapted")],..ChapterSnapshot::default()};
    assert!(validate_spec(&result,&ctx,&json!({"intent_constraints":intent})).unwrap_err().contains("删除"));
}
#[test]
fn user_future_state_overrides_model_including_explicit_null() {
    let mut adapted=view("spec",2);let mut proposed=node("a","模型名字",2,"ai_adapted");proposed.location=Some("place".into());adapted.nodes.push(proposed);
    let mut input=view("spec",2);input.nodes.push(node("a","用户名字",2,"user"));overlay_user(&mut adapted,&input);
    assert_eq!(adapted.nodes[0].name,"用户名字");assert_eq!(adapted.nodes[0].location,None);assert_eq!(adapted.nodes[0].provenance,"user");
}
#[test]
fn reconciliation_uses_short_refs_and_opaque_future_only_ids() {
    let mut ctx=context();ctx.actual.nodes.push(node("actual-secret-key","甲",1,"ai_original"));
    let known=reconcile_registry(&ctx);let mut messages=write_messages(&ctx,&config(),&json!({}),&view("actual",1)).unwrap();
    prepare_merge_messages(&mut messages,&known);let input:Value=serde_json::from_str(&messages[1].content).unwrap();
    assert_eq!(input["known_entities"][0]["ref"],"k0");assert_eq!(input["actual"]["nodes"][0]["key"],"k0");
    let text=TextVersion {id:0,book_id:1,chapter_idx:2,source:"spec".into(),content:String::new(),content_hash:"deps".into(),generated_id:None};
    let result=crate::core::kg_extract::decode_graph(json!({"summary":"规格","entities":[{"ref":"new","type":"item","name":"秘密信物","aliases":[],"description":"赠送给甲的信物","life_status":null,"location":null,"owner":"k0"}],
        "relations":[],"events":[{"title":"赠送","participants":["k0","new"],"location":null,"time":null,"summary":"交付信物"}],"open_threads":[]}),&text,&known,"ai_adapted").unwrap();
    let item=result.nodes.iter().find(|n|n.name=="秘密信物").unwrap();assert!(item.key.starts_with("spec-"));assert!(!item.key.contains("秘密"));
    assert_eq!(item.owner.as_deref(),Some("actual-secret-key"));assert!(result.events[0].participants.contains(&item.key));
}
#[test]
fn drift_compares_known_life_owner_location_relations_and_event_time() {
    let mut reference=view("reference",2);let mut hero=node("a","甲",2,"ai_original");hero.life_status=Some("alive".into());reference.nodes.push(hero);
    let mut actual=reference.clone();actual.nodes[0].life_status=Some("dead".into());assert!(actual_diff(&actual,&reference)>0.0);
    reference.nodes[0].entity_type="item".into();reference.nodes[0].life_status=None;actual=reference.clone();actual.nodes[0].owner=Some("b".into());assert!(actual_diff(&actual,&reference)>0.0);
    actual=reference.clone();actual.nodes[0].location=Some("p".into());assert!(actual_diff(&actual,&reference)>0.0);
    reference.events.push(event("e","a"));actual=reference.clone();actual.events[0].time=Some("夜晚".into());assert!(actual_diff(&actual,&reference)>0.0);
    actual=reference.clone();actual.nodes.push(actual.nodes[0].clone());assert_eq!(actual_diff(&actual,&reference),0.0);
}
#[test]
fn missing_stages_and_stale_readable_recovery_are_guarded() {
    assert_eq!(resume_stage(false,false),ResumeStage::Extract);assert_eq!(resume_stage(true,false),ResumeStage::Reconcile);assert_eq!(resume_stage(true,true),ResumeStage::Complete);
    assert!(completed_stage_error("[kg:prose-saved:extract] failed"));assert!(completed_stage_error("[kg:prose-saved:reconcile] failed"));assert!(!completed_stage_error("正文生成失败"));
    let (conn,book)=database();repo::edit(&conn,&directive(book,"改写")).unwrap();let line=repo::line_version(&conn,book).unwrap();
    let saved=repo::save_generated(&conn,book,2,"保存正文","write","deps",line).unwrap();
    assert_eq!(saved_readable_after_stage_error(&conn,book,2,line).unwrap().unwrap().id,saved.id);
    repo::edit(&conn,&directive(book,"另一意图")).unwrap();assert!(saved_readable_after_stage_error(&conn,book,2,line).is_err());
    assert!(repo::save_generated(&conn,book,2,"过时正文","write","old",line).is_err());
}
#[tokio::test]
async fn ready_story_plan_generates_selected_next_chapter() {
    let (conn,book)=database();
    let previous=repo::selected_text(&conn,book,1).unwrap().unwrap();
    let wiki=crate::core::story::NextChapterWiki { chapter_idx:2, goals:vec!["继续调查".into()], state_directions:vec![], open_threads:vec![], avoid:vec![], ending_direction:"留下线索".into(), related_entry_ids:vec![] };
    crate::store::story::save_next_wiki(&conn,book,2,previous.id,1,"plan",&wiki).unwrap();
    let state=AppState {db:parking_lot::Mutex::new(Some(conn))};let client=RecordedClient::new();
    let generated=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_eq!(generated.source,"generated");
    assert_eq!(client.writes.load(Ordering::SeqCst),1);
} 

#[tokio::test]
async fn original_jump_extracts_only_requested_chapter() {
    let (conn,book)=database();let state=AppState {db:parking_lot::Mutex::new(Some(conn))};let client=RecordedClient::new();
    let text=ensure_chapter(&state,&client,&provider(),&config(),book,5,Arc::new(|_|{})).await.unwrap();assert_eq!(text.source,"original");
    assert_eq!(client.extracts.load(Ordering::SeqCst),1);assert_eq!(client.writes.load(Ordering::SeqCst),0);
    let actual=db(&state,|c|repo::internal_view(c,&query(book,"actual",5,"global"))).unwrap();assert!(actual.coverage.missing_chapters.contains(&2));
}
#[tokio::test]
async fn backextract_failure_resumes_missing_stage_without_rewriting_text() {
    let (conn,book)=database();repo::edit(&conn,&directive(book,"改变剧情")).unwrap();let state=AppState {db:parking_lot::Mutex::new(Some(conn))};
    let client=RecordedClient::new();client.fail_extract.store(true,Ordering::SeqCst);
    let error=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap_err();assert!(completed_stage_error(&error));
    let saved=db(&state,|c|repo::selected_text(c,book,2)).unwrap().unwrap();assert!(!db(&state,|c|repo::ready(c,saved.id)).unwrap());
    let retried=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_eq!(retried.id,saved.id);assert_eq!(retried.content,saved.content);assert_eq!(client.writes.load(Ordering::SeqCst),1);
}
#[tokio::test]
async fn reconcile_failure_resumes_without_rewriting_or_reextracting() {
    let (conn,book)=database();repo::edit(&conn,&directive(book,"改变剧情")).unwrap();let state=AppState {db:parking_lot::Mutex::new(Some(conn))};
    let client=RecordedClient::new();client.fail_merge.store(true,Ordering::SeqCst);
    let error=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap_err();assert!(error.starts_with("[kg:prose-saved:reconcile]"));
    let saved=db(&state,|c|repo::selected_text(c,book,2)).unwrap().unwrap();assert!(db(&state,|c|repo::ready(c,saved.id)).unwrap());
    let retried=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();assert_eq!(saved.id,retried.id);
    assert_eq!(client.writes.load(Ordering::SeqCst),1);assert_eq!(client.extracts.load(Ordering::SeqCst),1);
}
#[tokio::test]
async fn generated_jump_builds_only_selected_generated_predecessors() {
    let (conn,book)=database();repo::edit(&conn,&directive(book,"改变剧情")).unwrap();let state=AppState {db:parking_lot::Mutex::new(Some(conn))};let client=RecordedClient::new();
    let generated=ensure_chapter(&state,&client,&provider(),&config(),book,4,Arc::new(|_|{})).await.unwrap();assert_eq!(generated.source,"generated");
    assert_eq!(client.writes.load(Ordering::SeqCst),3);assert_eq!(client.extracts.load(Ordering::SeqCst),3);
    db(&state,|c|{for idx in 2..=4 {assert_eq!(repo::selected_text(c,book,idx)?.unwrap().source,"generated");}Ok(())}).unwrap();
}
#[tokio::test]
async fn reverting_temporary_intent_revives_cached_text_and_snapshot() {
    let (conn,book)=database();repo::edit(&conn,&directive(book,"保留信使")).unwrap();let state=AppState {db:parking_lot::Mutex::new(Some(conn))};let client=RecordedClient::new();
    let first=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    let temporary=db(&state,|c|repo::edit(c,&directive(book,"改变路线"))).unwrap();
    let changed=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();assert_ne!(first.id,changed.id);
    db(&state,|c|repo::revert(c,book,temporary)).unwrap();let revived=ensure_chapter(&state,&client,&provider(),&config(),book,2,Arc::new(|_|{})).await.unwrap();
    assert_eq!(first.id,revived.id);assert!(db(&state,|c|repo::ready(c,revived.id)).unwrap());assert_eq!(client.writes.load(Ordering::SeqCst),2);
}

#[test]
fn imported_actual_endpoints_keep_history_metadata_in_future_specs() {
    let mut ctx=context();ctx.actual.nodes.push(node("actual","甲",1,"ai_original"));
    let snapshot=ChapterSnapshot {nodes:ctx.actual.nodes.clone(),events:vec![event("future","actual")],..ChapterSnapshot::default()};
    assert!(validate_spec(&snapshot,&ctx,&json!({})).is_ok());
    assert_eq!(snapshot.nodes[0].chapter_idx,1);assert_eq!(snapshot.nodes[0].provenance,"ai_original");
}
#[test]
fn deleted_event_content_cannot_return_under_a_different_record_key() {
    let mut ctx=context();ctx.spec.nodes.push(node("a","甲",2,"ai_original"));ctx.spec.events.push(event("deleted","a"));
    let mut intent=json!({"tombstones":[{"kind":"event","key":"deleted"}]});enrich_tombstones(&mut intent,&ctx.spec,&ctx.actual);
    let mut restored=ctx.spec.events[0].clone();restored.key="e-new-model-key".into();
    let snapshot=ChapterSnapshot {nodes:ctx.spec.nodes.clone(),events:vec![restored],..ChapterSnapshot::default()};
    assert!(validate_spec(&snapshot,&ctx,&json!({"intent_constraints":intent})).unwrap_err().contains("删除"));
}
#[test]
fn sparse_original_dependencies_do_not_require_past_graphs_or_fallback_after_divergence() {
    let (conn,book)=database();assert!(actual_dependencies(&conn,book,4).is_ok());
    repo::edit(&conn,&directive(book,"改写")).unwrap();
    assert!(actual_dependencies(&conn,book,2).unwrap_err().contains("不能使用原作替代"));
    assert_eq!(divergence_from_edits(&[EditRecord {id:99,target_type:"directive".into(),target_id:"x".into(),action:"upsert".into(),after_json:None,effective_from:1,active:true}]),Some(2));
}
#[test]
fn persisted_negative_polarity_and_deleted_user_node_reach_generation_intent() {
    let (conn,book)=database();repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,
        kind:"node".into(),action:"upsert".into(),key:None,data:json!({"name":"不应重现的角色"})}).unwrap();
    let public=repo::get_view(&conn,&query(book,"spec",2,"global")).unwrap().nodes[0].key.clone();
    let internal=repo::internal_view(&conn,&query(book,"spec",2,"global")).unwrap().nodes[0].key.clone();
    repo::edit(&conn,&EditInput {book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),action:"delete".into(),key:Some(public),data:json!({})}).unwrap();
    repo::edit(&conn,&EditInput {data:json!({"text":"信使死亡","polarity":"negative"}),..directive(book,"信使死亡")}).unwrap();
    let intent=repo::intent_constraints(&conn,book,2).unwrap();assert!(tombstone_keys(&intent,"node").contains(internal.as_str()));
    assert!(intent["directives"].as_array().unwrap().iter().any(|d|d["polarity"]=="negative"&&d["text"]=="信使死亡"));
    let prompt=write_messages(&context(),&config(),&json!({"intent_constraints":intent}),&view("actual",1)).unwrap();
    assert!(prompt[1].content.contains(&internal));assert!(prompt[1].content.contains("negative"));
}
