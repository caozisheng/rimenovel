use super::*;
use std::collections::{BTreeMap,HashMap,HashSet};

#[derive(Clone)]
struct Source { text:TextVersion, revision:i64, bundle:ChapterBundle }
fn query_target(conn:&Connection,q:&GraphQuery)->Result<i64,String>{
    ensure_book(conn,q.book_id)?;
    if !["global","chapter"].contains(&q.scope.as_str()) || !["actual","reference","spec"].contains(&q.purpose.as_str()){return Err("invalid graph query scope/purpose".into());}
    if q.scope=="chapter" && q.chapter_idx.is_none(){return Err("chapter query requires target".into());}
    let idx=q.chapter_idx.unwrap_or(progress(conn,q.book_id)?);
    if idx<0 || (idx==0 && q.scope=="chapter"){return Err("invalid graph target".into());}
    if idx>0{ensure_chapter(conn,q.book_id,idx)?;}Ok(idx)
}
fn sources(conn:&Connection,book:i64,purpose:&str,upto:i64)->Result<Vec<Source>,String>{
    let mut stmt=conn.prepare("SELECT t.id,r.id FROM kg_text_versions t JOIN kg_revisions r ON r.text_version_id=t.id AND r.active=1 AND r.complete=1 WHERE t.book_id=?1 AND t.chapter_idx<=?2 ORDER BY t.chapter_idx").map_err(err)?;
    let rows=stmt.query_map(params![book,upto],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).map_err(err)?;
    let mut out=Vec::new();
    for row in rows {let(text_id,revision)=row.map_err(err)?;let text=by_id(conn,text_id)?;
        let eligible=if purpose=="reference" || purpose=="spec" {text.source=="original"}else{selected_text(conn,book,text.chapter_idx)?.is_some_and(|selected|selected.id==text.id)};
        if eligible {out.push(Source{text,revision,bundle:revision_bundle(conn,revision)?});}
    }Ok(out)
}
pub fn extraction_state(conn:&Connection,book_id:i64,source:&str,upto:i64)->Result<ReducedState,String>{
    let purpose=match source{"original"=>"reference","generated"=>"actual",_=>return Err("invalid extraction state source".into())};
    let bundles=sources(conn,book_id,purpose,upto)?.into_iter().map(|s|s.bundle).collect::<Vec<_>>();Ok(reduce_state(&bundles,upto))
}
fn empty_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{Ok(GraphView{nodes:vec![],edges:vec![],events:vec![],purpose:q.purpose.clone(),reader_progress:progress(conn,q.book_id)?,chapter_idx:q.chapter_idx,fog:None,revealed:false,ready:true,unresolved:vec![],revision:None,drift_score:None,reading_line_version:line_version(conn,q.book_id)?,divergence_idx:divergence(conn,q.book_id)?})}
fn base_view(conn:&Connection,q:&GraphQuery,upto:i64)->Result<GraphView,String>{
    let mut view=empty_view(conn,q)?;
    let source=sources(conn,q.book_id,&q.purpose,upto)?;
    let bundles=source.iter().map(|s|s.bundle.clone()).collect::<Vec<_>>();
    let state=reduce_state(&bundles,upto);let identities=revealed_identity_map(&bundles,upto);
    let mut nodes:BTreeMap<String,GraphNode>=BTreeMap::new();
    for s in &source {
        let provenance=if s.text.source=="original"{"ai_original"}else{"ai_generated"};
        let mut eligible:std::collections::BTreeSet<String>=s.bundle.stage_b.facts.iter().filter(|f|f.observed_chapter<=upto).map(|f|f.subject_key.clone()).collect();
        for event in &s.bundle.stage_a.events {if event.observed_chapter<=upto {eligible.extend(event.participants.clone());eligible.extend(event.location.clone());}}
        for relation in &s.bundle.stage_b.relations {if relation.observed_chapter<=upto{eligible.insert(relation.src.clone());eligible.insert(relation.dst.clone());}}
        for key in eligible {
            let canonical=resolve_identity(&identities,&key);
            let identity=s.bundle.identities.iter().find(|i|i.key==key).cloned().or_else(||bundles.iter().flat_map(|b|&b.identities).find(|i|i.key==key).cloned());
            let Some(identity)=identity else {continue;};
            let observed_name=if s.bundle.spans.iter().any(|span|span.text.contains(&identity.name)){identity.name.clone()}else{s.bundle.stage_a.mentions.iter().find(|m|m.candidate_keys.contains(&key)).map(|m|m.name.clone()).unwrap_or_else(||"Unknown entity".into())};
            let node=nodes.entry(canonical.clone()).or_insert_with(||GraphNode{key:canonical.clone(),entity_type:identity.entity_type.clone(),name:observed_name,aliases:vec![],summary:None,chapter_idx:s.text.chapter_idx,provenance:provenance.into()});
            let lookup=|predicate:&str|state.values.get(&(canonical.clone(),predicate.into()));
            if let Some(name)=lookup("name").and_then(|v|v.value.as_ref()) {node.name=name.clone();}
            if let Some(description)=lookup("description").and_then(|v|v.value.as_ref()) {node.summary=Some(description.clone());}
            for alias in state.collections.get(&(canonical.clone(),"alias".into())).into_iter().flatten().filter_map(|v|v.value.as_ref()) {if !node.aliases.contains(alias){node.aliases.push(alias.clone());}}
        }
        for event in &s.bundle.stage_a.events {
            if event.observed_chapter>upto || (q.scope=="chapter" && Some(event.chapter_idx)!=q.chapter_idx){continue;}
            view.events.push(GraphEvent{key:format!("{}:{}",event.chapter_idx,event.key),title:event.title.clone(),chapter_idx:event.chapter_idx,order:event.observed_order,participants:event.participants.iter().map(|k|resolve_identity(&identities,k)).collect(),location:event.location.as_ref().map(|k|resolve_identity(&identities,k)),summary:event.summary.clone(),outcome:event.outcome.clone(),provenance:provenance.into()});
        }
        if q.scope=="chapter" && q.chapter_idx==Some(s.text.chapter_idx){view.revision=Some(s.revision);}
        view.unresolved.extend(s.bundle.stage_b.unresolved.clone());
    }
    view.unresolved.extend(state.conflicts);
    for (key,relation) in state.relations {
        let src=resolve_identity(&identities,&relation.src);let dst=resolve_identity(&identities,&relation.dst);
        if !nodes.contains_key(&src)||!nodes.contains_key(&dst){continue;}
        if q.scope=="chapter" && Some(relation.chapter_idx)!=q.chapter_idx && !bundles.iter().flat_map(|b|&b.stage_b.changes).any(|c|c.subject_key==key && Some(c.chapter_idx)==q.chapter_idx){continue;}
        let earliest=bundles.iter().flat_map(|b|&b.stage_b.relations).filter(|r|r.key==key).map(|r|r.chapter_idx).min().unwrap_or(relation.chapter_idx);
        let provenance=source.iter().find(|s|s.text.chapter_idx==relation.chapter_idx).map(|s|if s.text.source=="original"{"ai_original"}else{"ai_generated"}).unwrap_or("ai_original");
        view.edges.push(GraphEdge{key,src,dst,rel_type:relation.rel_type,description:relation.description,status:relation.status,since_chapter:earliest,chapter_idx:relation.chapter_idx,provenance:provenance.into()});
    }
    view.nodes=nodes.into_values().collect();
    if q.scope=="chapter" {
        let mut relevant:HashSet<_>=view.events.iter().flat_map(|e|e.participants.iter().cloned().chain(e.location.clone())).collect();
        relevant.extend(view.edges.iter().flat_map(|e|[e.src.clone(),e.dst.clone()]));
        for s in &source {if Some(s.text.chapter_idx)==q.chapter_idx{relevant.extend(s.bundle.stage_b.facts.iter().map(|f|resolve_identity(&identities,&f.subject_key)));}}
        view.nodes.retain(|n|relevant.contains(&n.key));
    }
    for idx in 1..=upto {if !source.iter().any(|s|s.text.chapter_idx==idx){view.ready=false;}}
    if !view.ready {view.unresolved.push("Missing committed chapters".into());}
    view.drift_score=Some(actual_drift(conn,q.book_id,upto)?);
    Ok(view)
}
pub fn internal_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{
    let target=query_target(conn,q)?;
    let mut view=base_view(conn,q,target)?;
    if q.purpose=="spec" {
        let spec=conn.query_row("SELECT data_json FROM kg_specs WHERE book_id=?1 AND chapter_idx=?2 AND active=1 AND line_version=?3",params![q.book_id,target,line_version(conn,q.book_id)?],|r|r.get::<_,String>(0)).optional().map_err(err)?;
        if let Some(spec)=spec {let mut adapted:GraphView=parse(&spec)?;adapted.reader_progress=view.reader_progress;adapted.reading_line_version=view.reading_line_version;adapted.divergence_idx=view.divergence_idx;adapted.chapter_idx=q.chapter_idx;view=adapted;}
        intent::overlay(conn,q.book_id,target,&mut view)?;
    }
    Ok(view)
}
fn publicize(book:i64,view:&mut GraphView){
    let names:HashMap<_,_>=view.nodes.iter().map(|n|(n.key.clone(),public_key(book,"n",&n.key))).collect();
    for node in &mut view.nodes{node.key=names[&node.key].clone();}
    view.edges.retain(|e|names.contains_key(&e.src)&&names.contains_key(&e.dst));
    for edge in &mut view.edges{edge.key=public_key(book,"r",&edge.key);edge.src=names[&edge.src].clone();edge.dst=names[&edge.dst].clone();}
    for event in &mut view.events{event.key=public_key(book,"e",&event.key);event.participants=event.participants.iter().filter_map(|k|names.get(k).cloned()).collect();event.location=event.location.as_ref().and_then(|k|names.get(k).cloned());}
    if !view.unresolved.is_empty(){view.unresolved=vec![format!("{} unresolved observations",view.unresolved.len())];}
}
pub fn get_view(conn:&Connection,q:&GraphQuery)->Result<GraphView,String>{
    let target=query_target(conn,q)?;let p=progress(conn,q.book_id)?;
    if q.purpose=="spec" && target>p+1{return Err("only next chapter specification is public".into());}
    let revealed=conn.query_row("SELECT revealed_idx=?2 FROM kg_lines WHERE book_id=?1",params![q.book_id,target],|r|r.get::<_,Option<bool>>(0)).map_err(err)?.unwrap_or(false) && target==p+1;
    let mut view=if q.purpose=="spec" && target>p {
        let mut full=internal_view(conn,q)?;
        if !revealed {
            let fog=FogCounts{entities:full.nodes.iter().filter(|n|n.provenance!="user").count(),relations:full.edges.iter().filter(|n|n.provenance!="user").count(),events:full.events.iter().filter(|n|n.provenance!="user").count()};
            full.nodes.retain(|n|n.provenance=="user");full.edges.retain(|n|n.provenance=="user");full.events.retain(|n|n.provenance=="user");
            let endpoints:HashSet<_>=full.edges.iter().flat_map(|e|[e.src.clone(),e.dst.clone()]).chain(full.events.iter().flat_map(|e|e.participants.iter().cloned().chain(e.location.clone()))).collect();
            let actual=base_view(conn,&GraphQuery{book_id:q.book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(p)},p)?;
            for node in actual.nodes{if endpoints.contains(&node.key)&&!full.nodes.iter().any(|n|n.key==node.key){full.nodes.push(node);}}
            full.fog=Some(fog);full.unresolved.clear();full.revision=None;full.drift_score=None;
        }full
    } else {base_view(conn,q,target.min(p))?};
    view.revealed=revealed && q.purpose=="spec";
    publicize(q.book_id,&mut view);Ok(view)
}
/// Resolve only identities present in a currently authorized view; never search the
/// unrestricted registry for a client-supplied key.
pub(super) fn authorized_key(conn:&Connection,book:i64,kind:&str,key:&str)->Result<String,String>{
    let p=progress(conn,book)?;let mut views=vec![internal_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(p)})?];
    if ensure_chapter(conn,book,p+1).is_ok(){views.push(internal_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"spec".into(),chapter_idx:Some(p+1)})?);}
    let reveal=conn.query_row("SELECT revealed_idx=?2 FROM kg_lines WHERE book_id=?1",params![book,p+1],|r|r.get::<_,Option<bool>>(0)).map_err(err)?.unwrap_or(false);
    for (index,view) in views.iter().enumerate(){
        match kind{
            "node"=>for n in &view.nodes{if (index==0||reveal||n.provenance=="user") && public_key(book,"n",&n.key)==key{return Ok(n.key.clone());}},
            "edge"=>for e in &view.edges{if (index==0||reveal||e.provenance=="user") && public_key(book,"r",&e.key)==key{return Ok(e.key.clone());}},
            "event"=>for e in &view.events{if (index==0||reveal||e.provenance=="user") && public_key(book,"e",&e.key)==key{return Ok(e.key.clone());}},_=>return Err("invalid graph element kind".into())
        }
    }Err("unknown or unauthorized graph target".into())
}
pub fn evidence(conn:&Connection,q:&GraphQuery,kind:&str,public_id:&str)->Result<Vec<EvidenceView>,String>{
    let visible=get_view(conn,q)?;
    let selected_provenance=match kind{"node"=>visible.nodes.iter().find(|n|n.key==public_id).map(|n|n.provenance.as_str()),"edge"=>visible.edges.iter().find(|e|e.key==public_id).map(|e|e.provenance.as_str()),"event"=>visible.events.iter().find(|e|e.key==public_id).map(|e|e.provenance.as_str()),_=>None};
    let selected_provenance=selected_provenance.ok_or("unknown or unauthorized evidence target")?;
    if selected_provenance=="user"||selected_provenance=="ai_adapted"{return Ok(vec![]);}
    let target=query_target(conn,q)?;let upto=if q.purpose=="spec" && visible.revealed {target}else{target.min(progress(conn,q.book_id)?)};
    let source=sources(conn,q.book_id,&q.purpose,upto)?;let bundles=source.iter().map(|s|s.bundle.clone()).collect::<Vec<_>>();let identity_map=revealed_identity_map(&bundles,upto);
    let state=reduce_state(&bundles,upto);
    let mut out=Vec::new();let mut seen=HashSet::new();
    for s in source{
        let mut refs=HashSet::new();let matches_node=|key:&str|public_key(q.book_id,"n",&resolve_identity(&identity_map,key))==public_id;
        if kind=="node"{
            for f in &s.bundle.stage_b.facts{let subject=resolve_identity(&identity_map,&f.subject_key);let slot=(subject,f.predicate.clone());let current=state.value_origins.get(&slot).is_some_and(|o|o.kind=="fact"&&o.chapter_idx==f.chapter_idx&&o.record_key==f.key)||state.collections.get(&slot).is_some_and(|values|values.contains(&f.value));if f.observed_chapter<=upto&&matches_node(&f.subject_key)&&current&&hard_assertion(&f.temporal_scope,&f.assertion_kind){refs.extend(f.evidence_refs.clone());}}
            for c in &s.bundle.stage_b.changes{let subject=resolve_identity(&identity_map,&c.subject_key);let slot=(subject,c.predicate.clone());let current=state.value_origins.get(&slot).is_some_and(|o|o.kind=="change"&&o.chapter_idx==c.chapter_idx&&o.record_key==c.key)||(c.operation=="add"&&state.collections.get(&slot).is_some_and(|values|values.contains(&c.after)));if c.observed_chapter<=upto&&matches_node(&c.subject_key)&&current&&hard_assertion(&c.temporal_scope,&c.assertion_kind){refs.extend(c.evidence_refs.clone());}}
            for l in &s.bundle.identity_links{if l.observed_chapter<=upto && (matches_node(&l.from_key)||matches_node(&l.to_key)){refs.extend(l.evidence_refs.clone());}}
        }else if kind=="edge"{
            for r in &s.bundle.stage_b.relations{if r.observed_chapter<=upto&&public_key(q.book_id,"r",&r.key)==public_id&&state.relation_origins.get(&r.key).is_some_and(|o|o.kind=="relation"&&o.chapter_idx==r.chapter_idx&&o.record_key==r.key){refs.extend(r.evidence_refs.clone());}}
            for c in &s.bundle.stage_b.changes{if c.observed_chapter<=upto&&c.predicate=="status"&&public_key(q.book_id,"r",&c.subject_key)==public_id&&state.relation_origins.get(&c.subject_key).is_some_and(|o|o.kind=="change"&&o.chapter_idx==c.chapter_idx&&o.record_key==c.key){refs.extend(c.evidence_refs.clone());}}
        }else{
            if (s.text.source=="original"&&selected_provenance!="ai_original")||(s.text.source=="generated"&&selected_provenance!="ai_generated"){continue;}
            for e in &s.bundle.stage_a.events{if e.observed_chapter<=upto && public_key(q.book_id,"e",&format!("{}:{}",e.chapter_idx,e.key))==public_id{refs.extend(e.evidence_refs.clone());}}
            for p in &s.bundle.stage_b.preconditions{if s.bundle.stage_a.events.iter().any(|e|e.key==p.event_key && e.observed_chapter<=upto && public_key(q.book_id,"e",&format!("{}:{}",e.chapter_idx,e.key))==public_id){refs.extend(p.evidence_refs.clone());}}
        }
        for span in &s.bundle.spans{if refs.contains(&span.key) && seen.insert((s.text.id,span.start_byte,span.end_byte)){out.push(EvidenceView{key:public_key(q.book_id,"s",&format!("{}:{}",s.text.id,span.key)),chapter_idx:span.chapter_idx,start_byte:span.start_byte,end_byte:span.end_byte,text:span.text.clone(),source:s.text.source.clone()});}}
    }Ok(out)
}
pub(super) fn validate_spec(conn:&Connection,book:i64,idx:i64,spec:&GraphView)->Result<(),String>{
    let reference=base_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"reference".into(),chapter_idx:Some(idx)},idx)?;
    let actual=base_view(conn,&GraphQuery{book_id:book,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(idx-1)},idx-1)?;
    let mut known:HashSet<String>=reference.nodes.into_iter().chain(actual.nodes).map(|n|n.key).collect();
    known.extend(intent::applicable(conn,book,idx)?.into_iter().filter(|e|e.target_type=="node"&&e.action=="upsert").map(|e|e.target_id));
    let mut nodes=HashSet::new();
    for node in &spec.nodes{if !ENTITY_TYPES.contains(&node.entity_type.as_str())||node.name.trim().is_empty()||node.key.is_empty()||!nodes.insert(node.key.as_str()){return Err("invalid/duplicate spec identity".into());}if !known.contains(&node.key) && !(node.provenance=="ai_adapted" && node.key.starts_with("spec-") && node.key.len()>=20 && node.key.bytes().all(|b|b.is_ascii_alphanumeric()||b==b'-')){return Err("foreign or unknown spec identity".into());}if node.chapter_idx>idx{return Err("spec node exceeds target".into());}}
    let mut keys=HashSet::new();for edge in &spec.edges{if !keys.insert(edge.key.as_str())||!nodes.contains(edge.src.as_str())||!nodes.contains(edge.dst.as_str())||!["active","broken","former","unknown"].contains(&edge.status.as_str()){return Err("invalid spec relation references/status".into());}}
    keys.clear();for event in &spec.events{if !keys.insert(event.key.as_str())||event.chapter_idx!=idx||event.order<0||event.participants.iter().any(|k|!nodes.contains(k.as_str()))||event.location.as_ref().is_some_and(|k|!nodes.contains(k.as_str())){return Err("invalid spec event references".into());}}
    Ok(())
}
pub fn generation_context(conn:&Connection,book_id:i64,idx:i64)->Result<GenerationContext,String>{
    ensure_chapter(conn,book_id,idx)?;if idx<=1{return Err("first chapter is immutable original".into());}
    let mut prior=Vec::new();
    for chapter in 1..idx{let text=selected_text(conn,book_id,chapter)?.ok_or_else(||format!("selected prior chapter {chapter} unavailable"))?;let bundle=active_bundle(conn,text.id)?.ok_or_else(||format!("prior chapter {chapter} extraction not ready"))?;prior.push((text,bundle));}
    let actual=internal_view(conn,&GraphQuery{book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(idx-1)})?;
    let spec=internal_view(conn,&GraphQuery{book_id,scope:"chapter".into(),purpose:"spec".into(),chapter_idx:Some(idx)})?;
    let previous_text=prior.last().unwrap().0.content.clone();
    let reference_text=Some(text_version(conn,book_id,idx,"original",None)?.content);
    let style_guide=conn.query_row("SELECT style_guide FROM books WHERE id=?1",[book_id],|r|r.get::<_,Option<String>>(0)).map_err(err)?;
    let records=intent::applicable(conn,book_id,idx)?;
    let directives=intent::intent_constraints(conn,book_id,idx)?["directives"].as_array().ok_or("invalid directive constraints")?.iter().map(json).collect::<Result<Vec<_>,_>>()?;
    let drift_pref=intent::drift_at(conn,book_id,idx)?;
    // No transient revision IDs or line version in content dependency identity.
    let stable_edits=records.iter().map(|r|(&r.target_type,&r.target_id,&r.action,&r.after_json,r.effective_from)).collect::<Vec<_>>();
    let reference=active_bundle(conn,text_version(conn,book_id,idx,"original",None)?.id)?;
    let stable_prior=prior.iter().map(|(t,b)|(&t.content_hash,&t.source,b)).collect::<Vec<_>>();
    let dependency=digest(json(&(book_id,idx,stable_prior,reference,stable_edits,&style_guide,drift_pref))?.as_bytes());
    Ok(GenerationContext{book_id,chapter_idx:idx,reading_line_version:line_version(conn,book_id)?,digest:dependency,actual,spec,previous_text,reference_text,style_guide,directives,drift_pref})
}
