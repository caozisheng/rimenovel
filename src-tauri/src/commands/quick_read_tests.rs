//! Offline worker acceptance: no credentials, external HTTP, or paid latency claims.
use super::*;
use crate::core::kg::RuntimeConfig;
use crate::llm::{LlmClient,LlmError,StructuredRequest,StructuredResponse,TextRequest};
use crate::store::{books::{create_book_with_chapters,NewChapter},Db};
use std::sync::{Arc,atomic::{AtomicUsize,Ordering}};
use wiremock::{matchers::{method,path},Mock,MockServer,ResponseTemplate};

struct OfflineClient {
 control:crate::llm::policy::CallControl,
 requests:parking_lot::Mutex<Vec<(String,Value)>>,
 fail_slice:Option<usize>,
 late_cancel:bool,
 barrier:Option<tokio::sync::Barrier>,
 active:AtomicUsize,
 peak:AtomicUsize,
}
impl OfflineClient {
 fn new()->Self {Self{control:Default::default(),requests:Default::default(),fail_slice:None,late_cancel:false,barrier:None,active:AtomicUsize::new(0),peak:AtomicUsize::new(0)}}
 fn slice_requests(&self)->Vec<Value>{self.requests.lock().iter().filter(|(name,_)|name=="quick_read_slice").map(|(_,value)|value.clone()).collect()}
}
#[async_trait::async_trait]
impl LlmClient for OfflineClient {
 async fn structured_with_usage(&self,request:StructuredRequest<'_>)->Result<StructuredResponse,LlmError>{
  let input:Value=serde_json::from_str(&request.messages[1].content).unwrap();
  self.requests.lock().push((request.schema_name.into(),input.clone()));
  if request.schema_name=="quick_read_summary" {
   let characters=input["slices"].as_array().unwrap().iter().flat_map(|slice|slice["characters"].as_array().unwrap()).map(|mention|json!({"member_local_ids":[mention["local_id"]],"id":mention["local_id"],"name":mention["name"],"aliases":[],"identity_evidence":["归并理由"],"motivations":[],"narrative_roles":[],"first_appearance_chapter":mention["chapter_idx"],"revelations":[],"source_chapters":[mention["chapter_idx"]]})).collect::<Vec<_>>();
   // Deliberately omit source notes and raw identity evidence: the host must retain them.
   return Ok(StructuredResponse{value:json!({"characters":characters,"plot_summary":"离线主线汇总","plot_notes":[]}),usage:Some((10,10))});
  }
  let idx=input["slice_idx"].as_u64().unwrap() as usize;
  let active=self.active.fetch_add(1,Ordering::SeqCst)+1;self.peak.fetch_max(active,Ordering::SeqCst);
  if let Some(barrier)=&self.barrier {barrier.wait().await;}
  self.active.fetch_sub(1,Ordering::SeqCst);
  if self.fail_slice==Some(idx) {return Err(LlmError::Server{status:504,body:"offline gateway timeout".into(),retry_after:None,request_id:None});}
  let chapters=input["chapters"].as_array().unwrap();
  let chapter_summaries=chapters.iter().map(|chapter|json!({"chapter_idx":chapter["chapter_idx"],"summary":format!("slice {idx} {}..{}",chapter["start_char"],chapter["end_char"]),"character_refs":[format!("c{}",chapter["chapter_idx"])]})).collect::<Vec<_>>();
  let characters=chapters.iter().map(|chapter|json!({"ref":format!("c{}",chapter["chapter_idx"]),"name":"阿七","aliases":["顾岚"],"chapter_idx":chapter["chapter_idx"],"evidence":format!("source evidence {idx} {}",chapter["start_char"])})).collect::<Vec<_>>();
  let plot_notes=chapters.iter().map(|chapter|json!({"summary":format!("source note {idx} {}",chapter["start_char"]),"source_chapters":[chapter["chapter_idx"]]})).collect::<Vec<_>>();
  if self.late_cancel {self.control.cancel();}
  Ok(StructuredResponse{value:json!({"chapter_summaries":chapter_summaries,"characters":characters,"plot_notes":plot_notes}),usage:Some((10,10))})
 }
 fn commit_guard(&self)->Result<Option<crate::llm::policy::CommitGuard<'_>>,LlmError>{self.control.commit_guard().map(Some)}
 async fn stream_text(&self,_:TextRequest,_:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,LlmError>{panic!("quick-read must not request prose")}
}
struct WebMatrixClient { verdict:&'static str, calls:AtomicUsize }
#[async_trait::async_trait]
impl LlmClient for WebMatrixClient {
 async fn structured_with_usage(&self,request:StructuredRequest<'_>)->Result<StructuredResponse,LlmError>{
  self.calls.fetch_add(1,Ordering::SeqCst);
  let input:Value=serde_json::from_str(&request.messages[1].content).unwrap();
  let url=input["sources"][0]["url"].as_str().unwrap();
  let value=if request.schema_name=="web_edition_paragraph_v1" {
   json!({"verdict":self.verdict,"source_url":url})
  } else {
   let chapter=&input["chapters"][0];
   let evidence="林昭走进旧宅发现一封信。";
   json!({"plot_summary":"林昭发现信件。","plot_source_url":url,"plot_evidence":evidence,"plot_chapter_indices":[1],"characters":[],"chapters":[{"index":chapter["index"],"heading":chapter["heading"],"summary":"林昭发现一封信。","source_url":url,"evidence":evidence}]})
  };
  Ok(StructuredResponse{value,usage:None})
 }
 async fn stream_text(&self,_:TextRequest,_:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,LlmError>{Err(LlmError::Config("unused".into()))}
}
fn web_lookup_fixture()->crate::web_search::OutlineLookup{
 crate::web_search::OutlineLookup{book_title:"钟楼书".into(),author:None,chapters:vec![crate::web_search::LocalOutlineChapter{index:1,heading:"第一章 雨夜".into(),text:"林昭走进旧宅发现一封信。她决定继续调查城南钟楼的线索，这是足够长的本地首段正文。".into()}]}
}
fn web_candidate_text()->&'static str{"钟楼书\n第一章 雨夜\n林昭走进旧宅发现一封信。这里进一步叙述城南钟楼的线索，正文长度足以支持来源章节匹配。"}
fn ddg_candidate_results(count:usize)->String{
 (0..count).map(|index|format!("<a class='result__a' href='https://public.example/{index}'>钟楼书</a><a class='result__snippet'>{}</a>",web_candidate_text())).collect()
}
#[tokio::test]
async fn ddg_ambiguous_sources_skip_tavily_and_fall_back_locally(){
 let ddg_server=MockServer::start().await;let tavily_server=MockServer::start().await;
 Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(200).set_body_string(ddg_candidate_results(2))).expect(1).mount(&ddg_server).await;
 Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&tavily_server).await;
 let ddg=crate::web_search::DuckDuckGoClient::with_endpoint(format!("{}/html/",ddg_server.uri())).unwrap();
 let tavily=crate::web_search::TavilyClient::with_endpoint("test-key".into(),format!("{}/search",tavily_server.uri())).unwrap();
 let llm=WebMatrixClient{verdict:"same",calls:AtomicUsize::new(0)};let token=CancellationToken::new();let rejections=parking_lot::Mutex::new(Vec::new());
 let result=find_web_outline_layers(Some(&ddg),Some(&tavily),&llm,&web_lookup_fixture(),crate::web_search::WebOutlineBudget::new(100_000,4_096),&token,&|_|{},&|engine,reason|rejections.lock().push((engine.to_owned(),reason))).await.unwrap();
 assert!(result.is_none());assert_eq!(llm.calls.load(Ordering::SeqCst),0);assert_eq!(tavily_server.received_requests().await.unwrap().len(),0);assert_eq!(rejections.lock()[0].1,crate::web_search::WebOutlineRejection::AmbiguousSources);
}

#[tokio::test]
async fn ddg_ai_different_skips_tavily_and_falls_back_locally(){
 let ddg_server=MockServer::start().await;let tavily_server=MockServer::start().await;
 Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(200).set_body_string(ddg_candidate_results(1))).expect(1).mount(&ddg_server).await;
 Mock::given(method("POST")).respond_with(ResponseTemplate::new(200)).expect(0).mount(&tavily_server).await;
 let ddg=crate::web_search::DuckDuckGoClient::with_endpoint(format!("{}/html/",ddg_server.uri())).unwrap();
 let tavily=crate::web_search::TavilyClient::with_endpoint("test-key".into(),format!("{}/search",tavily_server.uri())).unwrap();
 let llm=WebMatrixClient{verdict:"different",calls:AtomicUsize::new(0)};let token=CancellationToken::new();let rejections=parking_lot::Mutex::new(Vec::new());
 let result=find_web_outline_layers(Some(&ddg),Some(&tavily),&llm,&web_lookup_fixture(),crate::web_search::WebOutlineBudget::new(100_000,4_096),&token,&|_|{},&|engine,reason|rejections.lock().push((engine.to_owned(),reason))).await.unwrap();
 assert!(result.is_none());assert_eq!(llm.calls.load(Ordering::SeqCst),1);assert_eq!(tavily_server.received_requests().await.unwrap().len(),0);assert_eq!(rejections.lock()[0].1,crate::web_search::WebOutlineRejection::EditionUnverified);
}

