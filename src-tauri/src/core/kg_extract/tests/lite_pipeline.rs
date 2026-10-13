use crate::commands::AppState;
use crate::core::kg::{GraphQuery, RuntimeConfig};
use crate::llm::{LlmClient, LlmError, StructuredRequest, StructuredResponse, TextRequest};
use crate::store::{books::{create_book_with_chapters, NewChapter}, kg, Db};
use serde_json::json;
use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};

struct CompactClient { calls: Arc<AtomicUsize>, hang: bool, delay: Option<std::time::Duration> }
#[async_trait::async_trait]
impl LlmClient for CompactClient {
    async fn structured_with_usage(&self, request: StructuredRequest<'_>) -> Result<StructuredResponse,LlmError> {
        self.calls.fetch_add(1,Ordering::SeqCst);
        if let Some(delay)=self.delay {tokio::time::sleep(delay).await;}
        if self.hang { return std::future::pending().await; }
        if request.schema_name != "kg_chapter" { return Err(LlmError::Config(format!("obsolete graph stage {}",request.schema_name))); }
        Ok(StructuredResponse {value:json!({
            "summary":"阿岚持剑抵达客栈，等待同伴。",
            "entities":[
                {"ref":"c_a","type":"character","name":"阿岚","aliases":[],"description":"受伤但活着","life_status":"alive","location":"l_p","owner":null},
                {"ref":"l_p","type":"location","name":"客栈","aliases":[],"description":"暂时落脚处","life_status":null,"location":null,"owner":null},
                {"ref":"i_s","type":"item","name":"宝剑","aliases":[],"description":"随身武器","life_status":null,"location":"l_p","owner":"c_a"}
            ],
            "relations":[{"from":"c_a","to":"i_s","label":"持有","description":"阿岚携带宝剑"}],
            "events":[{"title":"抵达客栈","participants":["c_a"],"location":"l_p","time":"夜间","summary":"阿岚持剑进入客栈"}],
            "open_threads":["同伴尚未抵达"]
        }),usage:Some((4100,420))})
    }
    async fn stream_text(&self,_:TextRequest,_:Arc<dyn for<'a> Fn(&'a str)+Send+Sync>)->Result<String,LlmError>{Err(LlmError::Config("extraction does not write prose".into()))}
}
fn setup_content(content:String)->(AppState,i64,crate::core::kg::TextVersion,RuntimeConfig){
    let conn=rusqlite::Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();Db::new(&conn).migrate().unwrap();
    let book=create_book_with_chapters(&conn,"轻量图谱",None,"txt","lite.txt",&[NewChapter{idx:1,title:"落脚".into(),text:content,token_est:4000}]).unwrap();
    let text=kg::text_version(&conn,book,1,"original",None).unwrap();
    (AppState{db:parking_lot::Mutex::new(Some(conn))},book,text,RuntimeConfig{provider_id:1,model_extract:"compact".into(),context_limit:100_000,output_reserve:1024,prompt_version:"kg-lite/v1".into()})
}
fn setup()->(AppState,i64,crate::core::kg::TextVersion,RuntimeConfig){
    setup_content(format!("阿岚背着宝剑走进客栈，等待同伴。{}", "风雨交加，路人匆匆。".repeat(350)))
}
#[tokio::test]
async fn unaffordable_whole_chapter_sends_no_paid_partial_request(){
    let (state,_,text,config)=setup_content("雨".repeat(4097));let id=text.id;
    let calls=Arc::new(AtomicUsize::new(0));
    let mut budget=super::ExtractionBudget::new(45,tokio::time::Instant::now()+std::time::Duration::from_secs(10));budget.calls_used=44;
    super::extract_with_budget(&state,&CompactClient{calls:calls.clone(),hang:false,delay:None},text,&config,None,&mut budget).await.unwrap_err();
    assert_eq!(calls.load(Ordering::SeqCst),0);assert_eq!(budget.calls_used,44);
    assert!(kg::active_revision(state.db.lock().as_ref().unwrap(),id).unwrap().is_none());
}
#[tokio::test]
async fn one_compact_call_persists_writing_state_and_timeline(){
    let (state,book,text,config)=setup();let calls=Arc::new(AtomicUsize::new(0));
    crate::core::kg_extract::extract(&state,&CompactClient{calls:calls.clone(),hang:false,delay:None},text,&config,None).await.unwrap();
    assert_eq!(calls.load(Ordering::SeqCst),1);
    let guard=state.db.lock();let conn=guard.as_ref().unwrap();
    let view=kg::internal_view(conn,&GraphQuery{book_id:book,scope:"chapter".into(),purpose:"reference".into(),chapter_idx:Some(1)}).unwrap();
    assert!(view.ready);
    let data=serde_json::to_value(&view).unwrap();
    let person=data["nodes"].as_array().unwrap().iter().find(|n|n["name"]=="阿岚").unwrap();
    let place=data["nodes"].as_array().unwrap().iter().find(|n|n["name"]=="客栈").unwrap();
    let sword=data["nodes"].as_array().unwrap().iter().find(|n|n["name"]=="宝剑").unwrap();
    assert_eq!(person["life_status"],"alive");assert_eq!(person["location"],place["key"]);assert_eq!(sword["owner"],person["key"]);
    assert_eq!(data["edges"][0]["src"],person["key"]);assert_eq!(data["edges"][0]["dst"],sword["key"]);
    assert_eq!(data["events"][0]["participants"],json!([person["key"]]));assert_eq!(data["events"][0]["time"],"夜间");
    assert_eq!(data["open_threads"],json!(["同伴尚未抵达"]));
}
#[tokio::test(start_paused=true)]
async fn a_hanging_chapter_fails_at_the_broad_operation_timeout_without_publishing(){
    let (state,_,text,config)=setup();let calls=Arc::new(AtomicUsize::new(0));let id=text.id;
    let result=tokio::time::timeout(super::REQUEST_TIMEOUT+std::time::Duration::from_secs(1),crate::core::kg_extract::extract(&state,&CompactClient{calls:calls.clone(),hang:true,delay:None},text,&config,None)).await;
    assert!(matches!(result,Ok(Err(_))),"chapter must return at its broad operation timeout");
    assert_eq!(calls.load(Ordering::SeqCst),1);
    let guard=state.db.lock();assert!(kg::active_revision(guard.as_ref().unwrap(),id).unwrap().is_none());
}

