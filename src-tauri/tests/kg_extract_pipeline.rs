use rimenovel_lib::core::kg::{GraphNode,TextVersion};
use rimenovel_lib::core::kg_extract::decode_graph;
use serde_json::{json,Value};

fn text()->TextVersion {TextVersion{id:1,book_id:7,chapter_idx:3,source:"original".into(),content:"阿岚抵达渡口。".into(),content_hash:"immutable-content".into(),generated_id:None}}
fn known()->GraphNode {GraphNode {key:"existing-arlan".into(),entity_type:"character".into(),name:"阿岚".into(),aliases:vec![],summary:None,life_status:Some("alive".into()),location:None,owner:None,chapter_idx:1,provenance:"ai_original".into()}}
fn reply(reference:&str)->Value {json!({"summary":"阿岚抵达渡口","entities":[{"ref":reference,"type":"character","name":"阿岚","aliases":[],"description":"游侠","life_status":null,"location":null,"owner":null}],"relations":[],"events":[{"title":"抵达","participants":[reference],"location":null,"time":null,"summary":"旅人抵达渡口"}],"open_threads":[]})}
#[test]
fn supplied_short_reference_reuses_identity_but_same_name_is_not_forced_to_merge(){
    let known=known();
    let linked=decode_graph(reply("k0"),&text(),std::slice::from_ref(&known),"ai_original").unwrap();
    assert_eq!(linked.nodes[0].key,known.key);
    assert_eq!(linked.events[0].participants,vec![known.key.clone()]);
    let separate=decode_graph(reply("new-person"),&text(),std::slice::from_ref(&known),"ai_original").unwrap();
    assert_ne!(separate.nodes[0].key,known.key);
    assert_eq!(separate.events[0].participants,vec![separate.nodes[0].key.clone()]);
}
#[test]
fn foreign_or_unprovided_references_cannot_form_a_persistable_graph(){
    let mut value=reply("a");value["events"][0]["participants"]=json!(["foreign"]);
    assert!(decode_graph(value,&text(),&[],"ai_original").is_err());
    assert!(decode_graph(reply("k0"),&text(),&[],"ai_original").is_err());
}
#[test]
fn model_cannot_supply_runtime_ownership_or_invalid_story_state(){
    let mut value=reply("a");value["entities"][0]["provenance"]=json!("user");
    assert!(decode_graph(value,&text(),&[],"ai_original").is_err());
    value=reply("a");value["entities"][0]["life_status"]=json!("broken");
    assert!(decode_graph(value,&text(),&[],"ai_original").is_err());
}
#[test]
fn new_working_spec_identity_stays_future_owned(){
    let snapshot=decode_graph(reply("a"),&text(),&[],"ai_adapted").unwrap();
    assert!(snapshot.nodes[0].key.starts_with("spec-"));
    assert_eq!(snapshot.nodes[0].provenance,"ai_adapted");
    assert_eq!(snapshot.events[0].participants,vec![snapshot.nodes[0].key.clone()]);
}