#[tokio::test]
async fn ddg_infrastructure_failure_retries_tavily_successfully(){
 let ddg_server=MockServer::start().await;let tavily_server=MockServer::start().await;
 Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(500)).expect(2).mount(&ddg_server).await;
 Mock::given(method("POST")).and(path("/search")).respond_with(ResponseTemplate::new(200).set_body_json(json!({"results":[{"title":"钟楼书","url":"https://public.example/source","content":"","raw_content":web_candidate_text()}]}))).expect(1).mount(&tavily_server).await;
 let ddg=crate::web_search::DuckDuckGoClient::with_endpoint(format!("{}/html/",ddg_server.uri())).unwrap();
 let tavily=crate::web_search::TavilyClient::with_endpoint("test-key".into(),format!("{}/search",tavily_server.uri())).unwrap();
 let llm=WebMatrixClient{verdict:"same",calls:AtomicUsize::new(0)};let token=CancellationToken::new();
 let result=find_web_outline_layers(Some(&ddg),Some(&tavily),&llm,&web_lookup_fixture(),crate::web_search::WebOutlineBudget::new(100_000,4_096),&token,&|_|{},&|_,_|{}).await.unwrap();
 assert!(result.is_some());assert_eq!(ddg_server.received_requests().await.unwrap().len(),2);assert_eq!(tavily_server.received_requests().await.unwrap().len(),1);
}