#[tokio::test(start_paused=true)]
async fn a_slow_successful_chapter_is_not_rejected_by_the_experience_target(){
    let (state,book,text,config)=setup();let calls=Arc::new(AtomicUsize::new(0));
    let result=crate::core::kg_extract::extract(&state,&CompactClient{calls:calls.clone(),hang:false,delay:Some(std::time::Duration::from_secs(10))},text,&config,None).await;
    assert!(result.is_ok());assert_eq!(calls.load(Ordering::SeqCst),1);
    let view=kg::internal_view(state.db.lock().as_ref().unwrap(),&GraphQuery{book_id:book,scope:"chapter".into(),purpose:"reference".into(),chapter_idx:Some(1)}).unwrap();assert!(view.ready);
}

#[test]
fn request_contract_rejects_character_references_as_locations(){
    let (_,_,text,_)=setup_content("甲在客栈".into());
    let known=super::decode_graph(json!({"summary":"甲在客栈","entities":[
        {"ref":"a","type":"character","name":"甲","aliases":[],"description":"旅人","life_status":null,"location":"p","owner":null},
        {"ref":"p","type":"location","name":"客栈","aliases":[],"description":"地点","life_status":null,"location":null,"owner":null}
    ],"relations":[],"events":[],"open_threads":[]}),&text,&[],"ai_original").unwrap();
    let schema=super::extraction_schema(&known.nodes);
    let validator=jsonschema::validator_for(&schema).unwrap();
    let mut reply=json!({"summary":"乙抵达客栈","entities":[
        {"ref":"c_visitor","type":"character","name":"乙","aliases":[],"description":"旅人","life_status":"alive","location":"k1","owner":null}
    ],"relations":[],"events":[],"open_threads":[]});
    assert!(validator.is_valid(&reply));
    let snapshot=super::decode_graph(reply.clone(),&text,&known.nodes,"ai_original").unwrap();
    assert_eq!(snapshot.nodes[0].location.as_deref(),Some(known.nodes[1].key.as_str()));
    reply["entities"][0]["location"]=json!("k0");
    assert!(!validator.is_valid(&reply),"人物注册表引用不可充当地点");
    reply["entities"][0]["location"]=json!("c_visitor");
    assert!(!validator.is_valid(&reply),"新人物引用不可充当地点");
    reply["entities"][0]["location"]=json!("l_inn");
    reply["entities"].as_array_mut().unwrap().push(json!({"ref":"l_inn","type":"location","name":"新客栈","aliases":[],"description":"地点","life_status":null,"location":null,"owner":null}));
    assert!(validator.is_valid(&reply));
    assert!(super::decode_graph(reply,&text,&known.nodes,"ai_original").is_ok());
}

