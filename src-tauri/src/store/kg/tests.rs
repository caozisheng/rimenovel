use super::*;
use crate::store::{books::{create_book_with_chapters, NewChapter}, Db};

fn fixture() -> (Connection, i64) {
    let conn = Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();
    Db::new(&conn).migrate().unwrap();
    let chapters = (1..=4).map(|idx| NewChapter { idx, title: format!("Chapter {idx}"), text: "甲在城中。乙仍活着。".into(), token_est: 20 }).collect::<Vec<_>>();
    let book = create_book_with_chapters(&conn, "test", None, "txt", "test", &chapters).unwrap();
    (conn, book)
}
fn bundle(idx: i64) -> ChapterBundle {
    ChapterBundle {
        stage_a: StageA { schema_version: "kg-stage-a/v1".into(), chapter_idx: idx, summary: String::new(), open_threads: vec![], mentions: vec![], events: vec![] },
        stage_b: StageB { schema_version: "kg-stage-b/v1".into(), chapter_idx: idx, facts: vec![Fact { key: "name".into(), subject_key: "future-real-name".into(), predicate: "name".into(), value: TypedValue { kind: "scalar".into(), value: Some("甲".into()) }, chapter_idx: idx, observed_chapter: idx, observed_order: 0, temporal_scope: "current".into(), assertion_kind: "confirmed".into(), speaker_key: None, evidence_refs: vec!["s".into()] }], relations: vec![], changes: vec![], preconditions:vec![], unresolved: vec![] },
        identities: vec![Identity { key: "future-real-name".into(), entity_type: "character".into(), name: "未来真名".into(), aliases: vec!["未来别名".into()] }],
        spans: vec![Span { key: "s".into(), chapter_idx: idx, start_byte: 0, end_byte: 3, text: "甲".into() }], identity_links: vec![],
    }
}
fn query(book: i64, purpose: &str, idx: i64) -> GraphQuery { GraphQuery { book_id: book, scope: "global".into(), purpose: purpose.into(), chapter_idx: Some(idx) } }
#[test]
fn legacy_ai_projection_is_archived_without_becoming_fact_truth() {
    let conn=Connection::open_in_memory().unwrap();Db::apply_pragmas(&conn).unwrap();
    conn.execute_batch(include_str!("../../../../migrations/0001_init.sql")).unwrap();
    let id=crate::store::books::create_book_with_chapters(&conn,"legacy",None,"txt","legacy",&[NewChapter{idx:1,title:"one".into(),text:"text".into(),token_est:1}]).unwrap();
    conn.execute("INSERT INTO graph_nodes(book_id,scope,key,type,name,aliases_json,provenance) VALUES (?1,'global','old','character','old','[]','ai_original')",[id]).unwrap();
    Db::new(&conn).migrate().unwrap();
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM graph_nodes WHERE book_id=?1",[id],|r|r.get::<_,i64>(0)).unwrap(),0);
    assert_eq!(conn.query_row("SELECT reason FROM kg_legacy_archive WHERE book_id=?1",[id],|r|r.get::<_,String>(0)).unwrap(),"needs-reextract");
    assert_eq!(missing_original_chapters(&conn,id).unwrap(),vec![1]);
}
#[test]
fn revision_replay_exact_missing_set_and_public_names() {
    let (conn, book) = fixture();
    let t = text_version(&conn, book, 2, "original", None).unwrap();
    let a = commit_bundle(&conn, &t, &bundle(2), "d", 0, None).unwrap();
    assert_eq!(a, commit_bundle(&conn, &t, &bundle(2), "d", 0, None).unwrap());
    assert_eq!(missing_original_chapters(&conn, book).unwrap(), vec![1,3,4]);
    mark_read(&conn, book, 2).unwrap();
    let view = get_view(&conn, &query(book, "actual", 2)).unwrap();
    assert_eq!(view.nodes[0].name, "甲");
    let json = serde_json::to_string(&view).unwrap();
    assert!(!json.contains("future-real-name"));
    assert!(!json.contains("未来"));
}
#[test]
fn failed_job_checkpoint_rolls_back_every_authoritative_record() {
    let (conn, book) = fixture();
    let t = text_version(&conn, book, 1, "original", None).unwrap();
    assert!(commit_bundle(&conn, &t, &bundle(1), "d", 0, Some(999)).is_err());
    assert!(!ready(&conn, t.id).unwrap());
    assert_eq!(conn.query_row("SELECT COUNT(*) FROM kg_records", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
}
#[test]
fn future_intent_never_mutates_actual_and_revert_restores_digest() {
    let (conn, book) = fixture();
    let t = text_version(&conn, book, 1, "original", None).unwrap();
    commit_bundle(&conn, &t, &bundle(1), "d", 0, None).unwrap();
    mark_read(&conn, book, 1).unwrap();
    let before = generation_context(&conn, book, 2).unwrap();
    let key = get_view(&conn, &query(book,"actual",1)).unwrap().nodes[0].key.clone();
    let edit_id = edit(&conn, &EditInput { book_id: book, scope: "global".into(), chapter_idx: None, kind: "node".into(), action: "upsert".into(), key: Some(key), data: serde_json::json!({"name":"新名字"}) }).unwrap();
    assert_eq!(get_view(&conn, &query(book,"actual",1)).unwrap().nodes[0].name, "甲");
    assert_ne!(generation_context(&conn,book,2).unwrap().digest, before.digest);
    revert(&conn, book, edit_id).unwrap();
    assert_eq!(generation_context(&conn,book,2).unwrap().digest, before.digest);
}
#[test]
fn hidden_spec_reveal_is_target_only_and_evidence_authorized() {
    let (conn, book) = fixture();
    for idx in 1..=4 { let t = text_version(&conn,book,idx,"original",None).unwrap(); commit_bundle(&conn,&t,&bundle(idx),"d",0,None).unwrap(); }
    mark_read(&conn,book,1).unwrap();
    let hidden = get_view(&conn,&query(book,"spec",2)).unwrap();
    assert!(hidden.nodes.is_empty());
    assert!(hidden.fog.is_some());
    assert!(reveal(&conn,book,3).is_err());
    reveal(&conn,book,2).unwrap();
    let revealed = get_view(&conn,&query(book,"spec",2)).unwrap();
    assert!(!revealed.nodes.is_empty());
    assert!(get_view(&conn,&query(book,"reference",4)).unwrap().nodes.iter().all(|n| n.chapter_idx <= 1));
    assert!(evidence(&conn,&query(book,"actual",1),"node","n-hidden").is_err());
}
#[test]
fn immutable_versions_and_stale_generated_commits_are_rejected() {
    let (conn,book) = fixture();
    let original = text_version(&conn,book,1,"original",None).unwrap();
    assert!(conn.execute("UPDATE kg_text_versions SET content='changed' WHERE id=?1",[original.id]).is_err());
    mark_read(&conn,book,1).unwrap();
    edit(&conn,&EditInput { book_id:book, scope:"global".into(), chapter_idx:None, kind:"directive".into(), action:"upsert".into(), key:None, data:serde_json::json!({"text":"继续"}) }).unwrap();
    assert!(save_generated(&conn,book,2,"new","model","digest",0).is_err());
    let generated = save_generated(&conn,book,2,"new","model","digest",1).unwrap();
    let mut b = bundle(2); b.spans[0].text="n".into(); b.spans[0].end_byte=1;
    edit(&conn,&EditInput { book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"change"}) }).unwrap();
    assert!(commit_bundle(&conn,&generated,&b,"digest",1,None).is_err());
    assert!(selected_text(&conn,book,2).unwrap().is_none());
}