#[tokio::test]
async fn web_layer_cancellation_is_terminal_before_any_engine_request(){
 let ddg_server=MockServer::start().await;let tavily_server=MockServer::start().await;
 let ddg=crate::web_search::DuckDuckGoClient::with_endpoint(format!("{}/html/",ddg_server.uri())).unwrap();
 let tavily=crate::web_search::TavilyClient::with_endpoint("test-key".into(),format!("{}/search",tavily_server.uri())).unwrap();
 let llm=OfflineClient::new();let token=CancellationToken::new();token.cancel();
 assert!(matches!(find_web_outline_layers(Some(&ddg),Some(&tavily),&llm,&web_lookup_fixture(),crate::web_search::WebOutlineBudget::new(100_000,4_096),&token,&|_|{},&|_,_|{}).await,Err(crate::web_search::WebOutlineError::Cancelled)));
 assert_eq!(ddg_server.received_requests().await.unwrap().len(),0);assert_eq!(tavily_server.received_requests().await.unwrap().len(),0);
}

#[test]
fn web_rejection_codes_include_engine_prefix_and_retry_only_infrastructure_failures(){
 assert_eq!(web_rejection_code("ddg",crate::web_search::WebOutlineRejection::RateLimited),"ddg_rate_limited");
 assert_eq!(web_rejection_code("tavily",crate::web_search::WebOutlineRejection::SearchUnavailable),"tavily_search_unavailable");
 assert!(web_rejection_is_retryable(crate::web_search::WebOutlineRejection::EvidenceInsufficient));
 assert!(!web_rejection_is_retryable(crate::web_search::WebOutlineRejection::AmbiguousSources));
 assert!(!web_rejection_is_retryable(crate::web_search::WebOutlineRejection::EditionUnverified));
}
#[tokio::test]
async fn web_layers_skip_tavily_without_key_after_ddg_infrastructure_failure(){
 let ddg_server=MockServer::start().await;
 Mock::given(method("POST")).and(path("/html/")).respond_with(ResponseTemplate::new(500)).expect(2).mount(&ddg_server).await;
 let ddg=crate::web_search::DuckDuckGoClient::with_endpoint(format!("{}/html/",ddg_server.uri())).unwrap();
 let lookup=crate::web_search::OutlineLookup{book_title:"钟楼书".into(),author:None,chapters:vec![crate::web_search::LocalOutlineChapter{index:1,heading:"第一章 雨夜".into(),text:"林昭在雨夜走进旧宅寻找钟楼的线索，这是足够长的本地首段正文。".into()}]};
 let client=OfflineClient::new();let token=CancellationToken::new();
 let result=find_web_outline_layers(Some(&ddg),None,&client,&lookup,crate::web_search::WebOutlineBudget::new(100_000,4_096),&token,&|_|{},&|_,_|{}).await.unwrap();
 assert!(result.is_none());
 assert_eq!(ddg_server.received_requests().await.unwrap().len(),2);
}
#[test]
fn local_failure_classification_distinguishes_provider_from_output_errors(){
 assert_eq!(local_failure_kind(&LlmError::Server{status:429,body:"secret body".into(),retry_after:None,request_id:None}),"rate_limited");
 assert_eq!(local_failure_kind(&LlmError::Server{status:504,body:"secret body".into(),retry_after:None,request_id:None}),"upstream_unavailable");
 assert_eq!(local_failure_kind(&LlmError::Timeout),"provider_timeout");
 assert_eq!(local_failure_kind(&LlmError::Stream("generation ended with length".into())),"output_truncated");
 assert_eq!(local_failure_kind(&LlmError::Schema("raw future secret".into())),"invalid_output");
 assert_eq!(local_failure_kind(&LlmError::Server{status:401,body:"secret body".into(),retry_after:None,request_id:None}),"authentication");
}
#[test]
fn failed_local_job_exposes_only_safe_error_kind_and_keeps_checkpoint(){
 let (conn,book,revision,_,spans,_)=fixture(vec!["林昭归来".into(),"林昭守城".into()],100);
 let job=jobs::enqueue(&conn.lock(),book,jobs::JobKind::QuickRead,json!({"phase":"quick_read","completed_slices":0,"total_slices":spans.len()})).unwrap();
 jobs::mark_running(&conn.lock(),job).unwrap();
 settle_quick_read_failure(&conn.lock(),book,job,false,"local_failure_kind:rate_limited:secret prompt and api key").unwrap();
 let (payload,error):(String,String)=conn.lock().query_row("SELECT payload_json,error FROM jobs WHERE id=?1",[job],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
 assert_eq!(serde_json::from_str::<Value>(&payload).unwrap()["local_failure_kind"],"rate_limited");
 assert!(!format!("{payload}{error}").contains("secret"));
 assert_eq!(repo::pending_slices(&conn.lock(),revision).unwrap(),vec![0]);
}
#[test]
fn preface_book_lookup_keeps_only_first_substantive_chapter_prose_for_local_proof(){
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 let book=create_book_with_chapters(&conn,"悟空传",None,"txt","offline",&[
  NewChapter{idx:1,title:"前言".into(),text:"前言原文LOCAL_PREFACE".into(),token_est:9},
  NewChapter{idx:2,title:"第02章".into(),text:"第二章正文LOCAL_EDITION_PROOF".into(),token_est:20},
  NewChapter{idx:3,title:"第03章".into(),text:"后续内容LOCAL_FUTURE".into(),token_est:17},
 ]).unwrap();
 let (_,texts)=sources_and_text(&conn,book).unwrap();let lookup=outline_lookup(&conn,book,&texts).unwrap();
 assert!(lookup.chapters[0].text.is_empty());
 assert_eq!(lookup.chapters[1].text,"第二章正文LOCAL_EDITION_PROOF");
 assert!(lookup.chapters[2].text.is_empty());
 assert_eq!(lookup.chapters.iter().map(|chapter|chapter.index).collect::<Vec<_>>(),vec![1,2,3]);
}

#[test]
fn local_fallback_stays_with_the_selected_provider_or_fails_closed() {
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 for name in ["default", "selected"] {
  let key=format!("fallback-{name}");
  conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge) VALUES (?1,'openai','https://example.test/v1',?2,?1,'writer')",rusqlite::params![name,key]).unwrap();
  crate::creds::save(&key,"test-key").unwrap();
 }
 let selected:i64=conn.query_row("SELECT id FROM llm_providers WHERE name='selected'",[],|row|row.get(0)).unwrap();
 assert_ne!(super::super::kg::runtime_config(&conn,None).unwrap().0.id,selected);
 let (provider,config)=local_fallback_config(&conn,selected).unwrap();
 assert_eq!(provider.id,selected);
 assert_eq!(config.model_extract,"selected");
 conn.execute("DELETE FROM llm_providers WHERE id=?1",[selected]).unwrap();
 assert!(local_fallback_config(&conn,selected).is_err());
}
fn config()->RuntimeConfig {RuntimeConfig{provider_id:1,model_extract:"offline".into(),context_limit:250_000,output_reserve:4096,prompt_version:"quick-read/v1".into()}}
fn fixture(texts:Vec<String>,max:usize)->(parking_lot::Mutex<Connection>,i64,i64,Vec<SourceChapter>,Vec<Vec<SourceSpan>>,IndexedTexts){
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 let chapters=texts.into_iter().enumerate().map(|(idx,text)|NewChapter{idx:idx as i64+1,title:format!("Chapter {}",idx+1),text,token_est:10}).collect::<Vec<_>>();
 let book=create_book_with_chapters(&conn,"Offline worker",None,"txt","test",&chapters).unwrap();
 let (sources,texts)=sources_and_text(&conn,book).unwrap();let spans=plan_slices(&sources,&texts,max);
 let revision=repo::prepare_revision(&conn,book,&sources,"offline-dependency",1,"offline","quick-read/v1","quick-read-summary/v1",&spans).unwrap();
 (parking_lot::Mutex::new(conn),book,revision,sources,spans,index_texts(texts))
}
async fn batch(conn:&parking_lot::Mutex<Connection>,revision:i64,spans:&[Vec<SourceSpan>],texts:&IndexedTexts,client:&OfflineClient,limit:&Semaphore)->Result<(),String>{
 let pending=repo::pending_slices(&conn.lock(),revision).unwrap();
 run_slice_batch(&pending,spans,texts,&config(),client,&client.control.cancellation,limit,|slice,usage|repo::save_slice(&conn.lock(),revision,slice,usage)).await
}
fn assert_exact_requests(client:&OfflineClient,texts:&IndexedTexts){
 let mut received=std::collections::BTreeMap::<i64,Vec<(usize,String)>>::new();
 for request in client.slice_requests(){for chapter in request["chapters"].as_array().unwrap(){received.entry(chapter["chapter_idx"].as_i64().unwrap()).or_default().push((chapter["start_char"].as_u64().unwrap() as usize,chapter["text"].as_str().unwrap().to_owned()));}}
 for (chapter,(original,_)) in texts {let parts=received.get_mut(chapter).unwrap();parts.sort_by_key(|(start,_)|*start);let mut cursor=0;let mut rebuilt=String::new();for (start,text) in parts {assert_eq!(*start,cursor);cursor+=text.chars().count();rebuilt.push_str(text);}assert_eq!(&rebuilt,original);}
}
#[tokio::test]
async fn worker_reads_every_utf8_character_exactly_once_with_concurrent_requests(){
 let (conn,_,revision,sources,spans,texts)=fixture(vec!["阿七🗝️\n顾岚在城门。外".into(),"e\u{301}跨章证据🚪甲乙丙丁戊".into()],4);
 let mut client=OfflineClient::new();client.barrier=Some(tokio::sync::Barrier::new(2));
 // Even slice count lets each deterministic pair rendezvous without sleeps.
 assert_eq!(spans.len()%2,0);
 batch(&conn,revision,&spans,&texts,&client,&Semaphore::new(2)).await.unwrap();
 assert_eq!(client.peak.load(Ordering::SeqCst),2);assert_exact_requests(&client,&texts);
 crate::core::quick_read::validate_quick_read_coverage(&sources,&repo::load_slices(&conn.lock(),revision).unwrap()).unwrap();
}
#[tokio::test]
async fn worker_resumes_only_missing_checkpoint_after_504_and_keeps_reader_text(){
 let original="阿七🗝️顾岚归来".to_owned();let (conn,book,revision,sources,spans,texts)=fixture(vec![original.clone()],3);
 let mut failed=OfflineClient::new();failed.fail_slice=Some(1);
 assert!(batch(&conn,revision,&spans,&texts,&failed,&Semaphore::new(2)).await.unwrap_err().contains("504"));
 repo::mark_failed(&conn.lock(),revision,"Provider 504").unwrap();
 assert_eq!(repo::pending_slices(&conn.lock(),revision).unwrap(),vec![1]);
 assert_eq!(repo::prepare_revision(&conn.lock(),book,&sources,"offline-dependency",1,"offline","quick-read/v1","quick-read-summary/v1",&spans).unwrap(),revision);
 let retry=OfflineClient::new();batch(&conn,revision,&spans,&texts,&retry,&Semaphore::new(2)).await.unwrap();
 assert_eq!(retry.slice_requests().len(),1);assert_eq!(retry.slice_requests()[0]["slice_idx"],1);
 assert_eq!(conn.lock().query_row("SELECT original_text FROM chapters WHERE book_id=?1 AND idx=1",[book],|row|row.get::<_,String>(0)).unwrap(),original);
 crate::core::quick_read::validate_quick_read_coverage(&sources,&repo::load_slices(&conn.lock(),revision).unwrap()).unwrap();
}
#[tokio::test]
async fn cancellation_rejects_late_response_without_checkpoint_or_reader_mutation(){
 let original="阿七最后一章".to_owned();let (conn,book,revision,_,spans,texts)=fixture(vec![original.clone()],100);
 let mut client=OfflineClient::new();client.late_cancel=true;
 assert!(batch(&conn,revision,&spans,&texts,&client,&Semaphore::new(2)).await.is_err());
 assert_eq!(client.slice_requests().len(),1);assert_eq!(repo::pending_slices(&conn.lock(),revision).unwrap(),vec![0]);assert!(repo::load_slices(&conn.lock(),revision).unwrap().is_empty());
 assert_eq!(conn.lock().query_row("SELECT original_text FROM chapters WHERE book_id=?1",[book],|row|row.get::<_,String>(0)).unwrap(),original);
}
#[tokio::test]
async fn million_character_synthetic_book_has_complete_coverage_and_one_summary_pass(){
 let original="阿七🗝️\n".repeat(200_000);assert_eq!(original.chars().count(),1_000_000);
 let (conn,book,revision,sources,spans,texts)=fixture(vec![original],10_000);
 let mut client=OfflineClient::new();client.barrier=Some(tokio::sync::Barrier::new(2));
 batch(&conn,revision,&spans,&texts,&client,&Semaphore::new(2)).await.unwrap();
 assert_eq!(client.peak.load(Ordering::SeqCst),2);assert_exact_requests(&client,&texts);
 let slices=repo::load_slices(&conn.lock(),revision).unwrap();crate::core::quick_read::validate_quick_read_coverage(&sources,&slices).unwrap();
 let reply=run_summary(&slices,&config(),&client,&client.control.cancellation,&Semaphore::new(2)).await.unwrap();
 let (cards,plot_summary,notes)=finish_summary(&sources,&slices,reply).unwrap();
 assert_eq!(client.requests.lock().iter().filter(|(name,_)|name=="quick_read_summary").count(),1);
 for slice in &slices {for note in &slice.plot_notes {assert!(notes.contains(note));}for mention in &slice.characters{assert!(cards.iter().any(|card|card.identity_evidence.contains(&mention.evidence)&&card.source_chapters.contains(&mention.chapter_idx)));}}
 let reference=crate::core::quick_read::QuickReadReference{schema_version:"quick-read-summary/v1".into(),source_digest:repo::source_digest(&sources),source_kind:crate::core::quick_read::QuickReadSourceKind::LocalFullText,provider_id:1,model:"offline".into(),prompt_version:"quick-read/v1".into(),chapter_summaries:crate::core::quick_read::canonical_chapter_summaries(&slices),characters:cards,plot_summary,plot_notes:notes,completed_spans:slices.iter().flat_map(|slice|slice.source_spans.clone()).collect(),source_provenance:vec![]};
 repo::publish_reference(&conn.lock(),revision,&reference).unwrap();assert_eq!(repo::active_reference(&conn.lock(),book).unwrap(),Some(reference));
 // This is a synthetic scheduler/coverage case, not a Provider throughput benchmark.
}
#[tokio::test]
async fn oversized_summary_fails_before_call_without_truncating_checkpoint_evidence(){
 let (conn,book,revision,sources,spans,texts)=fixture(vec!["阿七归来".into()],2);let client=OfflineClient::new();
 batch(&conn,revision,&spans,&texts,&client,&Semaphore::new(2)).await.unwrap();let before=repo::load_slices(&conn.lock(),revision).unwrap();
 let mut small=config();small.context_limit=small.output_reserve+10;
 let failure=run_summary(&before,&small,&client,&client.control.cancellation,&Semaphore::new(2)).await.err().unwrap();assert_eq!(failure,"local_failure_kind:context_insufficient");
 repo::mark_failed(&conn.lock(),revision,&failure).unwrap();assert_eq!(repo::load_slices(&conn.lock(),revision).unwrap(),before);assert!(repo::pending_slices(&conn.lock(),revision).unwrap().is_empty());
 assert_eq!(client.requests.lock().iter().filter(|(name,_)|name=="quick_read_summary").count(),0);
 assert_eq!(conn.lock().query_row("SELECT original_text FROM chapters WHERE book_id=?1",[book],|row|row.get::<_,String>(0)).unwrap(),"阿七归来");
 let slice_calls=client.slice_requests().len();batch(&conn,revision,&spans,&texts,&client,&Semaphore::new(2)).await.unwrap();assert_eq!(client.slice_requests().len(),slice_calls);
 let reply=run_summary(&before,&config(),&client,&client.control.cancellation,&Semaphore::new(2)).await.unwrap();
 let (cards,plot_summary,plot_notes)=finish_summary(&sources,&before,reply).unwrap();
 let reference=crate::core::quick_read::QuickReadReference{schema_version:"quick-read-summary/v1".into(),source_digest:repo::source_digest(&sources),source_kind:crate::core::quick_read::QuickReadSourceKind::LocalFullText,provider_id:1,model:"offline".into(),prompt_version:"quick-read/v1".into(),chapter_summaries:crate::core::quick_read::canonical_chapter_summaries(&before),characters:cards,plot_summary,plot_notes,completed_spans:before.iter().flat_map(|slice|slice.source_spans.clone()).collect(),source_provenance:vec![]};
 repo::publish_reference(&conn.lock(),revision,&reference).unwrap();assert_eq!(repo::active_reference(&conn.lock(),book).unwrap(),Some(reference));
 assert_eq!(client.requests.lock().iter().filter(|(name,_)|name=="quick_read_summary").count(),1);
}
#[tokio::test]
async fn foreground_waiter_gets_shared_permit_before_next_slice(){
 let semaphore=Semaphore::new(1);let token=CancellationToken::new();let holder=semaphore.acquire().await.unwrap();
 let foreground=semaphore.acquire();tokio::pin!(foreground);assert!(matches!(futures_util::poll!(&mut foreground),std::task::Poll::Pending));
 let background=acquire_provider_permit(&semaphore,&token);tokio::pin!(background);assert!(matches!(futures_util::poll!(&mut background),std::task::Poll::Pending));
 drop(holder);let foreground=foreground.await.unwrap();assert!(matches!(futures_util::poll!(&mut background),std::task::Poll::Pending));
 token.cancel();assert!(background.await.is_err());assert_eq!(semaphore.available_permits(),0);drop(foreground);assert_eq!(semaphore.available_permits(),1);
}
#[test]
fn summary_with_unassigned_character_evidence_is_recoverable_not_silent_loss(){
 let (_,_,_,sources,spans,_)=fixture(vec!["阿七归来".into()],100);
 let slice=QuickReadSlice{slice_idx:0,source_spans:spans[0].clone(),chapter_summaries:vec![],characters:vec![crate::core::quick_read::QuickReadCharacterMention{local_id:"s0:c".into(),name:"阿七".into(),aliases:vec![],chapter_idx:1,evidence:"不可丢弃".into()}],plot_notes:vec![]};
 let reply=SummaryReply{characters:vec![],plot_summary:"主线".into(),plot_notes:vec![]};
 assert!(finish_summary(&sources,&[slice],reply).unwrap_err().contains("omits character evidence"));
}

