use super::{kg, Db};
use crate::core::kg::{public_key, ChapterSnapshot, GraphQuery};
use rusqlite::{params, Connection};
use serde_json::{json, Value};

fn legacy_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();
    conn.execute_batch(include_str!("../../../migrations/0001_init.sql")).unwrap();
    conn.execute_batch(include_str!("../../../migrations/0002_kg_facts.sql")).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();
    conn.execute("INSERT INTO books(id,title,source_path,format) VALUES (1,'legacy','test','txt')", []).unwrap();
    for idx in 1..=4 {
        conn.execute("INSERT INTO chapters(book_id,idx,title,original_text) VALUES (1,?1,'chapter',?2)", params![idx, format!("original {idx}")]).unwrap();
    }
    conn.execute("INSERT INTO kg_lines(book_id,progress,version) VALUES (1,2,7)", []).unwrap();
    conn
}
fn revision(conn: &Connection, id: i64, idx: i64, source: &str, selected: bool, active: bool, unresolved: Value) {
    let generated = if source == "generated" {
        conn.execute("INSERT INTO generated_chapters(id,book_id,chapter_idx,content,model,prompt_digest,active,line_version) VALUES (?1,1,?2,?3,'writer','dep',?4,7)", params![id, idx, format!("generated {id}"), selected]).unwrap();
        Some(id)
    } else { None };
    let content=if source=="original" {format!("original {idx}")} else {format!("generated {id}")};
    conn.execute("INSERT INTO kg_text_versions(id,book_id,chapter_idx,source,content,content_hash,generated_id) VALUES (?1,1,?2,?3,?4,'hash',?5)", params![id, idx, source, content, generated]).unwrap();
    if selected {
        conn.execute("INSERT INTO kg_read_selection VALUES (1,?1,?2)", params![idx,id]).unwrap();
    }
    conn.execute("INSERT INTO kg_revisions(id,book_id,text_version_id,digest,dependency_digest,stage_a_version,stage_b_version,summary,open_threads_json,unresolved_json,active,schema_digest) VALUES (?1,1,?1,?2,'dep','a','b','legacy summary','[]',?3,?4,'old')", params![id,format!("digest-{id}"),unresolved.to_string(),active]).unwrap();
}
fn record(conn: &Connection, revision: i64, kind: &str, key: &str, idx: i64, order: i64, value: Value) {
    conn.execute("INSERT INTO kg_records(revision_id,kind,key,subject_key,observed_chapter,observed_order,data_json) VALUES (?1,?2,?3,?4,?5,?6,?7)", params![revision,kind,key,value.get("subject_key").and_then(Value::as_str),idx,order,value.to_string()]).unwrap();
}
fn identity(conn: &Connection, rev: i64, idx: i64, key: &str, kind: &str) {
    record(conn,rev,"identity",key,idx,0,json!({"key":key,"entity_type":kind,"name":key,"aliases":[]}));
}
fn fact(key: &str, subject: &str, predicate: &str, value: &str, idx: i64, order: i64) -> Value {
    json!({"key":key,"subject_key":subject,"predicate":predicate,"value":{"kind":if ["location","owner"].contains(&predicate) {"entity_ref"} else {"scalar"},"value":value},"chapter_idx":idx,"observed_chapter":idx,"observed_order":order,"temporal_scope":"current","assertion_kind":"confirmed"})
}
fn event(conn: &Connection, rev: i64, idx: i64, participants: Value, location: Value) {
    let value=json!({"key":"event","title":"event","chapter_idx":idx,"observed_chapter":idx,"observed_order":0,"participants":participants,"location":location,"summary":"occurred","outcome":"known"});
    conn.execute("INSERT INTO graph_events(book_id,scope,chapter_idx,order_in_chapter,title,provenance,revision_id,record_key,observed_chapter,observed_order,data_json) VALUES (1,'chapter',?1,0,'event','ai_original',?2,'event',?1,0,?3)",params![idx,rev,value.to_string()]).unwrap();
}
fn query(purpose: &str, idx: i64) -> GraphQuery {
    GraphQuery {book_id:1,scope:"global".into(),purpose:purpose.into(),chapter_idx:Some(idx)}
}
fn snapshot(conn: &Connection, id: i64) -> ChapterSnapshot {
    let data:String=conn.query_row("SELECT data_json FROM kg_lite_snapshots WHERE id=?1",[id],|r|r.get(0)).unwrap();
    serde_json::from_str(&data).unwrap()
}