#[test]
fn reverting_prior_intent_reselects_original_for_read_generated_chapter() {
    let (conn,book)=fixture();
    let first=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&first,&bundle(1),"d",0,None).unwrap();
    mark_read(&conn,book,1).unwrap();
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})}).unwrap();
    let generated=save_generated(&conn,book,2,"generated text","m","dep",1).unwrap();let mut second=bundle(2);second.spans[0].text="generated".into();second.spans[0].end_byte=9;commit_bundle(&conn,&generated,&second,"dep",1,None).unwrap();mark_read(&conn,book,2).unwrap();
    assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().source,"generated");
    revert(&conn,book,1).unwrap();
    assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().source,"original");
    assert_eq!(progress(&conn,book).unwrap(),2);
}

#[test]
fn identical_generated_bytes_have_separate_dependency_owned_versions() {
    let (conn,book)=fixture();mark_read(&conn,book,1).unwrap();
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})}).unwrap();
    let a=save_generated(&conn,book,2,"identical","m","dep-a",1).unwrap();
    let b=save_generated(&conn,book,2,"identical","m","dep-b",1).unwrap();
    assert_eq!(a.content_hash,b.content_hash);assert_ne!(a.id,b.id);assert_ne!(a.generated_id,b.generated_id);
}

#[test]
fn independent_same_category_relations_preserve_intermediate_history() {
    let (conn,book)=fixture();
    for idx in 1..=3{
        let mut b=bundle(idx);b.identities.push(Identity{key:"b".into(),entity_type:"character".into(),name:"乙".into(),aliases:vec![]});
        b.spans.push(Span{key:"s2".into(),chapter_idx:idx,start_byte:15,end_byte:18,text:"乙".into()});
        b.stage_b.facts.push(Fact{key:"b-name".into(),subject_key:"b".into(),predicate:"name".into(),value:TypedValue{kind:"scalar".into(),value:Some("乙".into())},chapter_idx:idx,observed_chapter:idx,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),speaker_key:None,evidence_refs:vec!["s2".into()]});
        for key in ["family-a","family-b"]{b.stage_b.relations.push(RelationObservation{key:key.into(),src:"future-real-name".into(),dst:"b".into(),rel_category:"family".into(),rel_type:key.into(),status:if idx==2{"broken"}else{"active"}.into(),description:None,chapter_idx:idx,observed_chapter:idx,observed_order:1,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into(),"s2".into()]});}
        let t=text_version(&conn,book,idx,"original",None).unwrap();commit_bundle(&conn,&t,&b,"d",0,None).unwrap();
    }
    mark_read(&conn,book,3).unwrap();
    for (idx,status) in [(1,"active"),(2,"broken"),(3,"active")]{let graph=get_view(&conn,&query(book,"actual",idx)).unwrap();assert_eq!(graph.edges.len(),2);assert!(graph.edges.iter().all(|e|e.status==status));assert_ne!(graph.edges[0].key,graph.edges[1].key);}
}