#[test]
fn completed_bootstrap_requeues_when_current_extraction_config_is_stale(){
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge) VALUES ('bootstrap','openai','https://example.test/v1','bootstrap-test-key','reader','writer')",[]).unwrap();crate::creds::save("bootstrap-test-key","test-key").unwrap();let provider=conn.last_insert_rowid();
 let book=create_book_with_chapters(&conn,"Bootstrap",None,"txt","test",&[NewChapter{idx:1,title:"One".into(),text:"first original".into(),token_est:3},NewChapter{idx:2,title:"Two".into(),text:"second original".into(),token_est:3}]).unwrap();
 let (_,config)=super::super::kg::runtime_config(&conn,Some(provider)).unwrap();let line=crate::store::kg::line_version(&conn,book).unwrap();
 for chapter in 1..=2 {let text=crate::store::kg::text_version(&conn,book,chapter,"original",None).unwrap();let snapshot=crate::core::kg::ChapterSnapshot{summary:format!("chapter {chapter}"),..Default::default()};crate::store::kg::commit_snapshot(&conn,&text,&snapshot,"bootstrap",line,None,&config).unwrap();crate::store::story_projection::save_chapter_memory(&conn,&text,&snapshot,"bootstrap").unwrap();}
 let digest=repo::source_digest(&repo::source_chapters(&conn,book).unwrap());let done=crate::jobs::enqueue(&conn,book,crate::jobs::JobKind::BootstrapWiki,json!({"chapter_idx":1,"source_digest":digest})).unwrap();crate::jobs::mark_done(&conn,done).unwrap();
 assert_eq!(ensure_bootstrap_job(&conn,book,provider,&digest).unwrap(),None);
 conn.execute("UPDATE llm_providers SET params_json='{\"context_limit\":65536,\"output_reserve\":4096}' WHERE id=?1",[provider]).unwrap();
 assert!(matches!(ensure_bootstrap_job(&conn,book,provider,&digest).unwrap(),Some(retry) if retry!=done));
}