#[test]
fn supplementing_a_sampling_gap_is_not_overridden_by_later_unknown_state(){
    let conn=rusqlite::Connection::open_in_memory().unwrap();Db::new(&conn).migrate().unwrap();
    let chapters=(1..=4).map(|idx|NewChapter{idx,title:format!("Chapter {idx}"),text:"阿岚在客栈持有宝剑".into(),token_est:20}).collect::<Vec<_>>();
    let book=create_book_with_chapters(&conn,"Sparse state",None,"txt","sparse.txt",&chapters).unwrap();
    let config=RuntimeConfig{provider_id:1,model_extract:"compact".into(),context_limit:32768,output_reserve:1024,prompt_version:"kg-lite/v1".into()};
    let first=kg::text_version(&conn,book,1,"original",None).unwrap();
    let initial=super::decode_graph(json!({"summary":"阿岚持剑在客栈","entities":[
        {"ref":"a","type":"character","name":"阿岚","aliases":[],"description":"旅人","life_status":"alive","location":"p","owner":null},
        {"ref":"b","type":"character","name":"同伴","aliases":[],"description":"同伴","life_status":"alive","location":null,"owner":null},
        {"ref":"p","type":"location","name":"客栈","aliases":[],"description":"客栈","life_status":null,"location":null,"owner":null},
        {"ref":"q","type":"location","name":"城门","aliases":[],"description":"城门","life_status":null,"location":null,"owner":null},
        {"ref":"s","type":"item","name":"宝剑","aliases":[],"description":"武器","life_status":null,"location":"p","owner":"a"}
    ],"relations":[],"events":[],"open_threads":[]}),&first,&[],"ai_original").unwrap();
    kg::commit_snapshot(&conn,&first,&initial,"first",0,None,&config).unwrap();
    let late=kg::text_version(&conn,book,4,"original",None).unwrap();
    let unknown=super::decode_graph(json!({"summary":"本章未确认人物和宝剑状态","entities":[
        {"ref":"k0","type":"character","name":"阿岚","aliases":[],"description":"旅人","life_status":null,"location":null,"owner":null},
        {"ref":"k4","type":"item","name":"宝剑","aliases":[],"description":"武器","life_status":null,"location":null,"owner":null}
    ],"relations":[],"events":[],"open_threads":[]}),&late,&initial.nodes,"ai_original").unwrap();
    kg::commit_snapshot(&conn,&late,&unknown,"late",0,None,&config).unwrap();
    let middle=kg::text_version(&conn,book,2,"original",None).unwrap();let mut changed=initial.clone();
    changed.nodes[0].life_status=Some("dead".into());changed.nodes[0].location=Some(initial.nodes[3].key.clone());changed.nodes[0].chapter_idx=2;
    changed.nodes[4].owner=Some(initial.nodes[1].key.clone());changed.nodes[4].location=Some(initial.nodes[3].key.clone());changed.nodes[4].chapter_idx=2;
    kg::commit_snapshot(&conn,&middle,&changed,"middle",0,None,&config).unwrap();
    let view=kg::internal_view(&conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"reference".into(),chapter_idx:Some(4)}).unwrap();
    let hero=view.nodes.iter().find(|n|n.name=="阿岚").unwrap();let sword=view.nodes.iter().find(|n|n.name=="宝剑").unwrap();
    assert_eq!(hero.life_status.as_deref(),Some("dead"));assert_eq!(hero.location,Some(initial.nodes[3].key.clone()));
    assert_eq!(sword.owner,Some(initial.nodes[1].key.clone()));assert_eq!(sword.location,Some(initial.nodes[3].key.clone()));
}
