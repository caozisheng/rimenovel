use super::*;
use std::collections::{HashMap, HashSet};
pub const ENTITY_TYPES: &[&str] = &["character","location","item","faction","concept"];
pub const PREDICATES: &[&str] = &["name","alias","description","life_status","location","affiliation","owner"];
pub const RELATION_CATEGORIES:&[&str]=&["family","romantic","friendship","enmity","alliance","master_student","subordination","rivalry","other"];
pub const EVENT_CATEGORIES:&[&str]=&["conflict","battle","meeting","parting","journey","discovery","betrayal","deal","loss","death","romance","mystery","other"];
pub fn hard_assertion(temporal:&str,assertion:&str)->bool { temporal=="current" && assertion=="confirmed" }
fn one_of(value:&str,allowed:&[&str],what:&str)->Result<(),String> { if allowed.contains(&value) { Ok(()) } else { Err(format!("invalid {what}: {value}")) } }
pub fn validate_value(value:&TypedValue)->Result<(),String> {
    one_of(&value.kind,&["entity_ref","scalar","unknown","none"],"value kind")?;
    if matches!(value.kind.as_str(),"entity_ref"|"scalar") != value.value.is_some() { return Err("typed value payload does not match kind".into()); }
    if value.value.as_ref().is_some_and(|v|v.is_empty()) { return Err("empty typed value".into()); }
    Ok(())
}
pub fn validate_operation(predicate:&str,operation:&str)->Result<(),String> {
    let allowed = match predicate { "alias"|"affiliation"=> &["add","remove"][..], "name"|"description"|"life_status"|"location"|"owner"|"status"=> &["set","clear"][..], _=>return Err("unsupported predicate".into()) };
    one_of(operation,allowed,"predicate operation")
}
fn position(idx:i64,observed:i64,order:i64,text:&TextVersion,temporal:&str,assertion:&str)->Result<(),String> {
    if idx!=text.chapter_idx || observed!=text.chapter_idx || order<0 { return Err("record chapter or order outside owning text".into()); }
    one_of(temporal,&["current","historical","unspecified"],"temporal scope")?;
    one_of(assertion,&["confirmed","reported","inferred"],"assertion kind")
}
fn unique<'a>(keys:impl Iterator<Item=&'a str>,what:&str)->Result<(),String> { let mut seen=HashSet::new(); for key in keys { if key.is_empty() || !seen.insert(key) { return Err(format!("duplicate/empty {what} key")); } } Ok(()) }
pub fn validate_bundle(text:&TextVersion,bundle:&ChapterBundle,known:&[Identity],prior_relations:&[RelationObservation])->Result<(),String> {
    if text.chapter_idx<1 || !["original","generated"].contains(&text.source.as_str()) || digest(text.content.as_bytes())!=text.content_hash { return Err("invalid immutable text version".into()); }
    if bundle.stage_a.chapter_idx!=text.chapter_idx || bundle.stage_b.chapter_idx!=text.chapter_idx || bundle.stage_a.schema_version!="kg-stage-a/v1" || bundle.stage_b.schema_version!="kg-stage-b/v1" { return Err("stage chapter/schema mismatch".into()); }
    unique(bundle.spans.iter().map(|x|x.key.as_str()),"span")?;
    unique(bundle.identities.iter().map(|x|x.key.as_str()),"identity")?;
    unique(bundle.stage_a.mentions.iter().map(|x|x.mention_id.as_str()),"mention")?;
    unique(bundle.stage_a.events.iter().map(|x|x.key.as_str()),"event")?;
    unique(bundle.stage_b.facts.iter().map(|x|x.key.as_str()),"fact")?;
    unique(bundle.stage_b.relations.iter().map(|x|x.key.as_str()),"relation")?;
    unique(bundle.stage_b.changes.iter().map(|x|x.key.as_str()),"change")?;
    unique(bundle.stage_b.preconditions.iter().map(|x|x.key.as_str()),"precondition")?;
    let spans:HashSet<_>=bundle.spans.iter().map(|s|s.key.as_str()).collect();
    let evidence=|refs:&[String]|->Result<(),String>{ if refs.is_empty() || refs.iter().any(|s|!spans.contains(s.as_str())) { Err("missing or foreign evidence reference".into()) } else { Ok(()) } };
    for span in &bundle.spans { if span.chapter_idx!=text.chapter_idx || span.start_byte>=span.end_byte || text.content.get(span.start_byte..span.end_byte)!=Some(span.text.as_str()) { return Err("invalid UTF-8 evidence range or text".into()); } }
    let mut entities:HashMap<&str,&str>=known.iter().map(|i|(i.key.as_str(),i.entity_type.as_str())).collect();
    for identity in &bundle.identities { one_of(&identity.entity_type,ENTITY_TYPES,"entity type")?; if identity.name.trim().is_empty() { return Err("empty identity name".into()); } if entities.get(identity.key.as_str()).is_some_and(|kind|*kind!=identity.entity_type) { return Err("identity type mutation".into()); } entities.insert(&identity.key,&identity.entity_type); }
    let entity=|key:&str|->Result<(),String>{ if entities.contains_key(key) { Ok(()) } else { Err("missing or foreign entity reference".into()) } };
    let value=|v:&TypedValue|->Result<(),String>{ validate_value(v)?; if v.kind=="entity_ref" { entity(v.value.as_deref().unwrap())?; } Ok(()) };
    for mention in &bundle.stage_a.mentions { one_of(&mention.entity_type,ENTITY_TYPES,"mention entity type")?; evidence(&mention.evidence_refs)?; for key in &mention.candidate_keys { entity(key)?; } }
    let events:HashMap<_,_>=bundle.stage_a.events.iter().map(|e|(e.key.as_str(),e)).collect();
    for event in &bundle.stage_a.events { position(event.chapter_idx,event.observed_chapter,event.observed_order,text,&event.temporal_scope,&event.assertion_kind)?; one_of(&event.category,EVENT_CATEGORIES,"event category")?; evidence(&event.evidence_refs)?; for key in &event.participants { entity(key)?; } if let Some(key)=&event.location { entity(key)?; } for key in &event.cause_keys { let cause=events.get(key.as_str()).ok_or("foreign/missing cause event")?; if cause.observed_order>=event.observed_order { return Err("event cause must precede effect".into()); } } }
    for fact in &bundle.stage_b.facts { position(fact.chapter_idx,fact.observed_chapter,fact.observed_order,text,&fact.temporal_scope,&fact.assertion_kind)?; one_of(&fact.predicate,PREDICATES,"fact predicate")?; entity(&fact.subject_key)?; if let Some(speaker)=&fact.speaker_key { entity(speaker)?; } value(&fact.value)?; predicate_value(&fact.predicate,&fact.value)?; evidence(&fact.evidence_refs)?; }
    let mut relation_keys=HashSet::new();
    for relation in prior_relations {entity(&relation.src)?;entity(&relation.dst)?;if relation.observed_chapter>=text.chapter_idx{return Err("prior relation outside predecessor boundary".into());}relation_keys.insert(relation.key.as_str());}
    for relation in &bundle.stage_b.relations { position(relation.chapter_idx,relation.observed_chapter,relation.observed_order,text,&relation.temporal_scope,&relation.assertion_kind)?; entity(&relation.src)?; entity(&relation.dst)?; if relation.rel_type.is_empty() { return Err("empty relation type".into()); } one_of(&relation.rel_category,RELATION_CATEGORIES,"relation category")?; one_of(&relation.status,&["active","broken","former","unknown"],"relation status")?; evidence(&relation.evidence_refs)?; relation_keys.insert(relation.key.as_str()); }
    for change in &bundle.stage_b.changes { position(change.chapter_idx,change.observed_chapter,change.observed_order,text,&change.temporal_scope,&change.assertion_kind)?; validate_operation(&change.predicate,&change.operation)?; if change.predicate=="status" { if !relation_keys.contains(change.subject_key.as_str()) { return Err("missing relation change subject".into()); } } else { entity(&change.subject_key)?; } let event=events.get(change.event_key.as_str()).ok_or("missing change event")?; if change.observed_order<event.observed_order { return Err("change precedes associated event".into()); } value(&change.before)?; value(&change.after)?; predicate_value(&change.predicate,&change.before)?; predicate_value(&change.predicate,&change.after)?; if change.operation=="clear" && change.after.kind!="none" { return Err("clear requires explicit none".into()); } evidence(&change.evidence_refs)?; }
    for link in &bundle.identity_links { entity(&link.from_key)?; entity(&link.to_key)?; if link.from_key==link.to_key || entities[link.from_key.as_str()]!=entities[link.to_key.as_str()] || link.observed_chapter!=text.chapter_idx || link.observed_order<0 { return Err("invalid identity link".into()); } evidence(&link.evidence_refs)?; }
    for precondition in &bundle.stage_b.preconditions { if !events.contains_key(precondition.event_key.as_str()){return Err("missing precondition event".into());}one_of(&precondition.predicate,&["life_status","location","affiliation","owner","status"],"precondition predicate")?;if precondition.predicate=="status" {if !relation_keys.contains(precondition.subject_key.as_str()){return Err("missing precondition relation".into());}}else{entity(&precondition.subject_key)?;}value(&precondition.value)?;predicate_value(&precondition.predicate,&precondition.value)?;one_of(&precondition.assertion_kind,&["confirmed","reported","inferred"],"precondition assertion")?;evidence(&precondition.evidence_refs)?;}
    Ok(())
}
pub fn predicate_value(predicate:&str,value:&TypedValue)->Result<(),String> {
    if value.kind=="unknown" || value.kind=="none" { return Ok(()); }
    match predicate { "name"|"alias"|"description"|"life_status"|"status" if value.kind!="scalar"=>Err("predicate requires scalar value".into()), "location"|"affiliation"|"owner" if value.kind!="entity_ref"=>Err("predicate requires entity reference".into()), "status"=>one_of(value.value.as_deref().unwrap(),&["active","broken","former","unknown"],"relation status"), "life_status"=>one_of(value.value.as_deref().unwrap(),&["alive","dead","unknown"],"life status"), _=>Ok(()) }
}
