use super::*;
use crate::store::{books::{create_book_with_chapters, NewChapter}, Db};

fn fixture() -> (Connection, i64) {
    let conn=Connection::open_in_memory().unwrap(); Db::apply_pragmas(&conn).unwrap(); Db::new(&conn).migrate().unwrap();
    let chapters=(1..=4).map(|idx|NewChapter{idx,title:format!("Chapter {idx}"),text:"甲在城中。乙仍活着。".into(),token_est:20}).collect::<Vec<_>>();
    let book=create_book_with_chapters(&conn,"test",None,"txt","test",&chapters).unwrap(); (conn,book)
}
fn config()->RuntimeConfig { RuntimeConfig{provider_id:1,model_extract:"model".into(),context_limit:32768,output_reserve:4096,prompt_version:"kg-lite/v1".into()} }
fn snapshot(idx:i64)->ChapterSnapshot { ChapterSnapshot{summary:format!("chapter {idx}"),nodes:vec![GraphNode{key:"hero".into(),entity_type:"character".into(),name:"甲".into(),aliases:vec![],summary:None,life_status:Some("alive".into()),location:None,owner:None,chapter_idx:idx,provenance:"ai_original".into()}],edges:vec![],events:vec![],open_threads:vec![format!("thread {idx}")]} }
fn query(book:i64,purpose:&str,idx:i64)->GraphQuery {GraphQuery{book_id:book,scope:"global".into(),purpose:purpose.into(),chapter_idx:Some(idx)}}
fn commit(conn:&Connection,book:i64,idx:i64)->i64 {let text=text_version(conn,book,idx,"original",None).unwrap();commit_snapshot(conn,&text,&snapshot(idx),"dep",line_version(conn,book).unwrap(),None,&config()).unwrap()}
#[test]
fn sparse_reference_plan_is_complete_without_skipped_text_materialization() {
    let (conn,book)=fixture();set_extraction_plan(&conn,book,&[1,4],2,100).unwrap();commit(&conn,book,1);commit(&conn,book,4);
    let sampled=coverage(&conn,book).unwrap();assert_eq!(sampled.mode,"sampled");assert!(sampled.target_complete);assert_eq!(sampled.missing_chapters,vec![2,3]);
    assert!(internal_view(&conn,&query(book,"reference",4)).unwrap().ready);
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM kg_text_versions",[],|r|r.get::<_,i64>(0)).unwrap(),2);
    set_extraction_plan(&conn,book,&[1,4],0,0).unwrap();record_extraction_attempt(&conn,book,3,200).unwrap();assert_eq!(coverage(&conn,book).unwrap().calls_used,3);
}
#[test]
fn chapter_readiness_and_actual_gaps_do_not_claim_full_history() {
    let (conn,book)=fixture();commit(&conn,book,4);mark_read(&conn,book,4).unwrap();
    let mut q=query(book,"actual",4);q.scope="chapter".into();let chapter=internal_view(&conn,&q).unwrap();assert!(chapter.ready);assert_eq!(chapter.coverage.missing_chapters,vec![1,2,3]);
    assert!(!internal_view(&conn,&query(book,"actual",4)).unwrap().ready);
}
#[test]
fn snapshot_state_folds_in_order_and_edit_undo_preserves_actual() {
    let (conn,book)=fixture();commit(&conn,book,1);let text=text_version(&conn,book,2,"original",None).unwrap();let mut dead=snapshot(2);dead.nodes[0].life_status=Some("dead".into());commit_snapshot(&conn,&text,&dead,"dep",0,None,&config()).unwrap();mark_read(&conn,book,1).unwrap();
    let key=get_view(&conn,&query(book,"actual",1)).unwrap().nodes[0].key.clone();
    let input=EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some(key),action:"upsert".into(),data:serde_json::json!({"life_status":"alive","location":null})};let id=edit(&conn,&input).unwrap();
    assert_eq!(internal_view(&conn,&query(book,"spec",2)).unwrap().nodes[0].life_status.as_deref(),Some("alive"));
    assert_eq!(internal_view(&conn,&query(book,"actual",2)).unwrap().nodes[0].life_status.as_deref(),Some("alive"));assert_eq!(internal_view(&conn,&query(book,"reference",2)).unwrap().nodes[0].life_status.as_deref(),Some("dead"));revert(&conn,book,id).unwrap();assert_eq!(internal_view(&conn,&query(book,"spec",2)).unwrap().nodes[0].life_status.as_deref(),Some("dead"));
}
#[test]
fn hidden_future_redacts_summary_threads_and_state_references() {
    let (conn,book)=fixture();commit(&conn,book,1);commit(&conn,book,2);mark_read(&conn,book,1).unwrap();let hidden=get_view(&conn,&query(book,"spec",2)).unwrap();assert!(hidden.summary.is_empty());assert!(hidden.open_threads.is_empty());assert!(hidden.nodes.is_empty());reveal(&conn,book,2).unwrap();assert_eq!(get_view(&conn,&query(book,"spec",2)).unwrap().summary,"chapter 1\nchapter 2");
}
#[test]
fn cancelled_or_stale_result_cannot_publish_or_checkpoint() {
    let (conn,book)=fixture();let text=text_version(&conn,book,1,"original",None).unwrap();conn.execute("INSERT INTO jobs(book_id,kind,state,payload_json) VALUES (?1,'global_extract','cancelled','{}')",[book]).unwrap();let job=conn.last_insert_rowid();
    assert!(commit_snapshot(&conn,&text,&snapshot(1),"dep",0,Some(job),&config()).is_err());assert!(!ready(&conn,text.id).unwrap());assert_eq!(job_checkpoint(&conn,job).unwrap(),serde_json::json!({}));
    conn.execute("UPDATE kg_lines SET version=1 WHERE book_id=?1",[book]).unwrap();assert!(commit_snapshot(&conn,&text,&snapshot(1),"dep",0,None,&config()).is_err());
}
#[test]
fn adding_original_sample_does_not_invalidate_existing_cache() {
    let (conn,book)=fixture();commit(&conn,book,4);let text=text_version(&conn,book,4,"original",None).unwrap();commit(&conn,book,1);assert!(extraction_matches(&conn,text.id,&config()).unwrap());let mut changed=config();changed.model_extract="other".into();assert!(!extraction_matches(&conn,text.id,&changed).unwrap());
}
#[test]
fn sparse_generation_context_keeps_prose_without_graph_backfill() {
    let (conn,book)=fixture();commit(&conn,book,1);mark_read(&conn,book,3).unwrap();let context=generation_context(&conn,book,4).unwrap();assert_eq!(context.previous_text,"甲在城中。乙仍活着。");assert_eq!(context.actual.coverage.missing_chapters,vec![2,3]);
    assert_eq!(conn.query_row("SELECT count(*) FROM kg_text_versions",[],|r|r.get::<_,i64>(0)).unwrap(),3);assert!(!conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_text_versions WHERE book_id=?1 AND chapter_idx=2)",[book],|r|r.get::<_,bool>(0)).unwrap());
}
#[test]
fn local_migration_preserves_prose_pins_ids_and_legacy_meaning() {
    let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();conn.execute_batch(include_str!("../../../../migrations/0001_init.sql")).unwrap();conn.execute_batch(include_str!("../../../../migrations/0002_kg_facts.sql")).unwrap();conn.pragma_update(None,"user_version",2).unwrap();
    conn.execute("INSERT INTO books(id,title,source_path,format) VALUES (1,'legacy','test','txt')",[]).unwrap();conn.execute("INSERT INTO chapters(book_id,idx,title,original_text) VALUES (1,1,'one','original bytes')",[]).unwrap();conn.execute("INSERT INTO kg_lines(book_id,progress,version) VALUES (1,1,7)",[]).unwrap();
    conn.execute("INSERT INTO kg_text_versions(id,book_id,chapter_idx,source,content,content_hash) VALUES (9,1,1,'original','original bytes','hash')",[]).unwrap();conn.execute("INSERT INTO kg_read_selection VALUES (1,1,9)",[]).unwrap();
    conn.execute("INSERT INTO kg_revisions(id,book_id,text_version_id,digest,dependency_digest,stage_a_version,stage_b_version,summary,open_threads_json,unresolved_json,active,schema_digest) VALUES (23,1,9,'d','dep','a','b','old summary','[]','[]',1,'old')",[]).unwrap();
    conn.execute("INSERT INTO kg_records(revision_id,kind,key,subject_key,observed_chapter,observed_order,data_json) VALUES (23,'identity','hero','hero',1,0,?1)",[r#"{"key":"hero","entity_type":"character","name":"甲","aliases":[]}"#]).unwrap();
    conn.execute("INSERT INTO graph_events(book_id,scope,chapter_idx,order_in_chapter,title,summary,outcome,provenance,revision_id,record_key,observed_chapter,observed_order,data_json) VALUES (1,'chapter',1,0,'battle','fought','won','ai_original',23,'battle',1,0,?1)",[r#"{"key":"battle","title":"battle","chapter_idx":1,"observed_order":0,"participants":["hero"],"location":null,"summary":"fought","outcome":"won"}"#]).unwrap();
    conn.execute("INSERT INTO kg_records(revision_id,kind,key,subject_key,observed_chapter,observed_order,data_json) VALUES (23,'change','death','hero',1,1,?1)",[r#"{"subject_key":"hero","predicate":"life_status","operation":"set","after":{"kind":"scalar","value":"dead"},"temporal_scope":"current","assertion_kind":"confirmed"}"#]).unwrap();
    conn.execute("INSERT INTO kg_records(revision_id,kind,key,subject_key,observed_chapter,observed_order,data_json) VALUES (23,'relation','old-rel','hero',1,0,?1)",[r#"{"src":"hero","dst":"hero","rel_type":"旧盟约","description":"promise","status":"broken","temporal_scope":"current","assertion_kind":"confirmed"}"#]).unwrap();
    let before=r#"{"key":"old-rel","src":"hero","dst":"hero","rel_type":"旧盟约","description":"promise","status":"broken","since_chapter":1,"chapter_idx":2,"provenance":"user"}"#;
    let after=r#"{"key":"old-rel","src":"hero","dst":"hero","rel_type":"旧盟约","description":"new promise","status":"former","since_chapter":1,"chapter_idx":2,"provenance":"user"}"#;
    conn.execute("INSERT INTO edits_log(id,book_id,target_type,target_id,action,before_json,after_json,effective_from,scope,active) VALUES (41,1,'edge','old-rel','upsert',?1,?2,2,'global',1)",params![before,after]).unwrap();
    conn.execute("INSERT INTO kg_specs(id,book_id,chapter_idx,dependency_digest,line_version,data_json,active) VALUES (51,1,2,'spec-dep',7,?1,1)",[r#"{"nodes":[{"key":"hero","type":"character","name":"甲","aliases":[],"summary":null,"chapter_idx":1,"provenance":"ai_original"}],"edges":[],"events":[],"purpose":"spec","reader_progress":1}"#]).unwrap();
    Db::new(&conn).migrate().unwrap();assert_eq!(active_revision(&conn,9).unwrap(),Some(23));assert_eq!(selected_text(&conn,1,1).unwrap().unwrap().id,9);assert_eq!(line_version(&conn,1).unwrap(),7);let legacy=active_snapshot(&conn,9).unwrap().unwrap();assert!(legacy.events[0].summary.contains("won"));assert_eq!(legacy.events[0].key,"1:battle");assert!(!extraction_matches(&conn,9,&config()).unwrap());assert_eq!(coverage(&conn,1).unwrap().legacy_chapters,vec![1]);
    assert_eq!(legacy.nodes[0].life_status.as_deref(),Some("dead"));assert!(legacy.edges[0].description.as_deref().unwrap().contains("broken"));assert!(!ready(&conn,9).unwrap());
    let(before,after)=conn.query_row("SELECT before_json,after_json FROM edits_log WHERE id=41",[],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).unwrap();let before:GraphEdge=parse(&before).unwrap();let after:GraphEdge=parse(&after).unwrap();assert_eq!(before.key,"old-rel");assert!(before.description.unwrap().contains("broken"));assert!(after.description.unwrap().contains("former"));
    assert_eq!(conn.query_row("SELECT legacy FROM kg_lite_specs WHERE id=51",[],|r|r.get::<_,i64>(0)).unwrap(),1);assert_eq!(conn.query_row("SELECT count(*) FROM kg_extraction_plans",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
#[test]
fn same_text_reextract_preserves_edit_targets_and_all_state_references() {
    let(conn,book)=fixture();let text=text_version(&conn,book,1,"original",None).unwrap();let mut old=snapshot(1);
    let mut city=old.nodes[0].clone();city.key="city".into();city.entity_type="location".into();city.name="城".into();city.life_status=None;
    let mut item=city.clone();item.key="sword".into();item.entity_type="item".into();item.name="剑".into();item.location=Some("city".into());item.owner=Some("hero".into());old.nodes[0].location=Some("city".into());old.nodes.extend([city,item]);
    old.edges.push(GraphEdge{key:"old-rel".into(),src:"hero".into(),dst:"sword".into(),rel_type:"持有".into(),description:None,chapter_idx:1,provenance:"ai_original".into()});
    old.events.push(GraphEvent{key:"1:old-event".into(),title:"出发".into(),chapter_idx:1,order:0,participants:vec!["hero".into()],location:Some("city".into()),time:None,summary:"出发".into(),provenance:"ai_original".into()});
    commit_snapshot(&conn,&text,&old,"dep",0,None,&config()).unwrap();mark_read(&conn,book,1).unwrap();let hero=public_key(book,"n","hero");let edit_id=edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some(hero.clone()),action:"upsert".into(),data:serde_json::json!({"life_status":"dead"})}).unwrap();
    let mut fresh=old.clone();for n in &mut fresh.nodes{n.key=format!("new-{}",n.key);n.location=n.location.as_ref().map(|s|format!("new-{s}"));n.owner=n.owner.as_ref().map(|s|format!("new-{s}"));}fresh.edges[0].key="new-rel".into();fresh.edges[0].src="new-hero".into();fresh.edges[0].dst="new-sword".into();fresh.events[0].key="new-event".into();fresh.events[0].participants=vec!["new-hero".into()];fresh.events[0].location=Some("new-city".into());
    commit_snapshot(&conn,&text,&fresh,"dep",line_version(&conn,book).unwrap(),None,&config()).unwrap();let kept=active_snapshot(&conn,text.id).unwrap().unwrap();assert_eq!(kept,old);assert_eq!(active_edits(&conn,book).unwrap()[0].target_id,hero);revert(&conn,book,edit_id).unwrap();
}
#[test]
fn generated_cache_and_undo_do_not_mutate_archived_prose() {
    let(conn,book)=fixture();commit(&conn,book,1);mark_read(&conn,book,1).unwrap();let key=public_key(book,"n","hero");let id=edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some(key),action:"upsert".into(),data:serde_json::json!({"life_status":"dead"})}).unwrap();
    let line=line_version(&conn,book).unwrap();let text=save_generated(&conn,book,2,"selected new prose","writer","dep",line).unwrap();let mut generated=snapshot(2);generated.nodes[0].provenance="ai_generated".into();commit_snapshot(&conn,&text,&generated,"dep",line,None,&config()).unwrap();mark_read(&conn,book,2).unwrap();assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().id,text.id);
    revert(&conn,book,id).unwrap();assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().source,"original");assert_eq!(by_id(&conn,text.id).unwrap().content,"selected new prose");assert!(activate_cached_for_config(&conn,text.id,"dep",line,&config()).is_err());
}
#[test]
fn tombstones_negative_directives_and_opaque_auth_survive_cutover() {
    let(conn,book)=fixture();commit(&conn,book,1);mark_read(&conn,book,1).unwrap();let input=EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some("hero".into()),action:"delete".into(),data:serde_json::json!({})};assert!(edit(&conn,&input).is_err());let mut deletion=input;deletion.key=Some(public_key(book,"n","hero"));edit(&conn,&deletion).unwrap();edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),key:None,action:"upsert".into(),data:serde_json::json!({"text":"不要复活甲","polarity":"negative"})}).unwrap();let constraints=intent_constraints(&conn,book,2).unwrap();assert_eq!(constraints["tombstones"][0]["key"],"hero");assert_eq!(constraints["directives"][0]["polarity"],"negative");assert!(internal_view(&conn,&query(book,"spec",2)).unwrap().nodes.is_empty());
}
#[test]
fn migrating_v1_keeps_active_prose_and_complete_archived_graph_metadata() {
    let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();conn.execute_batch(include_str!("../../../../migrations/0001_init.sql")).unwrap();conn.pragma_update(None,"user_version",1).unwrap();
    conn.execute("INSERT INTO books(id,title,source_path,format) VALUES (1,'old','test','txt')",[]).unwrap();conn.execute("INSERT INTO chapters(book_id,idx,title,original_text) VALUES (1,1,'one','original')",[]).unwrap();conn.execute("INSERT INTO generated_chapters(id,book_id,chapter_idx,content,model,prompt_digest,active) VALUES (7,1,1,'unchanged chosen prose','writer','dep',1)",[]).unwrap();
    conn.execute("INSERT INTO graph_nodes(book_id,scope,chapter_idx,key,type,name,provenance) VALUES (1,'chapter',1,'old-hero','character','甲','ai_original')",[]).unwrap();conn.execute("INSERT INTO graph_events(book_id,scope,chapter_idx,order_in_chapter,title,summary,outcome,provenance) VALUES (1,'chapter',1,3,'旧战役','战斗','胜利','ai_original')",[]).unwrap();
    Db::new(&conn).migrate().unwrap();assert_eq!(conn.query_row("SELECT content FROM generated_chapters WHERE id=7 AND active=1",[],|r|r.get::<_,String>(0)).unwrap(),"unchanged chosen prose");
    let data=conn.query_row("SELECT data_json FROM kg_legacy_archive WHERE source_table='graph_events'",[],|r|r.get::<_,String>(0)).unwrap();let value:Value=parse(&data).unwrap();assert_eq!(value["chapter_idx"],1);assert_eq!(value["order"],3);assert_eq!(value["outcome"],"胜利");assert_eq!(conn.query_row("SELECT count(*) FROM jobs",[],|r|r.get::<_,i64>(0)).unwrap(),0);
}
#[test]
fn null_latest_snapshot_preserves_confirmed_state_but_future_null_edits_clear_it() {
    let(conn,book)=fixture();let first=text_version(&conn,book,1,"original",None).unwrap();let mut established=snapshot(1);established.nodes[0].aliases=vec!["旧别名".into()];established.nodes[0].summary=Some("已确认的描述".into());established.nodes[0].location=Some("city".into());
    let mut city=established.nodes[0].clone();city.key="city".into();city.entity_type="location".into();city.name="城".into();city.aliases.clear();city.life_status=None;city.location=None;city.summary=None;
    let mut item=city.clone();item.key="item".into();item.entity_type="item".into();item.name="剑".into();item.location=Some("city".into());item.owner=Some("hero".into());established.nodes.extend([city,item]);commit_snapshot(&conn,&first,&established,"dep",0,None,&config()).unwrap();
    let second=text_version(&conn,book,2,"original",None).unwrap();let mut latest=established.clone();for n in &mut latest.nodes{n.chapter_idx=2;n.life_status=None;n.location=None;n.owner=None;n.summary=None;n.aliases.clear();}latest.nodes[0].aliases=vec!["新别名".into()];commit_snapshot(&conn,&second,&latest,"dep",0,None,&config()).unwrap();mark_read(&conn,book,2).unwrap();
    let actual=internal_view(&conn,&query(book,"actual",2)).unwrap();let hero=actual.nodes.iter().find(|n|n.key=="hero").unwrap();let item=actual.nodes.iter().find(|n|n.key=="item").unwrap();assert_eq!(hero.life_status.as_deref(),Some("alive"));assert_eq!(hero.location.as_deref(),Some("city"));assert_eq!(hero.summary.as_deref(),Some("已确认的描述"));assert_eq!(hero.aliases,vec!["旧别名","新别名"]);assert_eq!(item.location.as_deref(),Some("city"));assert_eq!(item.owner.as_deref(),Some("hero"));assert!(active_snapshot(&conn,second.id).unwrap().unwrap().nodes[0].life_status.is_none());
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some(public_key(book,"n","hero")),action:"upsert".into(),data:serde_json::json!({"life_status":null,"location":null,"aliases":[]})}).unwrap();
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),key:Some(public_key(book,"n","item")),action:"upsert".into(),data:serde_json::json!({"owner":null,"location":null})}).unwrap();
    let spec=internal_view(&conn,&query(book,"spec",3)).unwrap();let future_hero=spec.nodes.iter().find(|n|n.key=="hero").unwrap();let future_item=spec.nodes.iter().find(|n|n.key=="item").unwrap();assert!(future_hero.life_status.is_none());assert!(future_hero.location.is_none());assert!(future_hero.aliases.is_empty());assert!(future_item.location.is_none());assert!(future_item.owner.is_none());
    let unchanged=internal_view(&conn,&query(book,"actual",2)).unwrap();assert_eq!(unchanged.nodes,actual.nodes);
}
#[test]
fn original_jump_only_materializes_target_but_generated_jump_pins_chosen_chain() {
    let(conn,book)=fixture();mark_read(&conn,book,4).unwrap();assert_eq!(progress(&conn,book).unwrap(),4);assert_eq!(conn.query_row("SELECT count(*) FROM kg_text_versions",[],|r|r.get::<_,i64>(0)).unwrap(),1);assert_eq!(conn.query_row("SELECT count(*) FROM kg_read_selection",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    let(conn,book)=fixture();mark_read(&conn,book,1).unwrap();edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),key:None,action:"upsert".into(),data:serde_json::json!({"text":"chosen story","polarity":"positive"})}).unwrap();let line=line_version(&conn,book).unwrap();let second=save_generated(&conn,book,2,"chosen two","writer","two",line).unwrap();let third=save_generated(&conn,book,3,"chosen three","writer","three",line).unwrap();mark_read(&conn,book,3).unwrap();assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().id,second.id);assert_eq!(selected_text(&conn,book,3).unwrap().unwrap().id,third.id);assert_eq!(conn.query_row("SELECT count(*) FROM kg_read_selection",[],|r|r.get::<_,i64>(0)).unwrap(),3);
    assert!(mark_read(&conn,book,4).is_err());assert_eq!(progress(&conn,book).unwrap(),3);
}
