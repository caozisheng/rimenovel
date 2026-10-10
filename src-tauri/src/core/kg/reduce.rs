use super::*;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub struct ValueOrigin { pub chapter_idx:i64, pub record_key:String, pub kind:String }
#[derive(Clone, Debug, Default)]
pub struct ReducedState {
    pub values: BTreeMap<(String,String),TypedValue>,
    pub collections: BTreeMap<(String,String),Vec<TypedValue>>,
    pub relations: BTreeMap<String,RelationObservation>,
    pub conflicts: Vec<String>,
    pub value_origins:BTreeMap<(String,String),ValueOrigin>,
    pub relation_origins:BTreeMap<String,ValueOrigin>,
}
enum Observation<'a> { Fact(&'a Fact), Relation(&'a RelationObservation), Change(&'a Change) }
impl Observation<'_> {
    fn order(&self)->(i64,i64,u8,&str) { match self { Self::Fact(f)=>(f.observed_chapter,f.observed_order,1,&f.key),Self::Relation(r)=>(r.observed_chapter,r.observed_order,1,&r.key),Self::Change(c)=>(c.observed_chapter,c.observed_order,0,&c.key) } }
}
/// Replay observations at their reveal positions. Changes run before their resulting
/// observations at the same position; historical and hearsay never mutate hard state.
pub fn reduce_state(bundles:&[ChapterBundle],upto:i64)->ReducedState {
    let mut state=ReducedState::default();
    let mut observations=Vec::new();
    let mut blocked=HashSet::new();
    for b in bundles {
        for unresolved in &b.stage_b.unresolved { blocked.insert(unresolved.as_str()); }
        observations.extend(b.stage_b.facts.iter().filter(|f|f.observed_chapter<=upto && hard_assertion(&f.temporal_scope,&f.assertion_kind)).map(Observation::Fact));
        observations.extend(b.stage_b.relations.iter().filter(|r|r.observed_chapter<=upto && hard_assertion(&r.temporal_scope,&r.assertion_kind)).map(Observation::Relation));
        observations.extend(b.stage_b.changes.iter().filter(|c|c.observed_chapter<=upto && hard_assertion(&c.temporal_scope,&c.assertion_kind)).map(Observation::Change));
    }
    observations.sort_by(|a,b|a.order().cmp(&b.order()));
    let identity_map=revealed_identity_map(bundles,upto);
    let canonical=|key:&str|resolve_identity(&identity_map,key);
    let value=|input:&TypedValue|{let mut v=input.clone();if v.kind=="entity_ref"{v.value=v.value.as_deref().map(&canonical);}v};
    let canonical_names:HashSet<_>=bundles.iter().flat_map(|b|&b.stage_b.facts).filter(|f|f.predicate=="name"&&f.observed_chapter<=upto&&hard_assertion(&f.temporal_scope,&f.assertion_kind)&&!blocked.contains(f.key.as_str())&&canonical(&f.subject_key)==f.subject_key).map(|f|f.subject_key.clone()).collect();
    let mut seen:HashMap<(String,String,i64,i64),(TypedValue,String)>=HashMap::new();
    let mut changes_at:HashMap<(String,String,i64,i64),TypedValue>=HashMap::new();
    let mut blocked_positions=HashSet::new();
    for observation in observations {
        match observation {
            Observation::Change(c)=> {
                let subject=if c.predicate=="status"{c.subject_key.clone()}else{canonical(&c.subject_key)};
                let slot=(subject.clone(),c.predicate.clone());
                let at=(subject.clone(),c.predicate.clone(),c.observed_chapter,c.observed_order);
                if blocked.contains(c.key.as_str()) || blocked.contains(c.subject_key.as_str()) {blocked_positions.insert(at);continue;}
                let before=value(&c.before);let after=value(&c.after);
                let current=if c.predicate=="status" {state.relations.get(&subject).map(|r|TypedValue{kind:"scalar".into(),value:Some(r.status.clone())})}else{state.values.get(&slot).cloned()};
                let collection=c.predicate=="alias" || c.predicate=="affiliation";
                if !collection && before.kind!="unknown" && current.as_ref().is_some_and(|v|v!=&before) {
                    state.conflicts.push(c.key.clone());blocked_positions.insert(at);continue;
                }
                if collection {
                    let items=state.collections.entry(slot).or_default();
                    match c.operation.as_str() { "add"=>if !items.contains(&after) {items.push(after.clone());},"remove"=>items.retain(|v|v!=&before && v!=&after),_=>{} }
                } else {
                    if c.predicate=="status" {
                        if let Some(relation)=state.relations.get_mut(&subject) {relation.status=after.value.clone().unwrap_or_else(||"unknown".into());relation.observed_chapter=c.observed_chapter;relation.observed_order=c.observed_order;relation.chapter_idx=c.chapter_idx;state.relation_origins.insert(subject.clone(),ValueOrigin{chapter_idx:c.chapter_idx,record_key:c.key.clone(),kind:"change".into()});}
                    }
                    state.value_origins.insert(slot.clone(),ValueOrigin{chapter_idx:c.chapter_idx,record_key:c.key.clone(),kind:"change".into()});
                    state.values.insert(slot,after.clone());changes_at.insert(at,after);
                }
            }
            Observation::Fact(f)=> {
                if blocked.contains(f.key.as_str()) || blocked.contains(f.subject_key.as_str()) {continue;}
                let subject=canonical(&f.subject_key);let slot=(subject.clone(),f.predicate.clone());let at=(subject.clone(),f.predicate.clone(),f.observed_chapter,f.observed_order);
                if blocked_positions.contains(&at){continue;}
                if f.predicate=="name" && subject!=f.subject_key && canonical_names.contains(&subject){continue;}
                let observed=value(&f.value);
                if f.predicate=="alias" || f.predicate=="affiliation" {let items=state.collections.entry(slot).or_default();if !items.contains(&observed){items.push(observed);}continue;}
                if changes_at.get(&at).is_some_and(|v|v!=&observed)||seen.get(&at).is_some_and(|(v,raw)|raw==&f.subject_key&&v!=&observed){state.conflicts.push(f.key.clone());state.values.remove(&slot);state.value_origins.remove(&slot);continue;}
                seen.insert(at,(observed.clone(),f.subject_key.clone()));state.value_origins.insert(slot.clone(),ValueOrigin{chapter_idx:f.chapter_idx,record_key:f.key.clone(),kind:"fact".into()});state.values.insert(slot,observed);
            }
            Observation::Relation(r)=> {
                if blocked.contains(r.key.as_str()) || blocked_positions.contains(&(r.key.clone(),"status".into(),r.observed_chapter,r.observed_order)){continue;}
                let mut relation=r.clone();relation.src=canonical(&r.src);relation.dst=canonical(&r.dst);
                if let Some(value)=changes_at.get(&(r.key.clone(),"status".into(),r.observed_chapter,r.observed_order)){relation.status=value.value.clone().unwrap_or_else(||"unknown".into());}else{state.relation_origins.insert(r.key.clone(),ValueOrigin{chapter_idx:r.chapter_idx,record_key:r.key.clone(),kind:"relation".into()});}
                state.relations.insert(r.key.clone(),relation);
            }
        }
    }
    state
}
/// Identity equivalence is itself a revealed assertion, never a global registry merge.
pub fn revealed_identity_map(bundles:&[ChapterBundle],upto:i64)->HashMap<String,String> {
    let mut map=HashMap::new();
    let mut links=bundles.iter().flat_map(|b|&b.identity_links).filter(|l|l.observed_chapter<=upto).collect::<Vec<_>>();
    links.sort_by_key(|l|(l.observed_chapter,l.observed_order));
    for link in links {
        let target=resolve_identity(&map,&link.to_key);
        let from=resolve_identity(&map,&link.from_key);
        if from!=target {map.insert(from,target);}
    }
    map
}
pub fn resolve_identity(map:&HashMap<String,String>,key:&str)->String {
    let mut current=key; let mut visited=HashSet::new();
    while let Some(next)=map.get(current) {if !visited.insert(current) {break;} current=next;}
    current.into()
}
pub fn canonicalize_state(state:&ReducedState,map:&HashMap<String,String>)->ReducedState{
    let canonical=|key:&str|resolve_identity(map,key);
    let value=|input:&TypedValue|{let mut v=input.clone();if v.kind=="entity_ref"{v.value=v.value.as_deref().map(&canonical);}v};
    let mut result=ReducedState{conflicts:state.conflicts.clone(),..ReducedState::default()};
    let mut values=state.values.iter().collect::<Vec<_>>();values.sort_by_key(|((subject,predicate),_)|(canonical(subject),predicate.clone(),canonical(subject)==*subject,subject.clone()));
    for ((subject,predicate),observed) in values{let slot=(canonical(subject),predicate.clone());result.values.insert(slot.clone(),value(observed));if let Some(origin)=state.value_origins.get(&(subject.clone(),predicate.clone())){result.value_origins.insert(slot,origin.clone());}}
    for ((subject,predicate),items) in &state.collections{let selected=result.collections.entry((canonical(subject),predicate.clone())).or_default();for item in items{let item=value(item);if !selected.contains(&item){selected.push(item);}}}
    for (key,relation) in &state.relations{let mut relation=relation.clone();relation.src=canonical(&relation.src);relation.dst=canonical(&relation.dst);result.relations.insert(key.clone(),relation);}
    result.relation_origins=state.relation_origins.clone();result
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fact(key:&str,chapter:i64,order:i64,value:&str)->Fact { Fact {key:key.into(),subject_key:"a".into(),predicate:"life_status".into(),value:TypedValue{kind:"scalar".into(),value:Some(value.into())},chapter_idx:chapter,observed_chapter:chapter,observed_order:order,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),speaker_key:None,evidence_refs:vec!["s".into()]} }
    fn bundle(facts:Vec<Fact>,changes:Vec<Change>)->ChapterBundle { ChapterBundle {stage_a:StageA{schema_version:"kg-stage-a/v1".into(),chapter_idx:1,summary:"".into(),open_threads:vec![],mentions:vec![],events:vec![]},stage_b:StageB{schema_version:"kg-stage-b/v1".into(),chapter_idx:1,facts,relations:vec![],changes,preconditions:vec![],unresolved:vec![]},identities:vec![],spans:vec![],identity_links:vec![]} }
    #[test]
    fn historical_reported_and_absence_do_not_overwrite_current() {
        let mut historical=fact("old",2,0,"dead"); historical.temporal_scope="historical".into();
        let mut reported=fact("rumor",3,0,"dead"); reported.assertion_kind="reported".into();
        let state=reduce_state(&[bundle(vec![fact("alive",1,0,"alive"),historical,reported],vec![])],4);
        assert_eq!(state.values[&("a".into(),"life_status".into())].value.as_deref(),Some("alive"));
    }
    #[test]
    fn explicit_change_precedes_same_position_result_not_false_conflict() {
        let change=Change{key:"death".into(),event_key:"e".into(),subject_key:"a".into(),predicate:"life_status".into(),operation:"set".into(),before:TypedValue{kind:"scalar".into(),value:Some("alive".into())},after:TypedValue{kind:"scalar".into(),value:Some("dead".into())},chapter_idx:2,observed_chapter:2,observed_order:1,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]};
        let b=bundle(vec![fact("start",1,0,"alive"),fact("after",2,1,"dead")],vec![change]);
        assert_eq!(reduce_state(&[b.clone()],1).values[&("a".into(),"life_status".into())].value.as_deref(),Some("alive"));
        let end=reduce_state(&[b],2); assert!(end.conflicts.is_empty()); assert_eq!(end.values[&("a".into(),"life_status".into())].value.as_deref(),Some("dead"));
    }
    #[test]
    fn unknown_and_none_are_distinct_and_unsupported_collection_set_rejected() { assert_ne!(TypedValue{kind:"unknown".into(),value:None},TypedValue{kind:"none".into(),value:None}); assert!(validate_operation("affiliation","set").is_err()); }
    #[test]
    fn unresolved_before_change_blocks_same_position_result_observation() {
        let change=Change{key:"change".into(),event_key:"e".into(),subject_key:"a".into(),predicate:"life_status".into(),operation:"set".into(),before:TypedValue{kind:"scalar".into(),value:Some("unknown-status".into())},after:TypedValue{kind:"scalar".into(),value:Some("dead".into())},chapter_idx:2,observed_chapter:2,observed_order:1,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]};
        let b=bundle(vec![fact("start",1,0,"alive"),fact("result",2,1,"dead")],vec![change]);
        let state=reduce_state(&[b.clone()],2);assert_eq!(state.values[&("a".into(),"life_status".into())].value.as_deref(),Some("alive"));assert!(state.conflicts.contains(&"change".into()));
        let mut unresolved=b;unresolved.stage_b.unresolved.push("change".into());let state=reduce_state(&[unresolved],2);assert_eq!(state.values[&("a".into(),"life_status".into())].value.as_deref(),Some("alive"));
    }
    #[test]
    fn identity_reveal_canonicalizes_subject_and_entity_ref_continuity() {
        let mut owner=fact("owner",1,0,"masked");owner.subject_key="sword".into();owner.predicate="owner".into();owner.value.kind="entity_ref".into();
        let mut b=bundle(vec![owner],vec![Change{key:"transfer".into(),event_key:"event".into(),subject_key:"sword".into(),predicate:"owner".into(),operation:"set".into(),before:TypedValue{kind:"entity_ref".into(),value:Some("real".into())},after:TypedValue{kind:"entity_ref".into(),value:Some("other".into())},chapter_idx:3,observed_chapter:3,observed_order:0,temporal_scope:"current".into(),assertion_kind:"confirmed".into(),evidence_refs:vec!["s".into()]}]);
        b.identity_links.push(IdentityLink{from_key:"masked".into(),to_key:"real".into(),observed_chapter:2,observed_order:0,evidence_refs:vec!["s".into()]});
        assert_eq!(reduce_state(&[b.clone()],1).values[&("sword".into(),"owner".into())].value.as_deref(),Some("masked"));
        assert_eq!(reduce_state(&[b.clone()],2).values[&("sword".into(),"owner".into())].value.as_deref(),Some("real"));
        let state=reduce_state(&[b],3);assert!(state.conflicts.is_empty());assert_eq!(state.values[&("sword".into(),"owner".into())].value.as_deref(),Some("other"));
    }
}