#[test]
fn changed_runtime_model_cannot_supersede_the_existing_active_reference(){
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge) VALUES ('guard','openai','https://example.test/v1','guard-key','reader','writer')",[]).unwrap();crate::creds::save("guard-key","test-key").unwrap();let provider=conn.last_insert_rowid();
 let book=create_book_with_chapters(&conn,"Guard",None,"txt","test",&[NewChapter{idx:1,title:"One".into(),text:"source".into(),token_est:2}]).unwrap();
 conn.execute("INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,source_kind,total_chars,total_chapters,total_slices,status,active) VALUES (?1,'old-source','old-dependency',?2,'reader','quick-read/v1','quick-read-summary/v1','local_full_text',6,1,1,'ready',1)",rusqlite::params![book,provider]).unwrap();let active=conn.last_insert_rowid();
 let reference=crate::core::quick_read::QuickReadReference{schema_version:"web-outline/v1".into(),source_digest:"new-source".into(),source_kind:crate::core::quick_read::QuickReadSourceKind::WebOutline,provider_id:provider,model:"reader".into(),prompt_version:"web-outline/v1".into(),chapter_summaries:vec![],characters:vec![],plot_summary:"plot".into(),plot_notes:vec![],completed_spans:vec![],source_provenance:vec![]};
 let proof=crate::web_search::EditionProof{chapter_index:1,method:crate::web_search::EditionProofMethod::AiParagraph,provenance:crate::web_search::WebOutlineProvenance{url:"https://public.example/source".into(),title:"source".into(),evidence:"公开候选首段".into()}};
 conn.execute("UPDATE llm_providers SET model_extract='reader-v2' WHERE id=?1",[provider]).unwrap();assert!(!publish_web_reference_if_current(&conn,book,provider,"reader","Guard",&proof,"old-dependency",&[],&reference).unwrap());
 assert_eq!(conn.query_row("SELECT id FROM quick_read_revisions WHERE book_id=?1 AND active=1",[book],|row|row.get::<_,i64>(0)).unwrap(),active);
}