#[test]
fn migration_legacy_noncurrent_and_unresolved_observations_do_not_confirm_death() {
    for (temporal,assertion,blocked) in [("historical","confirmed",false),("current","reported",false),("current","inferred",false),("unspecified","confirmed",false),("current","hypothetical",false),("current","confirmed",true)] {
        for kind in ["fact","change"] {
            let conn=legacy_db();
            revision(&conn,1,1,"original",true,true,json!([]));
            revision(&conn,2,2,"original",true,true,if blocked {json!(["death"])} else {json!([])});
            identity(&conn,1,1,"hero","character");
            record(&conn,1,"fact","alive",1,0,fact("alive","hero","life_status","alive",1,0));
            let mut death=fact("death","hero","life_status","dead",2,1);
            death["temporal_scope"]=json!(temporal);death["assertion_kind"]=json!(assertion);
            if kind=="change" {death["operation"]=json!("set");death["before"]=json!({"kind":"scalar","value":"alive"});death["after"]=death["value"].take();}
            record(&conn,2,kind,"death",2,1,death);
            let raw:String=conn.query_row("SELECT data_json FROM kg_records WHERE revision_id=2 AND key='death'",[],|r|r.get(0)).unwrap();
            Db::new(&conn).migrate().unwrap();
            let actual=kg::get_view(&conn,&query("actual",2)).unwrap();
            assert_eq!(actual.nodes.iter().find(|n|n.key==public_key(1,"n","hero")).unwrap().life_status.as_deref(),Some("alive"),"{temporal}/{assertion}/{blocked}/{kind}");
            assert_eq!(kg::generation_context(&conn,1,3).unwrap().actual.nodes[0].life_status.as_deref(),Some("alive"));
            assert_eq!(conn.query_row("SELECT data_json FROM kg_records WHERE revision_id=2 AND key='death'",[],|r|r.get::<_,String>(0)).unwrap(),raw);
        }
    }
}