#[test]
fn identity_links_are_revealed_only_at_evidenced_position() {
    let (conn,book)=fixture();
    let mut first=bundle(1);first.identities.push(Identity{key:"masked".into(),entity_type:"character".into(),name:"蒙面人".into(),aliases:vec![]});
    let mut name=first.stage_b.facts[0].clone();name.key="mask-name".into();name.subject_key="masked".into();name.value.value=Some("蒙面人".into());first.stage_b.facts.push(name);
    let t=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&t,&first,"d",0,None).unwrap();
    let mut second=bundle(2);second.identity_links.push(IdentityLink{from_key:"masked".into(),to_key:"future-real-name".into(),observed_chapter:2,observed_order:1,evidence_refs:vec!["s".into()]});
    let t=text_version(&conn,book,2,"original",None).unwrap();commit_bundle(&conn,&t,&second,"d",0,None).unwrap();mark_read(&conn,book,2).unwrap();
    let early=get_view(&conn,&query(book,"actual",1)).unwrap();assert_eq!(early.nodes.len(),2);
    let later=get_view(&conn,&query(book,"actual",2)).unwrap();assert_eq!(later.nodes.len(),1);
    assert!(early.nodes.iter().all(|n|!n.key.contains("masked")&&!n.key.contains("future")));
}