#[test]
fn explicit_nondefault_provider_can_publish_verified_web_reference() {
 let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
 conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge) VALUES ('default','openai','https://example.test/v1','web-default-key','default-reader','writer')",[]).unwrap();
 for key in ["web-default-key","web-selected-key","web-selected-tavily"] {crate::creds::save(key,"test-key").unwrap();}
 conn.execute("INSERT INTO llm_providers(name,protocol,base_url,key_ref,model_extract,model_merge,tavily_key_ref,params_json) VALUES ('selected','openai','https://example.test/v1','web-selected-key','reader','writer','web-selected-tavily','{\"tavily_enabled\":true}')",[]).unwrap();
 let selected=conn.last_insert_rowid();
 let book=create_book_with_chapters(&conn,"Guard",None,"txt","test",&[NewChapter{idx:1,title:"One".into(),text:"source chapter".into(),token_est:4}]).unwrap();
 let sources=repo::source_chapters(&conn,book).unwrap();
 let proof=crate::web_search::EditionProof{chapter_index:1,method:crate::web_search::EditionProofMethod::AiParagraph,provenance:crate::web_search::WebOutlineProvenance{url:"https://public.example/source".into(),title:"source".into(),evidence:"公开候选首段足够长".into()}};
 let reference=crate::core::quick_read::QuickReadReference{schema_version:"web-outline/v1".into(),source_digest:repo::source_digest(&sources),source_kind:crate::core::quick_read::QuickReadSourceKind::WebOutline,provider_id:selected,model:"reader".into(),prompt_version:"web-outline/ai-paragraph-v1".into(),chapter_summaries:vec![crate::core::quick_read::ChapterQuickRead{chapter_idx:1,summary:"第一章大纲".into(),character_refs:vec!["web-c".into()]}],characters:vec![crate::core::quick_read::QuickReadCharacterCard{id:"web-c".into(),name:"阿七".into(),aliases:vec![],identity_evidence:vec!["公开文本描述".into()],motivations:vec![],narrative_roles:vec!["主线人物".into()],first_appearance_chapter:1,revelations:vec![],source_chapters:vec![1]}],plot_summary:"公开主线".into(),plot_notes:vec![crate::core::quick_read::QuickReadPlotNote{summary:"公开主线".into(),source_chapters:vec![1]}],completed_spans:vec![],source_provenance:vec![proof.provenance.clone()]};
 let dependency=web_outline_dependency(&conn,selected).unwrap().unwrap();
 assert!(publish_web_reference_if_current(&conn,book,selected,"reader","Guard",&proof,&dependency,&[(1,"One".into(),proof.provenance.url.clone())],&reference).unwrap());
 assert_eq!(repo::active_reference(&conn,book).unwrap(),Some(reference));
}