#[test]
fn migration_generated_known_refs_follow_only_selected_prior_lineage() {
    let conn=legacy_db();
    revision(&conn,10,1,"original",true,true,json!([]));identity(&conn,10,1,"hero","character");
    record(&conn,10,"fact","alive",1,0,fact("alive","hero","life_status","alive",1,0));
    revision(&conn,20,2,"original",false,true,json!([]));identity(&conn,20,2,"original-only","character");
    record(&conn,20,"fact","original-name",2,0,fact("original-name","hero","name","unselected original",2,0));
    revision(&conn,21,2,"generated",false,true,json!([]));identity(&conn,21,2,"other-branch","character");
    revision(&conn,22,2,"generated",true,true,json!([]));
    record(&conn,22,"fact","death",2,0,fact("death","hero","life_status","dead",2,0));
    record(&conn,22,"relation","known-rel",2,1,json!({"src":"hero","dst":"hero","rel_type":"known","temporal_scope":"current","assertion_kind":"confirmed"}));event(&conn,22,2,json!(["hero"]),Value::Null);
    revision(&conn,30,3,"original",false,true,json!([]));identity(&conn,30,3,"future-original","character");
    revision(&conn,31,3,"generated",false,false,json!([]));record(&conn,31,"fact","return",3,0,fact("return","hero","life_status","alive",3,0));
    Db::new(&conn).migrate().unwrap();
    let converted=snapshot(&conn,22);assert_eq!(converted.nodes.iter().find(|n|n.key=="hero").unwrap().life_status.as_deref(),Some("dead"));assert_eq!(converted.edges[0].src,"hero");assert_eq!(converted.events[0].participants,vec!["hero"]);
    assert!(converted.nodes.iter().all(|n|!["other-branch","original-only","future-original"].contains(&n.key.as_str())));
    let inactive=snapshot(&conn,31);assert_eq!(inactive.nodes.iter().find(|n|n.key=="hero").unwrap().life_status.as_deref(),Some("alive"));assert!(inactive.nodes.iter().all(|n|n.key!="other-branch"&&n.key!="future-original"));
    let public=kg::get_view(&conn,&query("actual",2)).unwrap();assert_eq!(public.nodes.len(),1);assert_eq!(public.nodes[0].life_status.as_deref(),Some("dead"));assert_eq!(public.events[0].participants,vec![public_key(1,"n","hero")]);
    let context=kg::generation_context(&conn,1,3).unwrap();assert_eq!(context.actual.nodes[0].life_status.as_deref(),Some("dead"));assert_eq!(context.actual.events[0].participants,vec!["hero"]);
    assert_eq!(kg::selected_text(&conn,1,2).unwrap().unwrap().id,22);assert_eq!(kg::line_version(&conn,1).unwrap(),7);assert!(!kg::ready(&conn,22).unwrap());
}

#[test]
fn migration_revealed_links_canonicalize_refs_and_edit_targets_without_future_leak() {
    let conn=legacy_db();conn.execute("UPDATE kg_lines SET progress=1 WHERE book_id=1",[]).unwrap();
    revision(&conn,1,1,"original",true,true,json!([]));
    for (key,kind) in [("masked","character"),("masked-city","location"),("sword","item")] {identity(&conn,1,1,key,kind);}
    record(&conn,1,"fact","owner",1,0,fact("owner","sword","owner","masked",1,0));record(&conn,1,"fact","place",1,0,fact("place","sword","location","masked-city",1,0));
    record(&conn,1,"relation","held",1,1,json!({"src":"masked","dst":"sword","rel_type":"holds","temporal_scope":"current","assertion_kind":"confirmed"}));event(&conn,1,1,json!(["masked"]),json!("masked-city"));
    revision(&conn,2,2,"original",true,true,json!([]));identity(&conn,2,2,"real","character");identity(&conn,2,2,"real-city","location");
    for (from,to) in [("masked","real"),("masked-city","real-city")] {record(&conn,2,"link",from,2,0,json!({"from_key":from,"to_key":to,"observed_chapter":2,"observed_order":0}));}
    revision(&conn,3,3,"original",true,true,json!([]));
    record(&conn,3,"fact","future-owner",3,0,fact("future-owner","sword","owner","real",3,0));
    revision(&conn,4,4,"original",false,true,json!([]));identity(&conn,4,4,"future","character");record(&conn,4,"link","future-link",4,0,json!({"from_key":"real","to_key":"future","observed_chapter":4,"observed_order":0}));
    for (id,kind,target) in [(41,"node","real"),(42,"edge","held"),(43,"event","1:event")] {conn.execute("INSERT INTO edits_log(id,book_id,target_type,target_id,action,effective_from,scope,active) VALUES (?1,1,?2,?3,'delete',3,'global',1)",params![id,kind,target]).unwrap();}
    Db::new(&conn).migrate().unwrap();
    let before=kg::get_view(&conn,&query("actual",1)).unwrap();assert!(before.nodes.iter().all(|n|n.key!=public_key(1,"n","real")));assert_eq!(before.nodes.iter().find(|n|n.key==public_key(1,"n","sword")).unwrap().owner,Some(public_key(1,"n","masked")));assert_eq!(before.events[0].participants,vec![public_key(1,"n","masked")]);
    conn.execute("UPDATE kg_lines SET progress=2 WHERE book_id=1",[]).unwrap();
    let actual=kg::get_view(&conn,&query("actual",2)).unwrap();assert!(actual.nodes.iter().all(|n|![public_key(1,"n","masked"),public_key(1,"n","masked-city"),public_key(1,"n","future")].contains(&n.key)));
    let sword=actual.nodes.iter().find(|n|n.key==public_key(1,"n","sword")).unwrap();assert_eq!(sword.owner,Some(public_key(1,"n","real")));assert_eq!(sword.location,Some(public_key(1,"n","real-city")));assert_eq!(actual.edges[0].src,public_key(1,"n","real"));assert_eq!(actual.edges[0].key,public_key(1,"r","held"));assert_eq!(actual.events[0].participants,vec![public_key(1,"n","real")]);assert_eq!(actual.events[0].location,Some(public_key(1,"n","real-city")));assert_eq!(actual.events[0].key,public_key(1,"e","1:event"));
    let context=kg::generation_context(&conn,1,3).unwrap();assert_eq!(context.actual.nodes.iter().find(|n|n.key=="sword").unwrap().owner.as_deref(),Some("real"));assert_eq!(context.actual.events[0].participants,vec!["real"]);assert!(!context.spec.nodes.iter().any(|n|n.key=="real"||n.key=="masked"));assert!(context.spec.nodes.iter().find(|n|n.key=="sword").unwrap().owner.is_none());assert!(context.spec.edges.is_empty());assert!(context.spec.events.is_empty());
    let edits=kg::active_edits(&conn,1).unwrap();assert_eq!(edits[0].target_id,public_key(1,"n","real"));assert_eq!(edits[1].target_id,public_key(1,"r","held"));assert_eq!(edits[2].target_id,public_key(1,"e","1:event"));
    assert_eq!(conn.query_row("SELECT count(*) FROM kg_records WHERE kind='link'",[],|r|r.get::<_,i64>(0)).unwrap(),3);assert_eq!(kg::selected_text(&conn,1,1).unwrap().unwrap().id,1);assert_eq!(kg::line_version(&conn,1).unwrap(),7);
}