#[test]
fn foreign_identity_and_non_boundary_evidence_cannot_commit() {
    let (conn,book)=fixture();let text=text_version(&conn,book,1,"original",None).unwrap();
    let mut invalid=bundle(1);invalid.spans[0].end_byte=1;assert!(commit_bundle(&conn,&text,&invalid,"d",0,None).is_err());
    let mut invalid=bundle(1);invalid.stage_b.facts[0].subject_key="other-book-only".into();assert!(commit_bundle(&conn,&text,&invalid,"d",0,None).is_err());
    assert!(!ready(&conn,text.id).unwrap());
}

fn config()->RuntimeConfig{RuntimeConfig{provider_id:1,model_extract:"model".into(),context_limit:32768,output_reserve:4096,prompt_version:"test-1".into()}}
#[test]
fn refinement_invalidates_exact_descendant_and_cache_matches_prior_content() {
    let (conn,book)=fixture();let cfg=config();
    for idx in 1..=3{let t=text_version(&conn,book,idx,"original",None).unwrap();commit_bundle_with_config(&conn,&t,&bundle(idx),"d",0,None,&cfg).unwrap();}
    let second=text_version(&conn,book,2,"original",None).unwrap();assert!(extraction_matches(&conn,second.id,&cfg).unwrap());
    let first=text_version(&conn,book,1,"original",None).unwrap();let mut improved=bundle(1);improved.stage_a.summary="refined".into();commit_bundle_with_config(&conn,&first,&improved,"d2",0,None,&cfg).unwrap();
    assert!(!extraction_matches(&conn,second.id,&cfg).unwrap());assert_eq!(missing_original_chapters(&conn,book).unwrap(),vec![2,3,4]);
}
#[test]
fn cached_prose_activation_reuses_only_compatible_same_text_revision() {
    let (conn,book)=fixture();let cfg=config();let first=text_version(&conn,book,1,"original",None).unwrap();commit_bundle_with_config(&conn,&first,&bundle(1),"d",0,None,&cfg).unwrap();mark_read(&conn,book,1).unwrap();
    let directive=EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})};
    let edit_id=edit(&conn,&directive).unwrap();let generated=save_generated(&conn,book,2,"generated text","m","dep",1).unwrap();let mut b=bundle(2);b.spans[0].text="generated".into();b.spans[0].end_byte=9;commit_bundle_with_config(&conn,&generated,&b,"extract-dep",1,None,&cfg).unwrap();
    revert(&conn,book,edit_id).unwrap();edit(&conn,&directive).unwrap();
    assert!(activate_cached_for_config(&conn,generated.id,"wrong",3,&cfg).is_err());
    activate_cached_for_config(&conn,generated.id,"dep",3,&cfg).unwrap();assert!(ready(&conn,generated.id).unwrap());
    let mut changed=cfg.clone();changed.prompt_version="new".into();activate_cached_for_config(&conn,generated.id,"dep",3,&changed).unwrap();assert!(!ready(&conn,generated.id).unwrap());
}

#[test]
fn adapted_preconditions_persist_with_spec_and_never_promote_to_actual() {
    let (conn,book)=fixture();let first=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&first,&bundle(1),"d",0,None).unwrap();mark_read(&conn,book,1).unwrap();
    let mut spec=internal_view(&conn,&GraphQuery{book_id:book,scope:"chapter".into(),purpose:"spec".into(),chapter_idx:Some(2)}).unwrap();
    spec.nodes=internal_view(&conn,&query(book,"actual",1)).unwrap().nodes;
    spec.events.push(GraphEvent{key:"expected-event".into(),title:"Future intent".into(),chapter_idx:2,order:1,participants:vec!["future-real-name".into()],location:None,summary:"".into(),outcome:"".into(),provenance:"ai_adapted".into()});
    let conditions=vec![Precondition{key:"condition".into(),event_key:"expected-event".into(),subject_key:"future-real-name".into(),predicate:"life_status".into(),value:TypedValue{kind:"unknown".into(),value:None},assertion_kind:"confirmed".into(),evidence_refs:vec![]}];
    save_spec_with_preconditions(&conn,book,2,&spec,&conditions,"spec-dep",0).unwrap();assert_eq!(spec_preconditions(&conn,book,2).unwrap(),conditions);
    assert!(get_view(&conn,&query(book,"actual",1)).unwrap().events.is_empty());
    let edit_id=edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})}).unwrap();assert!(spec_preconditions(&conn,book,2).unwrap().is_empty());
    assert!(spec_matches(&conn,book,2,"spec-dep",0).is_err(), "old task cannot reactivate spec after reading-line change");
    assert!(spec_preconditions(&conn,book,2).unwrap().is_empty());
    revert(&conn,book,edit_id).unwrap();assert!(spec_matches(&conn,book,2,"spec-dep",line_version(&conn,book).unwrap()).unwrap());assert_eq!(spec_preconditions(&conn,book,2).unwrap(),conditions);
}

