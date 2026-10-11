//! One-time local conversion. Facts tables are never queried by the lightweight runtime.
use crate::core::kg::{ChapterSnapshot,GraphNode,GraphEdge,GraphEvent,empty_view};
use rusqlite::{params,Connection};
use serde_json::Value;
use std::collections::{BTreeMap,BTreeSet};
// Migration 2 historically discarded graph metadata and active prose selection.
// Capture it locally before that archived migration runs; remove temp storage afterward.
pub(super) fn before_facts(conn:&Connection)->rusqlite::Result<()>{conn.execute_batch("CREATE TEMP TABLE kg_before_facts_graphs(source_table TEXT,source_id INTEGER,data_json TEXT); CREATE TEMP TABLE kg_before_facts_prose AS SELECT id FROM generated_chapters WHERE active=1;
INSERT INTO kg_before_facts_graphs SELECT 'graph_nodes',id,json_object('key',key,'type',type,'name',name,'aliases_json',aliases_json,'summary',summary,'provenance',provenance,'scope',scope,'chapter_idx',chapter_idx,'superseded_by',superseded_by) FROM graph_nodes WHERE provenance!='user';
INSERT INTO kg_before_facts_graphs SELECT 'graph_edges',id,json_object('src',src_key,'dst',dst_key,'rel_type',rel_type,'description',description,'status',status,'provenance',provenance,'scope',scope,'chapter_idx',chapter_idx,'since_chapter_idx',since_chapter_idx) FROM graph_edges WHERE provenance!='user';
INSERT INTO kg_before_facts_graphs SELECT 'graph_events',id,json_object('title',title,'participants',participant_keys_json,'location',location_key,'order',order_in_chapter,'summary',summary,'outcome',outcome,'provenance',provenance,'scope',scope,'chapter_idx',chapter_idx) FROM graph_events WHERE provenance!='user';")}
pub(super) fn after_facts(conn:&Connection)->rusqlite::Result<()>{conn.execute_batch("UPDATE kg_legacy_archive SET data_json=(SELECT g.data_json FROM kg_before_facts_graphs g WHERE g.source_table=kg_legacy_archive.source_table AND g.source_id=kg_legacy_archive.source_id) WHERE EXISTS(SELECT 1 FROM kg_before_facts_graphs g WHERE g.source_table=kg_legacy_archive.source_table AND g.source_id=kg_legacy_archive.source_id); UPDATE generated_chapters SET active=1 WHERE id IN (SELECT id FROM kg_before_facts_prose); DROP TABLE kg_before_facts_graphs; DROP TABLE kg_before_facts_prose;")}
fn failure(e:impl std::fmt::Display)->rusqlite::Error{rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::new(std::io::ErrorKind::InvalidData,e.to_string())))}
fn parse(s:&str)->rusqlite::Result<Value>{serde_json::from_str(s).map_err(failure)}
fn encode<T:serde::Serialize>(value:&T)->rusqlite::Result<String>{serde_json::to_string(value).map_err(failure)}
fn text(v:&Value,k:&str)->String{v.get(k).and_then(Value::as_str).unwrap_or_default().to_owned()}
fn optional(v:&Value,k:&str)->Option<String>{v.get(k).and_then(Value::as_str).map(str::to_owned)}
fn strings(v:&Value,k:&str)->Vec<String>{match v.get(k){Some(Value::Array(a))=>a.iter().filter_map(Value::as_str).map(str::to_owned).collect(),Some(Value::String(s))=>serde_json::from_str(s).unwrap_or_default(),_=>vec![]}}
fn merged(a:Option<String>,b:Option<String>,label:&str)->Option<String>{match (a,b.filter(|s|!s.is_empty())){(Some(a),Some(b)) if !a.is_empty()=>Some(format!("{a}\n{label}: {b}")),(_,Some(b))=>Some(format!("{label}: {b}")),(a,None)=>a}}
fn node(v:&Value,idx:i64,provenance:&str)->GraphNode{GraphNode{key:text(v,"key"),entity_type:optional(v,"type").or_else(||optional(v,"entity_type")).unwrap_or_else(||"concept".into()),name:text(v,"name"),aliases:if v.get("aliases").is_some(){strings(v,"aliases")}else{strings(v,"aliases_json")},summary:optional(v,"summary"),life_status:optional(v,"life_status"),location:optional(v,"location"),owner:optional(v,"owner"),chapter_idx:idx,provenance:provenance.into()}}
fn edge(v:&Value,key:String,idx:i64,provenance:&str)->GraphEdge{GraphEdge{key,src:optional(v,"src").or_else(||optional(v,"src_key")).unwrap_or_default(),dst:optional(v,"dst").or_else(||optional(v,"dst_key")).unwrap_or_default(),rel_type:text(v,"rel_type"),description:merged(optional(v,"description"),optional(v,"status"),"Legacy relation status"),chapter_idx:idx,provenance:provenance.into()}}
fn event(v:&Value,key:String,idx:i64,provenance:&str)->GraphEvent{GraphEvent{key,title:text(v,"title"),chapter_idx:idx,order:v.get("order").or_else(||v.get("observed_order")).or_else(||v.get("order_in_chapter")).and_then(Value::as_i64).unwrap_or(0),participants:strings(v,"participants"),location:optional(v,"location"),time:optional(v,"time"),summary:merged(optional(v,"summary"),optional(v,"outcome"),"Legacy outcome").unwrap_or_default(),provenance:provenance.into()}}
fn convert_record(kind:&str,v:Value,idx:i64)->rusqlite::Result<Value>{match kind{"node"=>serde_json::to_value(node(&v,idx,"user")).map_err(failure),"edge"=>serde_json::to_value(edge(&v,text(&v,"key"),idx,"user")).map_err(failure),"event"=>serde_json::to_value(event(&v,text(&v,"key"),idx,"user")).map_err(failure),_=>Ok(v)}}
// These migration-only values replay archived observations; no legacy engine survives
// in the lightweight runtime. Every revision is projected against its own prefix.
struct LegacyRecord {kind:String,key:String,chapter:i64,order:i64,value:Value}
struct LegacyRevision {
    id:i64,book:i64,text_id:i64,idx:i64,source:String,digest:String,dependency:String,
    summary:String,threads:Vec<String>,unresolved:Vec<String>,active:bool,complete:bool,
    selected:bool,records:Vec<LegacyRecord>,events:Vec<(String,Value)>,
}
fn resolve(map:&BTreeMap<String,String>,key:&str)->String{
    let mut current=key;let mut visited=BTreeSet::new();
    while let Some(next)=map.get(current){if !visited.insert(current){break;}current=next;}
    current.to_owned()
}
fn identity_map(revisions:&[&LegacyRevision],upto:i64)->BTreeMap<String,String>{
    let mut links=revisions.iter().flat_map(|r|&r.records).filter(|r|r.kind=="link"&&r.chapter<=upto).collect::<Vec<_>>();
    links.sort_by_key(|r|(r.chapter,r.order));let mut map=BTreeMap::new();
    for link in links{let from=resolve(&map,&text(&link.value,"from_key"));let to=resolve(&map,&text(&link.value,"to_key"));if from!=to{map.insert(from,to);}}
    map
}
fn canonical_value(value:&Value,map:&BTreeMap<String,String>)->Value{
    let mut value=value.clone();if text(&value,"kind")=="entity_ref"{if let Some(key)=value.get("value").and_then(Value::as_str){value["value"]=Value::String(resolve(map,key));}}value
}
fn hard_assertion(value:&Value)->bool{text(value,"temporal_scope")=="current"&&text(value,"assertion_kind")=="confirmed"}
fn selected_legacy_text(conn:&Connection,book:i64,idx:i64,text_id:i64,source:&str)->rusqlite::Result<bool>{
    // Match legacy selected_text without materializing any skipped immutable prose.
    conn.query_row("SELECT CASE WHEN EXISTS(SELECT 1 FROM kg_read_selection WHERE book_id=?1 AND chapter_idx=?2) THEN EXISTS(SELECT 1 FROM kg_read_selection WHERE book_id=?1 AND chapter_idx=?2 AND text_version_id=?3) WHEN ?2=1 OR NOT EXISTS(SELECT 1 FROM edits_log WHERE book_id=?1 AND active=1 AND effective_from<=?2) THEN ?4='original' ELSE ?3=coalesce((SELECT t.id FROM generated_chapters g JOIN kg_text_versions t ON t.generated_id=g.id WHERE g.book_id=?1 AND g.chapter_idx=?2 AND g.active=1 ORDER BY g.id DESC LIMIT 1),-1) END",params![book,idx,text_id,source],|r|r.get(0))
}
fn load_revisions(conn:&Connection)->rusqlite::Result<Vec<LegacyRevision>>{
    let mut stmt=conn.prepare("SELECT r.id,r.book_id,r.text_version_id,t.chapter_idx,t.source,r.digest,r.dependency_digest,r.summary,r.open_threads_json,r.unresolved_json,r.active,r.complete FROM kg_revisions r JOIN kg_text_versions t ON t.id=r.text_version_id ORDER BY r.book_id,t.chapter_idx,r.id")?;
    let mut revisions=stmt.query_map([],|r|Ok(LegacyRevision{id:r.get(0)?,book:r.get(1)?,text_id:r.get(2)?,idx:r.get(3)?,source:r.get(4)?,digest:r.get(5)?,dependency:r.get(6)?,summary:r.get(7)?,threads:serde_json::from_str(&r.get::<_,String>(8)?).map_err(failure)?,unresolved:serde_json::from_str(&r.get::<_,String>(9)?).map_err(failure)?,active:r.get(10)?,complete:r.get(11)?,selected:false,records:vec![],events:vec![]}))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for revision in &mut revisions{
        revision.selected=selected_legacy_text(conn,revision.book,revision.idx,revision.text_id,&revision.source)?;
        let mut stmt=conn.prepare("SELECT kind,key,observed_chapter,observed_order,data_json FROM kg_records WHERE revision_id=?1 ORDER BY observed_chapter,observed_order,id")?;
        revision.records=stmt.query_map([revision.id],|r|Ok(LegacyRecord{kind:r.get(0)?,key:r.get(1)?,chapter:r.get(2)?,order:r.get(3)?,value:parse(&r.get::<_,String>(4)?)?}))?.collect::<rusqlite::Result<Vec<_>>>()?;
        let mut stmt=conn.prepare("SELECT record_key,data_json FROM graph_events WHERE revision_id=?1 ORDER BY observed_order,id")?;
        revision.events=stmt.query_map([revision.id],|r|Ok((r.get(0)?,parse(&r.get::<_,String>(1)?)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    }Ok(revisions)
}
fn project_legacy(current:&LegacyRevision,prefix:&[&LegacyRevision])->(ChapterSnapshot,BTreeMap<String,String>,BTreeSet<String>){
    let prior_map=identity_map(prefix,current.idx-1);let mut lineage=prefix.to_vec();lineage.push(current);
    let map=identity_map(&lineage,current.idx);let canonical=|key:&str|resolve(&map,key);
    let blocked=lineage.iter().flat_map(|r|r.unresolved.iter().cloned()).collect::<BTreeSet<_>>();
    let mut qualifications=BTreeSet::new();let mut nodes=BTreeMap::<String,GraphNode>::new();
    let mut identities=lineage.iter().flat_map(|r|r.records.iter().filter(|o|o.kind=="identity").map(move |o|(*r,o))).collect::<Vec<_>>();
    // Target identity metadata wins over the retired disguise, as canonical name facts did.
    identities.sort_by_key(|(_,o)|canonical(&text(&o.value,"key"))==text(&o.value,"key"));
    for (revision,record) in identities{let provenance=if revision.source=="original"{"ai_original"}else{"ai_generated"};let mut n=node(&record.value,revision.idx,provenance);n.key=canonical(&n.key);n.life_status=None;n.location=None;n.owner=None;nodes.insert(n.key.clone(),n);}
    let mut observations=lineage.iter().flat_map(|r|r.records.iter().map(move |o|(*r,o))).filter(|(_,o)|matches!(o.kind.as_str(),"fact"|"change"|"relation")&&o.chapter<=current.idx).collect::<Vec<_>>();
    observations.sort_by(|(_,a),(_,b)|(a.chapter,a.order,if a.kind=="change"{0}else{1},&a.key).cmp(&(b.chapter,b.order,if b.kind=="change"{0}else{1},&b.key)));
    let mut values=BTreeMap::<(String,String),(Value,i64)>::new();let mut collections=BTreeMap::<(String,String),Vec<(Value,i64)>>::new();
    let mut relations=BTreeMap::<String,(GraphEdge,i64,String)>::new();
    let mut seen=BTreeMap::<(String,String,i64,i64),(Value,String)>::new();let mut changes_at=BTreeMap::<(String,String,i64,i64),Value>::new();let mut blocked_positions=BTreeSet::new();
    let canonical_names=lineage.iter().flat_map(|r|&r.records).filter(|o|o.kind=="fact"&&text(&o.value,"predicate")=="name"&&o.chapter<=current.idx&&hard_assertion(&o.value)&&!blocked.contains(&o.key)&&canonical(&text(&o.value,"subject_key"))==text(&o.value,"subject_key")).map(|o|text(&o.value,"subject_key")).collect::<BTreeSet<_>>();
    for (revision,record) in observations{
        let v=&record.value;let raw_subject=text(v,"subject_key");let predicate=text(v,"predicate");
        if !hard_assertion(v){qualifications.insert(format!("{}:noncurrent-or-unconfirmed",record.key));continue;}
        let subject=if predicate=="status"{raw_subject.clone()}else{canonical(&raw_subject)};
        let slot=(subject.clone(),predicate.clone());let at=(subject.clone(),predicate.clone(),record.chapter,record.order);
        let blocked_record=blocked.contains(&record.key)||(record.kind!="relation"&&blocked.contains(&raw_subject));
        if blocked_record{qualifications.insert(format!("{}:unresolved",record.key));if record.kind=="change"{blocked_positions.insert(at);}continue;}
        if record.kind=="relation"{
            if blocked_positions.contains(&(record.key.clone(),"status".into(),record.chapter,record.order)){continue;}
            let provenance=if revision.source=="original"{"ai_original"}else{"ai_generated"};let mut e=edge(v,record.key.clone(),revision.idx,provenance);e.src=canonical(&e.src);e.dst=canonical(&e.dst);
            let status=changes_at.get(&(record.key.clone(),"status".into(),record.chapter,record.order)).and_then(|v|optional(v,"value")).unwrap_or_else(||text(v,"status"));
            if status!=text(v,"status"){e.description=merged(optional(v,"description"),Some(status.clone()),"Legacy relation status");}
            relations.insert(record.key.clone(),(e,revision.id,status));continue;
        }
        let collection=predicate=="alias"||predicate=="affiliation";
        if record.kind=="change"{
            let before=canonical_value(v.get("before").unwrap_or(&Value::Null),&map);let after=canonical_value(v.get("after").unwrap_or(&Value::Null),&map);
            let previous=if predicate=="status"{relations.get(&subject).map(|(_,_,s)|serde_json::json!({"kind":"scalar","value":s}))}else{values.get(&slot).map(|(v,_)|v.clone())};
            if !collection&&text(&before,"kind")!="unknown"&&!before.is_null()&&previous.as_ref().is_some_and(|value|value!=&before){qualifications.insert(format!("{}:conflicting-before-state",record.key));blocked_positions.insert(at);continue;}
            if collection{let items=collections.entry(slot).or_default();match text(v,"operation").as_str(){"add"=>if !items.iter().any(|(value,_)|value==&after){items.push((after,revision.id));},"remove"=>items.retain(|(value,_)|value!=&before&&value!=&after),_=>{}}}
            else{if predicate=="status"{if let Some((e,origin,status))=relations.get_mut(&subject){*status=optional(&after,"value").unwrap_or_else(||"unknown".into());e.description=merged(e.description.take(),Some(status.clone()),"Legacy relation status");e.chapter_idx=revision.idx;*origin=revision.id;}}values.insert(slot,(after.clone(),revision.id));changes_at.insert(at,after);}
        }else{
            if blocked_positions.contains(&at){qualifications.insert(format!("{}:blocked-change-result",record.key));continue;}
            if predicate=="name"&&subject!=raw_subject&&canonical_names.contains(&subject){continue;}
            let observed=canonical_value(v.get("value").unwrap_or(&Value::Null),&map);
            if collection{let items=collections.entry(slot).or_default();if !items.iter().any(|(value,_)|value==&observed){items.push((observed,revision.id));}continue;}
            if changes_at.get(&at).is_some_and(|v|v!=&observed)||seen.get(&at).is_some_and(|(v,raw)|raw==&raw_subject&&v!=&observed){qualifications.insert(format!("{}:conflicting-observations",record.key));values.remove(&slot);continue;}
            seen.insert(at,(observed.clone(),raw_subject));values.insert(slot,(observed,revision.id));
        }
    }
    // Only this revision's observations are persisted. Replaying a known prefix is
    // for validation, never permission to restamp an old alive/location/owner value.
    for ((subject,predicate),(value,origin)) in values{
        if origin!=current.id||predicate=="status"{continue;}let Some(n)=nodes.get_mut(&subject)else{qualifications.insert(format!("{subject}:missing-lineage-identity"));continue;};
        n.chapter_idx=current.idx;let scalar=optional(&value,"value");
        match predicate.as_str(){"name"=>if let Some(s)=scalar{n.name=s},"description"=>n.summary=scalar,"life_status"=>n.life_status=scalar.filter(|s|s=="alive"||s=="dead"),"location"=>n.location=scalar,"owner"=>n.owner=scalar,_=>{qualifications.insert(format!("{subject}:{predicate}:not-lightweight-state"));}}
        if matches!(text(&value,"kind").as_str(),"unknown"|"none"){qualifications.insert(format!("{subject}:{predicate}:unknown-or-clear-retained-raw"));}
    }
    for ((subject,predicate),items) in collections{if predicate=="alias"{if let Some(n)=nodes.get_mut(&subject){for (value,origin) in items{if origin==current.id{if let Some(alias)=optional(&value,"value"){if !n.aliases.contains(&alias){n.aliases.push(alias);}n.chapter_idx=current.idx;}}}}}else{qualifications.insert(format!("{subject}:{predicate}:not-lightweight-state"));}}
    let mut edges=relations.into_values().filter(|(_,origin,_)|*origin==current.id).map(|(edge,_,_)|edge).collect::<Vec<_>>();
    let provenance=if current.source=="original"{"ai_original"}else{"ai_generated"};
    let mut events=current.events.iter().filter(|(_,v)|v.get("observed_chapter").and_then(Value::as_i64).unwrap_or(current.idx)<=current.idx).map(|(key,v)|{let mut e=event(v,format!("{}:{key}",current.idx),current.idx,provenance);for key in &mut e.participants{*key=canonical(key);}e.location=e.location.as_deref().map(&canonical);e}).collect::<Vec<_>>();
    for n in nodes.values_mut(){if n.entity_type!="character"{n.life_status=None;}if n.entity_type!="item"{n.owner=None;}}
    let types=nodes.iter().map(|(key,n)|(key.clone(),n.entity_type.clone())).collect::<BTreeMap<_,_>>();
    for n in nodes.values_mut(){if n.location.as_ref().is_some_and(|k|types.get(k).map(String::as_str)!=Some("location")){qualifications.insert(format!("{}:incompatible-location",n.key));n.location=None;}if n.owner.as_ref().is_some_and(|k|!matches!(types.get(k).map(String::as_str),Some("character"|"faction"))){qualifications.insert(format!("{}:incompatible-owner",n.key));n.owner=None;}}
    edges.retain(|e|{let valid=nodes.contains_key(&e.src)&&nodes.contains_key(&e.dst);if !valid{qualifications.insert(format!("{}:dangling-relation",e.key));}valid});
    for e in &mut events{e.participants.retain(|key|{let valid=nodes.contains_key(key);if !valid{qualifications.insert(format!("{}:dangling-participant:{key}",e.key));}valid});if e.location.as_ref().is_some_and(|key|types.get(key).map(String::as_str)!=Some("location")){qualifications.insert(format!("{}:incompatible-event-location",e.key));e.location=None;}}
    let mut retirements=BTreeMap::new();for key in map.keys(){let previous=resolve(&prior_map,key);let replacement=canonical(key);if previous!=replacement{retirements.insert(previous,replacement);}}
    (ChapterSnapshot{summary:current.summary.clone(),nodes:nodes.into_values().collect(),edges,events,open_threads:current.threads.clone()},retirements,qualifications)
}
pub(super) fn migrate(conn:&Connection)->rusqlite::Result<()>{
    let revisions=load_revisions(conn)?;
    for current in &revisions{
        let prefix=revisions.iter().filter(|r|r.book==current.book&&r.idx<current.idx&&r.active&&r.complete&&if current.source=="original"{r.source=="original"}else{r.selected}).collect::<Vec<_>>();
        let (snapshot,retirements,mut qualifications)=project_legacy(current,&prefix);
        if !current.complete{qualifications.insert("incomplete-legacy-revision".into());}
        conn.execute("INSERT INTO kg_lite_snapshots(id,book_id,text_version_id,digest,dependency_digest,schema_digest,data_json,legacy,active) VALUES (?1,?2,?3,?4,?5,'legacy-local',?6,1,?7)",params![current.id,current.book,current.text_id,current.digest,current.dependency,encode(&snapshot)?,current.active&&current.complete])?;
        for (retired,replacement) in retirements{conn.execute("INSERT INTO kg_lite_legacy_retirements(snapshot_id,retired_key,replacement_key) VALUES (?1,?2,?3)",params![current.id,retired,replacement])?;}
        if !qualifications.is_empty(){conn.execute("INSERT INTO kg_legacy_archive(book_id,source_table,source_id,data_json,reason) VALUES (?1,'kg_revisions',?2,?3,'lite-local-qualified-observations-retained-raw')",params![current.book,current.id,encode(&serde_json::json!({"qualifications":qualifications,"unresolved":current.unresolved}))?])?;}
    }
    let mut stmt=conn.prepare("SELECT id,book_id,chapter_idx,dependency_digest,line_version,data_json,active FROM kg_specs ORDER BY id")?;
    let specs=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?,r.get::<_,i64>(4)?,r.get::<_,String>(5)?,r.get::<_,bool>(6)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for(id,book,idx,dependency,line,data,active)in specs{let v=parse(&data)?;let mut view=empty_view("spec");view.chapter_idx=Some(idx);view.reading_line_version=line;view.summary=text(&v,"summary");view.open_threads=strings(&v,"open_threads");view.reader_progress=v.get("reader_progress").and_then(Value::as_i64).unwrap_or(0);view.divergence_idx=v.get("divergence_idx").and_then(Value::as_i64);
        for n in v.get("nodes").and_then(Value::as_array).into_iter().flatten(){view.nodes.push(node(n,n.get("chapter_idx").and_then(Value::as_i64).unwrap_or(idx),&text(n,"provenance")));}
        for e in v.get("edges").and_then(Value::as_array).into_iter().flatten(){view.edges.push(edge(e,text(e,"key"),e.get("chapter_idx").and_then(Value::as_i64).unwrap_or(idx),&text(e,"provenance")));}
        for e in v.get("events").and_then(Value::as_array).into_iter().flatten(){view.events.push(event(e,text(e,"key"),idx,&text(e,"provenance")));}
        conn.execute("INSERT INTO kg_lite_specs(id,book_id,chapter_idx,dependency_digest,line_version,data_json,legacy,active) VALUES (?1,?2,?3,?4,?5,?6,1,?7)",params![id,book,idx,dependency,line,encode(&view)?,active])?;
    }
    let mut stmt=conn.prepare("SELECT id,target_type,target_id,effective_from,before_json,after_json FROM edits_log ORDER BY id")?;let edits=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,i64>(3)?,r.get::<_,Option<String>>(4)?,r.get::<_,Option<String>>(5)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for(id,mut kind,target,idx,before,after)in edits{
        if kind=="graph"{if let Some(data)=after.as_deref().or(before.as_deref()){let v=parse(data)?;kind=if v.get("name").is_some()&&v.get("type").is_some(){"node"}else if v.get("src").is_some()||v.get("src_key").is_some(){"edge"}else if v.get("title").is_some(){"event"}else{"graph"}.into();}if kind=="graph"{conn.execute("UPDATE kg_legacy_archive SET reason='unconvertible-graph-intent-retained' WHERE source_table='edits_log' AND source_id=?1",[id])?;}}
        let convert=|data:Option<String>|->rusqlite::Result<Option<String>>{data.map(|s|{let mut v=parse(&s)?;if ["node","edge","event"].contains(&kind.as_str())&&v.is_object(){v["key"]=Value::String(target.clone());}convert_record(&kind,v,idx).and_then(|v|encode(&v))}).transpose()};
        conn.execute("UPDATE edits_log SET target_type=?2,before_json=?3,after_json=?4 WHERE id=?1",params![id,kind,convert(before)?,convert(after)?])?;
    }
    // Pre-facts settings are copied into the same durable intent log without resetting their IDs or bytes.
    conn.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,after_json,effective_from,scope,active) SELECT d.book_id,'directive','legacy-directive-'||d.id,'upsert',json_object('text',d.text,'polarity',d.polarity),d.effective_from,'global',d.active FROM directives d WHERE NOT EXISTS(SELECT 1 FROM edits_log e WHERE e.book_id=d.book_id AND e.target_type='directive' AND json_extract(e.after_json,'$.text')=d.text AND json_extract(e.after_json,'$.polarity')=d.polarity)",[])?;
    conn.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,after_json,effective_from,scope,active) SELECT r.book_id,'drift_pref','drift','upsert',json_object('value',r.drift_pref),coalesce(l.progress,0)+1,'global',1 FROM reader_intent r LEFT JOIN kg_lines l ON l.book_id=r.book_id WHERE r.drift_pref!=0 AND NOT EXISTS(SELECT 1 FROM edits_log e WHERE e.book_id=r.book_id AND e.target_type='drift_pref')",[])?;
    convert_projection_graphs(conn)?;Ok(())
}
fn convert_projection_graphs(conn:&Connection)->rusqlite::Result<()>{
    let mut graphs=BTreeMap::<(i64,i64,String),ChapterSnapshot>::new();
    // Already archived pre-facts graphs have no trustworthy chapter ownership: keep them qualified at 0.
    let mut stmt=conn.prepare("SELECT book_id,source_table,source_id,data_json FROM kg_legacy_archive WHERE source_table IN ('graph_nodes','graph_edges','graph_events') ORDER BY id")?;
    for row in stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?{let(book,table,id,data)=row?;let v=parse(&data)?;let graph=graphs.entry((book,0,"reference".into())).or_default();match table.as_str(){"graph_nodes"=>graph.nodes.push(node(&v,0,"legacy")),"graph_edges"=>graph.edges.push(edge(&v,format!("legacy-edge-{id}"),0,"legacy")),"graph_events"=>graph.events.push(event(&v,format!("legacy-event-{id}"),0,"legacy")),_=>{}}}
    // User projection rows from schema v1 are future intent, not extracted actual truth.
    let mut stmt=conn.prepare("SELECT book_id,key,coalesce(chapter_idx,0),json_object('key',key,'type',type,'name',name,'aliases_json',aliases_json,'summary',summary) FROM graph_nodes WHERE provenance='user' ORDER BY id")?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for(book,key,idx,data)in rows{let from=conn.query_row("SELECT coalesce((SELECT progress+1 FROM kg_lines WHERE book_id=?1),1)",[book],|r|r.get::<_,i64>(0))?.max(idx);let n=node(&parse(&data)?,from,"user");conn.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,after_json,effective_from,scope,active) SELECT ?1,'node',?2,'upsert',?3,?4,'global',1 WHERE NOT EXISTS(SELECT 1 FROM edits_log WHERE book_id=?1 AND target_type='node' AND target_id=?2)",params![book,key,encode(&n)?,from])?;}
    let mut stmt=conn.prepare("SELECT id,book_id,coalesce(chapter_idx,since_chapter_idx,0),json_object('src',src_key,'dst',dst_key,'rel_type',rel_type,'description',description,'status',status) FROM graph_edges WHERE provenance='user' ORDER BY id")?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for(id,book,idx,data)in rows{let from=conn.query_row("SELECT coalesce((SELECT progress+1 FROM kg_lines WHERE book_id=?1),1)",[book],|r|r.get::<_,i64>(0))?.max(idx);let key=format!("legacy-edge-{id}");let e=edge(&parse(&data)?,key.clone(),from,"user");conn.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,after_json,effective_from,scope,active) SELECT ?1,'edge',?2,'upsert',?3,?4,'global',1 WHERE NOT EXISTS(SELECT 1 FROM edits_log WHERE book_id=?1 AND target_type='edge' AND target_id=?2)",params![book,key,encode(&e)?,from])?;}
    let mut stmt=conn.prepare("SELECT id,book_id,chapter_idx,json_object('title',title,'order',order_in_chapter,'participants',participant_keys_json,'location',location_key,'summary',summary,'outcome',outcome) FROM graph_events WHERE provenance='user' AND revision_id IS NULL ORDER BY id")?;
    let rows=stmt.query_map([],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?,r.get::<_,String>(3)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    for(id,book,idx,data)in rows{let from=conn.query_row("SELECT coalesce((SELECT progress+1 FROM kg_lines WHERE book_id=?1),1)",[book],|r|r.get::<_,i64>(0))?.max(idx);let key=format!("legacy-event-{id}");let e=event(&parse(&data)?,key.clone(),from,"user");conn.execute("INSERT INTO edits_log(book_id,target_type,target_id,action,after_json,effective_from,scope,active) SELECT ?1,'event',?2,'upsert',?3,?4,'global',1 WHERE NOT EXISTS(SELECT 1 FROM edits_log WHERE book_id=?1 AND target_type='event' AND target_id=?2)",params![book,key,encode(&e)?,from])?;}
    for((book,idx,source),graph)in graphs{conn.execute("INSERT INTO kg_lite_legacy_graphs(book_id,chapter_idx,source,data_json) VALUES (?1,?2,?3,?4)",params![book,idx,source,encode(&graph)?])?;}
    Ok(())
}
