use super::*;
use std::collections::{BTreeMap,HashMap,HashSet};

struct Source {idx:i64,revision:i64,legacy:bool,snapshot:ChapterSnapshot,retirements:Vec<(String,String)>}
fn query_target(conn:&Connection,q:&GraphQuery)->Result<i64,String>{
    ensure_book(conn,q.book_id)?;if !["global","chapter"].contains(&q.scope.as_str())||!["actual","reference","spec"].contains(&q.purpose.as_str()){return Err("invalid graph query scope/purpose".into());}
    if q.scope=="chapter"&&q.chapter_idx.is_none(){return Err("chapter query requires target".into());}
    let idx=match q.chapter_idx{Some(idx)=>idx,None if q.purpose=="reference"=>conn.query_row("SELECT coalesce(max(idx),0) FROM chapters WHERE book_id=?1",[q.book_id],|r|r.get(0)).map_err(err)?,None=>progress(conn,q.book_id)?};
    if idx<0||idx==0&&q.scope=="chapter"{return Err("invalid graph target".into());}if idx>0{ensure_chapter(conn,q.book_id,idx)?;}Ok(idx)
}
fn sources(conn:&Connection,book:i64,purpose:&str,upto:i64)->Result<Vec<Source>,String>{
    // Decide selected story entirely from existing pins/active prose; do not materialize skipped texts.
    let mut stmt=conn.prepare("SELECT t.chapter_idx,s.id,s.legacy,s.data_json FROM kg_lite_snapshots s JOIN kg_text_versions t ON t.id=s.text_version_id WHERE t.book_id=?1 AND t.chapter_idx<=?2 AND s.active=1 AND ((?3!='actual' AND t.source='original') OR (?3='actual' AND (EXISTS(SELECT 1 FROM kg_read_selection p WHERE p.book_id=t.book_id AND p.chapter_idx=t.chapter_idx AND p.text_version_id=t.id) OR (NOT EXISTS(SELECT 1 FROM kg_read_selection p WHERE p.book_id=t.book_id AND p.chapter_idx=t.chapter_idx) AND ((t.source='original' AND (t.chapter_idx=1 OR NOT EXISTS(SELECT 1 FROM edits_log e WHERE e.book_id=t.book_id AND e.active=1 AND e.effective_from<=t.chapter_idx))) OR (t.source='generated' AND EXISTS(SELECT 1 FROM generated_chapters g WHERE g.id=t.generated_id AND g.active=1))))))) ORDER BY t.chapter_idx,s.id").map_err(err)?;
    let rows=stmt.query_map(params![book,upto,purpose],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,bool>(2)?,r.get::<_,String>(3)?))).map_err(err)?;let mut out=Vec::new();
    let mut retirement_rows=conn.prepare("SELECT retired_key,replacement_key FROM kg_lite_legacy_retirements WHERE snapshot_id=?1 ORDER BY retired_key").map_err(err)?;
    for row in rows{
        let(idx,revision,legacy,data)=row.map_err(err)?;
        let retirements=retirement_rows.query_map([revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
        out.push(Source{idx,revision,legacy,snapshot:parse(&data)?,retirements});
    }Ok(out)
}
fn initialized_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{let mut view=crate::core::kg::empty_view(&q.purpose);view.reader_progress=progress(conn,q.book_id)?;view.chapter_idx=q.chapter_idx;view.reading_line_version=line_version(conn,q.book_id)?;view.divergence_idx=divergence(conn,q.book_id)?;view.coverage=coverage(conn,q.book_id)?;Ok(view)}
fn fold_node(nodes:&mut BTreeMap<String,GraphNode>,n:&GraphNode){
    match nodes.entry(n.key.clone()){
        std::collections::btree_map::Entry::Vacant(entry)=>{entry.insert(n.clone());},
        std::collections::btree_map::Entry::Occupied(mut entry)=>{
            let prior=entry.get_mut();if prior.chapter_idx>n.chapter_idx{return;}
            if prior.entity_type!=n.entity_type{*prior=n.clone();return;}
            // Extraction null means unknown, not a clearing instruction. Explicit user nulls
            // are applied afterward by the future-only intent overlay and remain authoritative.
            prior.name.clone_from(&n.name);prior.chapter_idx=n.chapter_idx;prior.provenance.clone_from(&n.provenance);
            for alias in &n.aliases{if !prior.aliases.contains(alias){prior.aliases.push(alias.clone());}}
            if n.summary.is_some(){prior.summary.clone_from(&n.summary);}
            if n.life_status.is_some(){prior.life_status.clone_from(&n.life_status);}
            if n.location.is_some(){prior.location.clone_from(&n.location);}
            if n.owner.is_some(){prior.owner.clone_from(&n.owner);}
        }
    }
}
fn apply_retirements(s:&Source,nodes:&mut BTreeMap<String,GraphNode>,edges:&mut BTreeMap<String,GraphEdge>,events:&mut BTreeMap<String,GraphEvent>){
    if s.retirements.is_empty(){return;}
    let remapped=s.retirements.iter().map(|(retired,replacement)|(retired.as_str(),replacement.as_str())).collect::<HashMap<_,_>>();
    let replacement=|key:&mut String|{if let Some(to)=remapped.get(key.as_str()){*key=(*to).to_string();}};
    for node in nodes.values_mut(){if let Some(key)=&mut node.location{replacement(key);}if let Some(key)=&mut node.owner{replacement(key);}}
    for edge in edges.values_mut(){replacement(&mut edge.src);replacement(&mut edge.dst);}
    for event in events.values_mut(){for key in &mut event.participants{replacement(key);}if let Some(key)=&mut event.location{replacement(key);}}
    for (retired,canonical) in &s.retirements{
        if let Some(mut node)=nodes.remove(retired){
            node.key.clone_from(canonical);
            if let Some(prior)=nodes.get_mut(canonical){
                if prior.entity_type==node.entity_type{
                    if prior.life_status.is_none(){prior.life_status.clone_from(&node.life_status);}
                    if prior.location.is_none(){prior.location.clone_from(&node.location);}
                    if prior.owner.is_none(){prior.owner.clone_from(&node.owner);}
                    if prior.summary.is_none(){prior.summary.clone_from(&node.summary);}
                    for alias in &node.aliases{if !prior.aliases.contains(alias){prior.aliases.push(alias.clone());}}
                }
            }
            fold_node(nodes,&node);
        }
    }
}
fn base_view(conn:&Connection,q:&GraphQuery,upto:i64)->Result<GraphView,String>{
    let mut view=initialized_view(conn,q)?;let source=sources(conn,q.book_id,&q.purpose,upto)?;
    let mut nodes=BTreeMap::<String,GraphNode>::new();let mut edges=BTreeMap::<String,GraphEdge>::new();let mut events=BTreeMap::<String,GraphEvent>::new();let mut summaries=Vec::new();
    for s in &source{
        apply_retirements(s,&mut nodes,&mut edges,&mut events);
        for n in &s.snapshot.nodes{fold_node(&mut nodes,n);}
        for e in &s.snapshot.edges{edges.insert(e.key.clone(),e.clone());}
        for e in &s.snapshot.events{events.insert(e.key.clone(),e.clone());}
        if q.scope!="chapter"||q.chapter_idx==Some(s.idx){if !s.snapshot.summary.is_empty(){summaries.push(s.snapshot.summary.clone());}view.open_threads=s.snapshot.open_threads.clone();view.revision=Some(s.revision);}
    }
    // Earlier, evidence-less legacy projections remain inspectable, never current-format cache/readiness.
    let mut stmt=conn.prepare("SELECT data_json FROM kg_lite_legacy_graphs WHERE book_id=?1 AND source='reference' AND chapter_idx<=?2 AND ?3='reference' ORDER BY chapter_idx,id").map_err(err)?;
    for row in stmt.query_map(params![q.book_id,upto,q.purpose],|r|r.get::<_,String>(0)).map_err(err)?{let legacy:ChapterSnapshot=parse(&row.map_err(err)?)?;for n in legacy.nodes{nodes.entry(n.key.clone()).or_insert(n);}for e in legacy.edges{edges.entry(e.key.clone()).or_insert(e);}for e in legacy.events{events.entry(e.key.clone()).or_insert(e);}}
    view.summary=summaries.join("\n");view.nodes=nodes.into_values().collect();view.edges=edges.into_values().collect();view.events=events.into_values().collect();
    if q.scope=="chapter"{
        let mut relevant=HashSet::new();if let Some(s)=source.iter().find(|s|Some(s.idx)==q.chapter_idx){relevant.extend(s.snapshot.nodes.iter().map(|n|n.key.clone()));}
        view.edges.retain(|e|Some(e.chapter_idx)==q.chapter_idx);view.events.retain(|e|Some(e.chapter_idx)==q.chapter_idx);
        relevant.extend(view.edges.iter().flat_map(|e|[e.src.clone(),e.dst.clone()]));relevant.extend(view.events.iter().flat_map(|e|e.participants.iter().cloned().chain(e.location.clone())));
        loop{let mut added=false;for node in &view.nodes{if relevant.contains(&node.key){for key in node.location.iter().chain(node.owner.iter()){added|=relevant.insert(key.clone());}}}if !added{break;}}
        view.nodes.retain(|n|relevant.contains(&n.key));
    }
    if q.purpose=="actual"{
        let completed=source.iter().filter(|s|!s.legacy).map(|s|s.idx).collect::<Vec<_>>();let legacy=source.iter().filter(|s|s.legacy).map(|s|s.idx).collect::<Vec<_>>();let missing=(1..=upto).filter(|i|!completed.contains(i)).collect::<Vec<_>>();
        view.coverage.completed_chapters=completed;view.coverage.legacy_chapters=legacy;view.coverage.missing_chapters=missing;view.coverage.selected_chapters=(1..=upto).collect();view.coverage.target_complete=upto>0&&view.coverage.missing_chapters.is_empty();view.coverage.mode=if view.coverage.target_complete{"full"}else{"partial"}.into();
    }
    view.ready=if q.scope=="chapter"{source.iter().any(|s|Some(s.idx)==q.chapter_idx&&!s.legacy)}else{view.coverage.target_complete};view.drift_score=Some(actual_drift(conn,q.book_id,upto)?);Ok(view)
}
pub(super) fn include_actual_references(conn:&Connection,book:i64,idx:i64,view:&mut GraphView)->Result<(),String>{
    let progress=progress(conn,book)?;
    let actual=base_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(progress)},progress)?;
    let available=actual.nodes.into_iter().map(|node|(node.key.clone(),node)).collect::<HashMap<_,_>>();
    let removed=intent::applicable(conn,book,idx)?.into_iter().filter(|edit|edit.target_type=="node"&&edit.action=="delete").map(|edit|edit.target_id).collect::<HashSet<_>>();
    let mut known=view.nodes.iter().map(|node|node.key.clone()).collect::<HashSet<_>>();
    let mut needed=view.nodes.iter().flat_map(|node|node.location.iter().chain(node.owner.iter()).cloned()).chain(view.edges.iter().flat_map(|edge|[edge.src.clone(),edge.dst.clone()])).chain(view.events.iter().flat_map(|event|event.participants.iter().cloned().chain(event.location.clone()))).collect::<Vec<_>>();
    while let Some(key)=needed.pop(){
        if known.contains(&key)||removed.contains(&key){continue;}
        if let Some(node)=available.get(&key){known.insert(key);needed.extend(node.location.iter().chain(node.owner.iter()).cloned());view.nodes.push(node.clone());}
    }
    Ok(())
}
pub fn internal_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{let target=query_target(conn,q)?;let mut view=base_view(conn,q,target)?;
    if q.purpose=="spec"{
        let spec=conn.query_row("SELECT data_json,legacy FROM kg_lite_specs WHERE book_id=?1 AND chapter_idx=?2 AND active=1 AND line_version=?3",params![q.book_id,target,line_version(conn,q.book_id)?],|r|Ok((r.get::<_,String>(0)?,r.get::<_,bool>(1)?))).optional().map_err(err)?;
        if let Some((data,legacy))=spec{let mut adapted:GraphView=parse(&data)?;adapted.reader_progress=view.reader_progress;adapted.reading_line_version=view.reading_line_version;adapted.divergence_idx=view.divergence_idx;adapted.coverage=view.coverage;adapted.chapter_idx=q.chapter_idx;adapted.ready=!legacy;view=adapted;}
        intent::overlay(conn,q.book_id,target,&mut view)?;
        include_actual_references(conn,q.book_id,target,&mut view)?;
    }Ok(view)
}
fn publicize(book:i64,view:&mut GraphView){let names=view.nodes.iter().map(|n|(n.key.clone(),public_key(book,"n",&n.key))).collect::<HashMap<_,_>>();
    for node in &mut view.nodes{node.key=names[&node.key].clone();node.location=node.location.as_ref().and_then(|k|names.get(k).cloned());node.owner=node.owner.as_ref().and_then(|k|names.get(k).cloned());}
    view.edges.retain(|e|names.contains_key(&e.src)&&names.contains_key(&e.dst));for e in &mut view.edges{e.key=public_key(book,"r",&e.key);e.src=names[&e.src].clone();e.dst=names[&e.dst].clone();}
    for e in &mut view.events{e.key=public_key(book,"e",&e.key);e.participants=e.participants.iter().filter_map(|k|names.get(k).cloned()).collect();e.location=e.location.as_ref().and_then(|k|names.get(k).cloned());}
}
pub fn get_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{let target=query_target(conn,q)?;let p=progress(conn,q.book_id)?;
    if q.purpose=="spec"&&target>p+1{return Err("only next chapter specification is public".into());}
    let revealed=conn.query_row("SELECT revealed_idx=?2 FROM kg_lines WHERE book_id=?1",params![q.book_id,target],|r|r.get::<_,Option<bool>>(0)).map_err(err)?.unwrap_or(false)&&target==p+1;
    let mut view=if q.purpose=="spec"&&target>p{
        let mut full=internal_view(conn,q)?;if !revealed{
            let fog=FogCounts{entities:full.nodes.iter().filter(|n|n.provenance!="user").count(),relations:full.edges.iter().filter(|e|e.provenance!="user").count(),events:full.events.iter().filter(|e|e.provenance!="user").count()};
            full.nodes.retain(|n|n.provenance=="user");full.edges.retain(|e|e.provenance=="user");full.events.retain(|e|e.provenance=="user");
            let needed=full.edges.iter().flat_map(|e|[e.src.clone(),e.dst.clone()]).chain(full.events.iter().flat_map(|e|e.participants.iter().cloned().chain(e.location.clone()))).chain(full.nodes.iter().flat_map(|n|n.location.iter().chain(n.owner.iter()).cloned())).collect::<HashSet<_>>();
            let actual=base_view(conn,&GraphQuery{book_id:q.book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(p)},p)?;for node in actual.nodes{if needed.contains(&node.key)&&!full.nodes.iter().any(|n|n.key==node.key){full.nodes.push(node);}}
            full.summary.clear();full.open_threads.clear();full.fog=Some(fog);full.revision=None;full.drift_score=None;
        }full
    }else if q.purpose=="reference"{internal_view(conn,q)?}else{base_view(conn,q,target.min(p))?};view.revealed=revealed&&q.purpose=="spec";publicize(q.book_id,&mut view);Ok(view)
}
pub(super) fn authorized_key(conn:&Connection,book:i64,kind:&str,key:&str)->Result<String,String>{let p=progress(conn,book)?;let mut views=vec![internal_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(p)})?];if ensure_chapter(conn,book,p+1).is_ok(){views.push(internal_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"spec".into(),chapter_idx:Some(p+1)})?);}let reveal=conn.query_row("SELECT revealed_idx=?2 FROM kg_lines WHERE book_id=?1",params![book,p+1],|r|r.get::<_,Option<bool>>(0)).map_err(err)?.unwrap_or(false);
    for(index,view)in views.iter().enumerate(){match kind{
        "node"=>for n in &view.nodes{if(index==0||reveal||n.provenance=="user")&&public_key(book,"n",&n.key)==key{return Ok(n.key.clone());}},
        "edge"=>for e in &view.edges{if(index==0||reveal||e.provenance=="user")&&public_key(book,"r",&e.key)==key{return Ok(e.key.clone());}},
        "event"=>for e in &view.events{if(index==0||reveal||e.provenance=="user")&&public_key(book,"e",&e.key)==key{return Ok(e.key.clone());}},_=>return Err("invalid graph element kind".into())}}
    Err("unknown or unauthorized graph target".into())
}
pub(super) fn validate_spec(conn:&Connection,book:i64,idx:i64,spec:&GraphView)->Result<(),String>{
    let reference=base_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"reference".into(),chapter_idx:Some(idx)},idx)?;let actual=base_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(idx-1)},idx-1)?;
    let mut known=reference.nodes.into_iter().chain(actual.nodes).map(|n|n.key).collect::<HashSet<_>>();known.extend(intent::applicable(conn,book,idx)?.into_iter().filter(|e|e.target_type=="node"&&e.action=="upsert").map(|e|e.target_id));
    for n in &spec.nodes{if !known.contains(&n.key)&&!(n.provenance=="ai_adapted"&&n.key.starts_with("spec-")&&n.key.len()>=20&&n.key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-')){return Err("foreign or unknown spec identity".into());}if n.chapter_idx>idx{return Err("spec node exceeds target".into());}}
    if spec.events.iter().any(|e|e.chapter_idx!=idx){return Err("spec event exceeds target".into());}
    validate_snapshot(&ChapterSnapshot{summary:spec.summary.clone(),nodes:spec.nodes.clone(),edges:spec.edges.clone(),events:spec.events.clone(),open_threads:spec.open_threads.clone()})
}
pub fn generation_context(conn:&Connection,book_id:i64,idx:i64)->Result<GenerationContext,String>{ensure_chapter(conn,book_id,idx)?;if idx<=1{return Err("first chapter is immutable original".into());}
    // Immutable skipped originals need neither text versions nor hashes to establish
    // dependency identity. Keep the chosen generated chain and immediate prior prose.
    let start=divergence(conn,book_id)?.filter(|from|*from<idx).unwrap_or(idx-1);
    let mut prior=Vec::new();for chapter in start..idx{let text=selected_text(conn,book_id,chapter)?.ok_or_else(||format!("selected prior chapter {chapter} unavailable"))?;let graph=active_snapshot(conn,text.id)?;prior.push((text,graph));}
    let actual=internal_view(conn,&GraphQuery{book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(idx-1)})?;let spec=internal_view(conn,&GraphQuery{book_id,scope:"chapter".into(),purpose:"spec".into(),chapter_idx:Some(idx)})?;
    let previous_text=prior.last().ok_or("previous prose unavailable")?.0.content.clone();let reference=text_version(conn,book_id,idx,"original",None)?;let reference_graph=active_snapshot(conn,reference.id)?;
    let style_guide=conn.query_row("SELECT style_guide FROM books WHERE id=?1",[book_id],|r|r.get::<_,Option<String>>(0)).map_err(err)?;let records=intent::applicable(conn,book_id,idx)?;let directives=intent_constraints(conn,book_id,idx)?["directives"].as_array().ok_or("invalid directive constraints")?.iter().map(json).collect::<Result<Vec<_>,_>>()?;let drift_pref=intent::drift_at(conn,book_id,idx)?;
    let stable_edits=records.iter().map(|r|(&r.target_type,&r.target_id,&r.action,&r.after_json,r.effective_from)).collect::<Vec<_>>();let stable_prior=prior.iter().map(|(t,s)|(&t.content_hash,&t.source,s)).collect::<Vec<_>>();
    let stable_actual=(&actual.nodes,&actual.edges,&actual.events,&actual.summary,&actual.open_threads);
    let dependency=digest(json(&(book_id,idx,stable_prior,stable_actual,reference_graph,stable_edits,&style_guide,drift_pref))?.as_bytes());Ok(GenerationContext{book_id,chapter_idx:idx,reading_line_version:line_version(conn,book_id)?,digest:dependency,actual,spec,previous_text,reference_text:Some(reference.content),style_guide,directives,drift_pref})
}