#[test]
fn original_refinement_invalidates_generated_descendants_and_read_selection() {
    let (conn,book)=fixture();let first=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&first,&bundle(1),"d",0,None).unwrap();mark_read(&conn,book,1).unwrap();
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})}).unwrap();
    let generated=save_generated(&conn,book,2,"generated text","m","dep",1).unwrap();let mut b=bundle(2);b.spans[0].text="generated".into();b.spans[0].end_byte=9;commit_bundle(&conn,&generated,&b,"extract",1,None).unwrap();mark_read(&conn,book,2).unwrap();
    let mut improved=bundle(1);improved.stage_a.summary="refinement".into();commit_bundle(&conn,&first,&improved,"new",1,None).unwrap();
    assert!(selected_text(&conn,book,2).unwrap().is_none());assert!(!ready(&conn,generated.id).unwrap());
}

#[test]
fn replacing_read_generated_text_switches_pin_and_invalidates_later_selection() {
    let (conn,book)=fixture();mark_read(&conn,book,1).unwrap();edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"branch"})}).unwrap();
    let old=save_generated(&conn,book,2,"old text","m","old",1).unwrap();let later=save_generated(&conn,book,3,"later text","m","later",1).unwrap();mark_read(&conn,book,3).unwrap();
    let replacement=save_generated(&conn,book,2,"new text","new-model","new",1).unwrap();assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().id,replacement.id);assert_ne!(old.id,replacement.id);assert!(selected_text(&conn,book,3).unwrap().is_none());
    activate_cached(&conn,old.id,"old",1).unwrap();assert_eq!(selected_text(&conn,book,2).unwrap().unwrap().id,old.id);assert!(selected_text(&conn,book,3).unwrap().is_none());assert!(!ready(&conn,later.id).unwrap());
}

#[test]
fn negative_directive_and_tombstone_are_explicit_generation_constraints() {
    let (conn,book)=fixture();let first=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&first,&bundle(1),"d",0,None).unwrap();mark_read(&conn,book,1).unwrap();
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"directive".into(),action:"upsert".into(),key:None,data:serde_json::json!({"text":"甲死亡","polarity":"negative"})}).unwrap();
    let key=get_view(&conn,&query(book,"actual",1)).unwrap().nodes[0].key.clone();edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),action:"delete".into(),key:Some(key),data:serde_json::json!({})}).unwrap();
    let constraints=intent_constraints(&conn,book,2).unwrap();assert_eq!(constraints["directives"][0]["polarity"],"negative");assert_eq!(constraints["tombstones"][0]["key"],"future-real-name");assert_eq!(constraints["tombstones"][0]["data"]["name"],"甲");
    let context=generation_context(&conn,book,2).unwrap();assert!(context.directives[0].contains("negative"));
}