#[test]
fn migration_legacy_lite_legacy_unknown_preserves_newer_confirmed_state() {
    let conn=legacy_db();
    revision(&conn,1,1,"original",true,true,json!([]));identity(&conn,1,1,"hero","character");
    record(&conn,1,"fact","alive",1,0,fact("alive","hero","life_status","alive",1,0));
    revision(&conn,4,4,"original",false,true,json!([]));
    let mut unknown=fact("unknown","hero","life_status","alive",4,0);unknown["value"]=json!({"kind":"unknown","value":null});
    record(&conn,4,"fact","unknown",4,0,unknown);
    Db::new(&conn).migrate().unwrap();
    let text=kg::text_version(&conn,1,2,"original",None).unwrap();
    let mut current=snapshot(&conn,1);current.nodes[0].chapter_idx=2;current.nodes[0].life_status=Some("dead".into());
    let mut added=current.nodes[0].clone();added.key="lite-only".into();added.name="lite-only".into();current.nodes.push(added);
    let config=crate::core::kg::RuntimeConfig {provider_id:1,model_extract:"model".into(),context_limit:32768,output_reserve:4096,prompt_version:"kg-lite/v1".into()};
    kg::commit_snapshot(&conn,&text,&current,"dep",7,None,&config).unwrap();
    conn.execute("UPDATE kg_lines SET progress=4 WHERE book_id=1",[]).unwrap();
    let actual=kg::get_view(&conn,&query("actual",4)).unwrap();
    assert_eq!(actual.nodes.iter().find(|n|n.key==public_key(1,"n","hero")).unwrap().life_status.as_deref(),Some("dead"));
    assert!(actual.nodes.iter().any(|n|n.key==public_key(1,"n","lite-only")));
}