#[test]
fn user_node_intent_never_inherits_original_fact_evidence() {
    let (conn,book)=fixture();for idx in 1..=2{let t=text_version(&conn,book,idx,"original",None).unwrap();commit_bundle(&conn,&t,&bundle(idx),"d",0,None).unwrap();}mark_read(&conn,book,1).unwrap();reveal(&conn,book,2).unwrap();
    let key=get_view(&conn,&query(book,"spec",2)).unwrap().nodes[0].key.clone();assert!(!evidence(&conn,&query(book,"spec",2),"node",&key).unwrap().is_empty());
    edit(&conn,&EditInput{book_id:book,scope:"global".into(),chapter_idx:None,kind:"node".into(),action:"upsert".into(),key:Some(key.clone()),data:serde_json::json!({"name":"Edited future intent"})}).unwrap();
    assert!(evidence(&conn,&query(book,"spec",2),"node",&key).unwrap().is_empty());assert!(!evidence(&conn,&query(book,"actual",1),"node",&key).unwrap().is_empty());
}

#[test]
fn established_relation_status_change_requires_no_duplicate_observation() {
    let (conn,book)=fixture();let mut first=bundle(1);first.stage_b.relations.push(RelationObservation{key:"relationship".into(),src:"future-real-name".into(),dst:"future-real-name".into(),rel_category:"alliance".into(),rel_type:"alliance".into(),status:"active".into(),description:None,chapter_idx:1,observed_chapter:1,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]});
    let t=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&t,&first,"d",0,None).unwrap();
    let mut second=bundle(2);second.stage_a.events.push(ExtractEvent{key:"break".into(),title:"Break".into(),category:"parting".into(),chapter_idx:2,observed_chapter:2,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),participants:vec!["future-real-name".into()],location:None,summary:"".into(),outcome:"".into(),cause_keys:vec![],evidence_refs:vec!["s".into()]});
    second.stage_b.changes.push(Change{key:"break-status".into(),event_key:"break".into(),subject_key:"relationship".into(),predicate:"status".into(),operation:"set".into(),before:TypedValue{kind:"scalar".into(),value:Some("active".into())},after:TypedValue{kind:"scalar".into(),value:Some("broken".into())},chapter_idx:2,observed_chapter:2,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]});
    second.stage_b.preconditions.push(Precondition{key:"prior-status".into(),event_key:"break".into(),subject_key:"relationship".into(),predicate:"status".into(),value:TypedValue{kind:"scalar".into(),value:Some("active".into())},assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]});
    let t=text_version(&conn,book,2,"original",None).unwrap();commit_bundle(&conn,&t,&second,"d",0,None).unwrap();mark_read(&conn,book,2).unwrap();
    assert_eq!(get_view(&conn,&query(book,"actual",2)).unwrap().edges[0].status,"broken");assert_eq!(get_view(&conn,&query(book,"actual",1)).unwrap().edges[0].status,"active");
}

#[test]
fn identity_merge_projects_target_name_and_summary_deterministically() {
    let (conn,book)=fixture();let mut first=bundle(1);first.identities.push(Identity{key:"masked".into(),entity_type:"character".into(),name:"蒙面人".into(),aliases:vec![]});let mut f=first.stage_b.facts[0].clone();f.key="mask-name".into();f.subject_key="masked".into();f.value.value=Some("蒙面人".into());first.stage_b.facts.push(f);
    let t=text_version(&conn,book,1,"original",None).unwrap();commit_bundle(&conn,&t,&first,"d",0,None).unwrap();let mut second=bundle(2);second.identity_links.push(IdentityLink{from_key:"masked".into(),to_key:"future-real-name".into(),observed_chapter:2,observed_order:0,evidence_refs:vec!["s".into()]});let t=text_version(&conn,book,2,"original",None).unwrap();commit_bundle(&conn,&t,&second,"d",0,None).unwrap();mark_read(&conn,book,2).unwrap();
    let baseline=serde_json::to_string(&get_view(&conn,&query(book,"actual",2)).unwrap()).unwrap();for _ in 0..20{let view=get_view(&conn,&query(book,"actual",2)).unwrap();assert_eq!(view.nodes.len(),1);assert_eq!(view.nodes[0].name,"甲");assert_eq!(serde_json::to_string(&view).unwrap(),baseline);}
}
