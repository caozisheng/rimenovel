//! SQLite ownership boundary for immutable texts and replace-whole extraction revisions.
use crate::core::kg::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

mod view;
mod intent;
pub use view::{get_view,internal_view,evidence,generation_context,extraction_state};
pub use intent::{edit,revert,active_edits,divergence,drift_pref,intent_constraints};
#[cfg(test)] mod tests;
fn err(error:impl std::fmt::Display)->String {error.to_string()}
fn json<T:Serialize>(value:&T)->Result<String,String>{serde_json::to_string(value).map_err(err)}
fn parse<T:DeserializeOwned>(value:&str)->Result<T,String>{serde_json::from_str(value).map_err(err)}
fn ensure_book(conn:&Connection,book:i64)->Result<(),String>{
    if !conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE id=?1)",[book],|r|r.get::<_,bool>(0)).map_err(err)? {return Err("book not found".into());}
    conn.execute("INSERT OR IGNORE INTO kg_lines(book_id) VALUES (?1)",[book]).map_err(err)?; Ok(())
}
fn ensure_chapter(conn:&Connection,book:i64,idx:i64)->Result<(),String>{ensure_book(conn,book)?;if !conn.query_row("SELECT EXISTS(SELECT 1 FROM chapters WHERE book_id=?1 AND idx=?2)",params![book,idx],|r|r.get::<_,bool>(0)).map_err(err)? {return Err("chapter not found".into());} Ok(())}
fn text_row(r:&rusqlite::Row<'_>)->rusqlite::Result<TextVersion>{Ok(TextVersion{id:r.get(0)?,book_id:r.get(1)?,chapter_idx:r.get(2)?,source:r.get(3)?,content:r.get(4)?,content_hash:r.get(5)?,generated_id:r.get(6)?})}
fn by_id(conn:&Connection,id:i64)->Result<TextVersion,String>{conn.query_row("SELECT id,book_id,chapter_idx,source,content,content_hash,generated_id FROM kg_text_versions WHERE id=?1",[id],text_row).map_err(err)}
pub fn text_version(conn:&Connection,book_id:i64,idx:i64,source:&str,generated_id:Option<i64>)->Result<TextVersion,String>{
    ensure_chapter(conn,book_id,idx)?;
    let (content,generated_id)=match source {
        "original"=>{if generated_id.is_some(){return Err("original cannot own generated id".into());} (conn.query_row("SELECT original_text FROM chapters WHERE book_id=?1 AND idx=?2",params![book_id,idx],|r|r.get::<_,String>(0)).map_err(err)?,None)},
        "generated"=>{let id=match generated_id {Some(id)=>id,None=>conn.query_row("SELECT id FROM generated_chapters WHERE book_id=?1 AND chapter_idx=?2 AND active=1 ORDER BY id DESC LIMIT 1",params![book_id,idx],|r|r.get(0)).map_err(err)?}; let content=conn.query_row("SELECT content FROM generated_chapters WHERE id=?1 AND book_id=?2 AND chapter_idx=?3",params![id,book_id,idx],|r|r.get::<_,String>(0)).map_err(err)?;(content,Some(id))},
        _=>return Err("invalid text source".into())
    };
    let hash=digest(content.as_bytes());
    conn.execute("INSERT OR IGNORE INTO kg_text_versions(book_id,chapter_idx,source,content,content_hash,generated_id) VALUES (?1,?2,?3,?4,?5,?6)",params![book_id,idx,source,content,hash,generated_id]).map_err(err)?;
    let version=conn.query_row("SELECT id,book_id,chapter_idx,source,content,content_hash,generated_id FROM kg_text_versions WHERE book_id=?1 AND chapter_idx=?2 AND source=?3 AND generated_id IS ?4",params![book_id,idx,source,generated_id],text_row).map_err(err)?;
    if version.content!=content || (source=="generated" && version.generated_id!=generated_id) {return Err("text identity/content ownership conflict".into());} Ok(version)
}
pub fn progress(conn:&Connection,book_id:i64)->Result<i64,String>{ensure_book(conn,book_id)?;conn.query_row("SELECT progress FROM kg_lines WHERE book_id=?1",[book_id],|r|r.get(0)).map_err(err)}
pub fn line_version(conn:&Connection,book_id:i64)->Result<i64,String>{ensure_book(conn,book_id)?;conn.query_row("SELECT version FROM kg_lines WHERE book_id=?1",[book_id],|r|r.get(0)).map_err(err)}
pub fn mark_read(conn:&Connection,book_id:i64,idx:i64)->Result<(),String>{
    ensure_chapter(conn,book_id,idx)?;let tx=conn.unchecked_transaction().map_err(err)?;
    for chapter in 1..=idx {let text=selected_text(&tx,book_id,chapter)?.ok_or("selected chapter text unavailable")?;tx.execute("INSERT OR IGNORE INTO kg_read_selection(book_id,chapter_idx,text_version_id) VALUES (?1,?2,?3)",params![book_id,chapter,text.id]).map_err(err)?;}
    tx.execute("UPDATE kg_lines SET progress=max(progress,?2),revealed_idx=CASE WHEN revealed_idx<=?2 THEN NULL ELSE revealed_idx END WHERE book_id=?1",params![book_id,idx]).map_err(err)?;tx.commit().map_err(err)
}
pub fn reveal(conn:&Connection,book_id:i64,idx:i64)->Result<(),String>{ensure_chapter(conn,book_id,idx)?;if idx!=progress(conn,book_id)?+1 {return Err("only next chapter can be revealed".into());}conn.execute("UPDATE kg_lines SET revealed_idx=?2 WHERE book_id=?1",params![book_id,idx]).map_err(err)?;Ok(())}
pub fn selected_text(conn:&Connection,book_id:i64,idx:i64)->Result<Option<TextVersion>,String>{
    ensure_chapter(conn,book_id,idx)?;
    if let Some(id)=conn.query_row("SELECT text_version_id FROM kg_read_selection WHERE book_id=?1 AND chapter_idx=?2",params![book_id,idx],|r|r.get::<_,i64>(0)).optional().map_err(err)?{return by_id(conn,id).map(Some);}
    if divergence(conn,book_id)?.is_none_or(|d|idx<d) || idx==1 {return text_version(conn,book_id,idx,"original",None).map(Some);}
    let id=conn.query_row("SELECT id FROM generated_chapters WHERE book_id=?1 AND chapter_idx=?2 AND active=1 ORDER BY id DESC LIMIT 1",params![book_id,idx],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    id.map(|id|text_version(conn,book_id,idx,"generated",Some(id))).transpose()
}
pub fn ready(conn:&Connection,text_version_id:i64)->Result<bool,String>{conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_revisions WHERE text_version_id=?1 AND active=1 AND complete=1)",[text_version_id],|r|r.get(0)).map_err(err)}
fn revision_bundle(conn:&Connection,revision:i64)->Result<ChapterBundle,String>{
    let (idx,a,b,summary,threads,unresolved)=conn.query_row("SELECT t.chapter_idx,r.stage_a_version,r.stage_b_version,r.summary,r.open_threads_json,r.unresolved_json FROM kg_revisions r JOIN kg_text_versions t ON t.id=r.text_version_id WHERE r.id=?1",[revision],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,String>(5)?))).map_err(err)?;
    let mut bundle=ChapterBundle{stage_a:StageA{schema_version:a,chapter_idx:idx,summary,open_threads:parse(&threads)?,mentions:vec![],events:vec![]},stage_b:StageB{schema_version:b,chapter_idx:idx,facts:vec![],relations:vec![],changes:vec![],preconditions:vec![],unresolved:parse(&unresolved)?},identities:vec![],spans:vec![],identity_links:vec![]};
    let mut stmt=conn.prepare("SELECT kind,data_json FROM kg_records WHERE revision_id=?1 ORDER BY id").map_err(err)?;
    let records=stmt.query_map([revision],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?))).map_err(err)?;
    for row in records {let(kind,data)=row.map_err(err)?;match kind.as_str(){"identity"=>bundle.identities.push(parse(&data)?),"mention"=>bundle.stage_a.mentions.push(parse(&data)?),"fact"=>bundle.stage_b.facts.push(parse(&data)?),"relation"=>bundle.stage_b.relations.push(parse(&data)?),"change"=>bundle.stage_b.changes.push(parse(&data)?),"link"=>bundle.identity_links.push(parse(&data)?),"precondition"=>bundle.stage_b.preconditions.push(parse(&data)?),_=>return Err("invalid stored record kind".into())}}
    let mut stmt=conn.prepare("SELECT key,chapter_idx,start_byte,end_byte,text FROM kg_spans WHERE revision_id=?1 ORDER BY id").map_err(err)?;
    bundle.spans=stmt.query_map([revision],|r|Ok(Span{key:r.get(0)?,chapter_idx:r.get(1)?,start_byte:r.get::<_,i64>(2)? as usize,end_byte:r.get::<_,i64>(3)? as usize,text:r.get(4)?})).map_err(err)?.collect::<Result<_,_>>().map_err(err)?;
    let mut stmt=conn.prepare("SELECT data_json FROM graph_events WHERE revision_id=?1 ORDER BY id").map_err(err)?;
    for row in stmt.query_map([revision],|r|r.get::<_,String>(0)).map_err(err)? {bundle.stage_a.events.push(parse(&row.map_err(err)?)?);}
    Ok(bundle)
}
pub fn active_bundle(conn:&Connection,text_version_id:i64)->Result<Option<ChapterBundle>,String>{let revision=conn.query_row("SELECT id FROM kg_revisions WHERE text_version_id=?1 AND active=1 AND complete=1",[text_version_id],|r|r.get::<_,i64>(0)).optional().map_err(err)?;revision.map(|id|revision_bundle(conn,id)).transpose()}
pub fn active_revision(conn:&Connection,text_version_id:i64)->Result<Option<i64>,String>{conn.query_row("SELECT id FROM kg_revisions WHERE text_version_id=?1 AND active=1 AND complete=1",[text_version_id],|r|r.get(0)).optional().map_err(err)}
fn registry(conn:&Connection,book:i64)->Result<Vec<Identity>,String>{let mut stmt=conn.prepare("SELECT key,entity_type FROM kg_entities WHERE book_id=?1").map_err(err)?;let rows=stmt.query_map([book],|r|Ok(Identity{key:r.get(0)?,entity_type:r.get(1)?,name:String::new(),aliases:vec![]})).map_err(err)?;rows.collect::<Result<_,_>>().map_err(err)}
fn insert_record<T:Serialize>(conn:&Connection,revision:i64,kind:&str,key:&str,subject:Option<&str>,chapter:i64,order:i64,value:&T)->Result<(),String>{conn.execute("INSERT INTO kg_records(revision_id,kind,key,subject_key,observed_chapter,observed_order,data_json) VALUES (?1,?2,?3,?4,?5,?6,?7)",params![revision,kind,key,subject,chapter,order,json(value)?]).map_err(err)?;Ok(())}
pub fn commit_bundle(conn:&Connection,text:&TextVersion,bundle:&ChapterBundle,dependency_digest:&str,expected_line_version:i64,job_id:Option<i64>)->Result<i64,String>{commit(conn,text,bundle,dependency_digest,expected_line_version,job_id,None)}
pub fn commit_bundle_with_config(conn:&Connection,text:&TextVersion,bundle:&ChapterBundle,dependency_digest:&str,expected_line_version:i64,job_id:Option<i64>,config:&RuntimeConfig)->Result<i64,String>{commit(conn,text,bundle,dependency_digest,expected_line_version,job_id,Some(config))}
fn commit(conn:&Connection,text:&TextVersion,bundle:&ChapterBundle,dependency_digest:&str,expected_line_version:i64,job_id:Option<i64>,config:Option<&RuntimeConfig>)->Result<i64,String>{
    let stored=by_id(conn,text.id)?;if &stored!=text{return Err("immutable text source/id conflict".into());}
    let prior=extraction_state(conn,text.book_id,&text.source,text.chapter_idx-1)?.relations.into_values().collect::<Vec<_>>();
    validate_bundle(text,bundle,&registry(conn,text.book_id)?,&prior)?;
    let tx=conn.unchecked_transaction().map_err(err)?;
    if text.source=="generated" {check_line(&tx,text.book_id,expected_line_version)?;let selected=selected_text(&tx,text.book_id,text.chapter_idx)?.ok_or("generated text not selected")?;if selected.id!=text.id{return Err("generated revision belongs to unselected text".into());}}
    let fingerprint=digest(json(bundle)?.as_bytes());
    let previous=tx.query_row("SELECT digest FROM kg_revisions WHERE text_version_id=?1 AND active=1",[text.id],|r|r.get::<_,String>(0)).optional().map_err(err)?;
    if previous.as_deref()!=Some(fingerprint.as_str()){
        tx.execute("UPDATE kg_revisions SET active=0 WHERE text_version_id IN(SELECT id FROM kg_text_versions WHERE book_id=?1 AND chapter_idx>?2 AND source=?3)",params![text.book_id,text.chapter_idx,text.source]).map_err(err)?;
        tx.execute("UPDATE kg_specs SET active=0 WHERE book_id=?1 AND chapter_idx>?2",params![text.book_id,text.chapter_idx]).map_err(err)?;
        if text.source=="generated" || selected_text(&tx,text.book_id,text.chapter_idx)?.is_some_and(|selected|selected.id==text.id){invalidate_generated_from(&tx,text.book_id,text.chapter_idx+1)?;}
        if text.source=="original"{tx.execute("UPDATE books SET synopsis=NULL,style_guide=NULL,meta_json=json_remove(coalesce(meta_json,'{}'),'$.themes') WHERE id=?1",[text.book_id]).map_err(err)?;}
    }
    let old=tx.query_row("SELECT id FROM kg_revisions WHERE text_version_id=?1 AND digest=?2",params![text.id,fingerprint],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    tx.execute("UPDATE kg_revisions SET active=0 WHERE text_version_id=?1",[text.id]).map_err(err)?;
    let revision=if let Some(id)=old {tx.execute("UPDATE kg_revisions SET active=1,dependency_digest=?2 WHERE id=?1",params![id,dependency_digest]).map_err(err)?;id} else {
        tx.execute("INSERT INTO kg_revisions(book_id,text_version_id,digest,dependency_digest,stage_a_version,stage_b_version,summary,open_threads_json,unresolved_json,active,schema_digest) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,1,?10)",params![text.book_id,text.id,fingerprint,dependency_digest,bundle.stage_a.schema_version,bundle.stage_b.schema_version,bundle.stage_a.summary,json(&bundle.stage_a.open_threads)?,json(&bundle.stage_b.unresolved)?,schema_digest()]).map_err(err)?;
        let id=tx.last_insert_rowid();
        for identity in &bundle.identities {tx.execute("INSERT OR IGNORE INTO kg_entities(book_id,key,entity_type) VALUES (?1,?2,?3)",params![text.book_id,identity.key,identity.entity_type]).map_err(err)?;insert_record(&tx,id,"identity",&identity.key,Some(&identity.key),text.chapter_idx,0,identity)?;}
        for span in &bundle.spans {let start=i64::try_from(span.start_byte).map_err(err)?;let end=i64::try_from(span.end_byte).map_err(err)?;tx.execute("INSERT INTO kg_spans(revision_id,key,chapter_idx,start_byte,end_byte,text) VALUES (?1,?2,?3,?4,?5,?6)",params![id,span.key,span.chapter_idx,start,end,span.text]).map_err(err)?;}
        for mention in &bundle.stage_a.mentions {insert_record(&tx,id,"mention",&mention.mention_id,None,text.chapter_idx,0,mention)?;}
        for fact in &bundle.stage_b.facts {insert_record(&tx,id,"fact",&fact.key,Some(&fact.subject_key),fact.observed_chapter,fact.observed_order,fact)?;}
        for relation in &bundle.stage_b.relations {insert_record(&tx,id,"relation",&relation.key,Some(&relation.src),relation.observed_chapter,relation.observed_order,relation)?;}
        for change in &bundle.stage_b.changes {insert_record(&tx,id,"change",&change.key,Some(&change.subject_key),change.observed_chapter,change.observed_order,change)?;}
        for p in &bundle.stage_b.preconditions {let event=bundle.stage_a.events.iter().find(|e|e.key==p.event_key).ok_or("missing precondition event")?;insert_record(&tx,id,"precondition",&p.key,Some(&p.subject_key),event.observed_chapter,event.observed_order,p)?;}
        for (index,link) in bundle.identity_links.iter().enumerate() {insert_record(&tx,id,"link",&format!("link-{index}"),Some(&link.from_key),link.observed_chapter,link.observed_order,link)?;}
        for event in &bundle.stage_a.events {tx.execute("INSERT INTO graph_events(book_id,scope,chapter_idx,order_in_chapter,title,participant_keys_json,location_key,summary,outcome,provenance,revision_id,record_key,observed_chapter,observed_order,data_json) VALUES (?1,'chapter',?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14)",params![text.book_id,event.chapter_idx,event.observed_order,event.title,json(&event.participants)?,event.location,event.summary,event.outcome,if text.source=="original"{"ai_original"}else{"ai_generated"},id,event.key,event.observed_chapter,event.observed_order,json(event)?]).map_err(err)?;}
        id
    };
    tx.execute("UPDATE kg_revisions SET prior_digest=?2 WHERE id=?1",params![revision,prior_digest(&tx,text)?]).map_err(err)?;
    if let Some(config)=config {tx.execute("UPDATE kg_revisions SET config_digest=?2 WHERE id=?1",params![revision,extraction_config_digest(config)?]).map_err(err)?;}
    if let Some(job)=job_id {
        let (owner,payload)=tx.query_row("SELECT book_id,payload_json FROM jobs WHERE id=?1",[job],|r|Ok((r.get::<_,Option<i64>>(0)?,r.get::<_,Option<String>>(1)?))).map_err(err)?;if owner!=Some(text.book_id){return Err("foreign job checkpoint".into());}
        let mut payload:Value=payload.as_deref().map(parse).transpose()?.unwrap_or_else(||serde_json::json!({}));if !payload.is_object(){return Err("invalid job checkpoint payload".into());}
        let revisions=payload.as_object_mut().unwrap().entry("committed_revisions").or_insert_with(||serde_json::json!({}));revisions.as_object_mut().ok_or("invalid checkpoint revisions")?.insert(text.chapter_idx.to_string(),serde_json::json!({"text_version_id":text.id,"revision":revision,"dependency_digest":dependency_digest}));
        let total:i64=tx.query_row("SELECT COUNT(*) FROM chapters WHERE book_id=?1",[text.book_id],|r|r.get(0)).map_err(err)?;let missing=missing_original_chapters(&tx,text.book_id)?.len();
        tx.execute("UPDATE jobs SET payload_json=?2,progress=?3,updated_at=datetime('now') WHERE id=?1",params![job,json(&payload)?,if total>0 {(total as usize-missing) as f64/total as f64}else{0.0}]).map_err(err)?;
    }
    tx.commit().map_err(err)?;Ok(revision)
}
fn check_line(conn:&Connection,book:i64,expected:i64)->Result<(),String>{if line_version(conn,book)?!=expected {Err("reading line changed; stale result cannot activate".into())}else{Ok(())}}
pub fn missing_original_chapters(conn:&Connection,book_id:i64)->Result<Vec<i64>,String>{ensure_book(conn,book_id)?;let mut stmt=conn.prepare("SELECT c.idx FROM chapters c WHERE c.book_id=?1 AND NOT EXISTS(SELECT 1 FROM kg_text_versions t JOIN kg_revisions r ON r.text_version_id=t.id AND r.active=1 AND r.complete=1 WHERE t.book_id=c.book_id AND t.chapter_idx=c.idx AND t.source='original') ORDER BY c.idx").map_err(err)?;let rows=stmt.query_map([book_id],|r|r.get(0)).map_err(err)?;rows.collect::<Result<_,_>>().map_err(err)}
pub fn job_checkpoint(conn:&Connection,job_id:i64)->Result<Value,String>{let payload=conn.query_row("SELECT payload_json FROM jobs WHERE id=?1",[job_id],|r|r.get::<_,Option<String>>(0)).map_err(err)?;payload.as_deref().map(parse).transpose().map(|p|p.unwrap_or_else(||serde_json::json!({})))}
pub fn save_generated(conn:&Connection,book_id:i64,idx:i64,content:&str,model:&str,dependency_digest:&str,expected_line_version:i64)->Result<TextVersion,String>{
    ensure_chapter(conn,book_id,idx)?;if idx==1 || divergence(conn,book_id)?.is_none_or(|d|idx<d){return Err("chapter does not belong to generated reading line".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,book_id,expected_line_version)?;
    let hash=digest(content.as_bytes());
    let existing=tx.query_row("SELECT t.generated_id FROM kg_text_versions t JOIN generated_chapters g ON g.id=t.generated_id WHERE t.book_id=?1 AND t.chapter_idx=?2 AND t.source='generated' AND t.content_hash=?3 AND g.prompt_digest=?4 AND g.model=?5 ORDER BY t.id DESC LIMIT 1",params![book_id,idx,hash,dependency_digest,model],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    let id=if let Some(id)=existing {id} else {tx.execute("INSERT INTO generated_chapters(book_id,chapter_idx,content,model,prompt_digest,active,line_version) VALUES (?1,?2,?3,?4,?5,0,?6)",params![book_id,idx,content,model,dependency_digest,expected_line_version]).map_err(err)?;tx.last_insert_rowid()};
    let text=text_version(&tx,book_id,idx,"generated",Some(id))?;select_generated(&tx,&text,expected_line_version)?;tx.commit().map_err(err)?;Ok(text)
}
pub fn cached_generated(conn:&Connection,book_id:i64,idx:i64,dependency_digest:&str)->Result<Option<TextVersion>,String>{
    ensure_chapter(conn,book_id,idx)?;let id=conn.query_row("SELECT id FROM generated_chapters WHERE book_id=?1 AND chapter_idx=?2 AND prompt_digest=?3 ORDER BY id DESC LIMIT 1",params![book_id,idx,dependency_digest],|r|r.get::<_,i64>(0)).optional().map_err(err)?;id.map(|id|text_version(conn,book_id,idx,"generated",Some(id))).transpose()
}
fn invalidate_generated_from(conn:&Connection,book:i64,idx:i64)->Result<(),String>{
    conn.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;
    conn.execute("UPDATE kg_revisions SET active=0 WHERE text_version_id IN(SELECT id FROM kg_text_versions WHERE book_id=?1 AND chapter_idx>=?2 AND source='generated')",params![book,idx]).map_err(err)?;
    conn.execute("DELETE FROM kg_read_selection WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;
    conn.execute("UPDATE kg_specs SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;Ok(())
}
fn select_generated(conn:&Connection,text:&TextVersion,line:i64)->Result<(),String>{
    let previous=selected_text(conn,text.book_id,text.chapter_idx)?;
    if previous.as_ref().is_none_or(|old|old.id!=text.id){invalidate_generated_from(conn,text.book_id,text.chapter_idx+1)?;}
    conn.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![text.book_id,text.chapter_idx]).map_err(err)?;
    conn.execute("UPDATE generated_chapters SET active=1,line_version=?2 WHERE id=?1",params![text.generated_id,line]).map_err(err)?;
    conn.execute("UPDATE kg_read_selection SET text_version_id=?3 WHERE book_id=?1 AND chapter_idx=?2",params![text.book_id,text.chapter_idx,text.id]).map_err(err)?;Ok(())
}
pub fn save_spec_with_preconditions(conn:&Connection,book_id:i64,idx:i64,spec:&GraphView,preconditions:&[Precondition],dependency_digest:&str,expected_line_version:i64)->Result<(),String>{
    ensure_chapter(conn,book_id,idx)?;if spec.purpose!="spec" || spec.chapter_idx!=Some(idx){return Err("spec ownership mismatch".into());}
    view::validate_spec(conn,book_id,idx,spec)?;
    let nodes=spec.nodes.iter().map(|n|n.key.as_str()).collect::<std::collections::HashSet<_>>();let edges=spec.edges.iter().map(|e|e.key.as_str()).collect::<std::collections::HashSet<_>>();let events=spec.events.iter().map(|e|e.key.as_str()).collect::<std::collections::HashSet<_>>();let mut seen=std::collections::HashSet::new();
    for p in preconditions {if !seen.insert(p.key.as_str())||!events.contains(p.event_key.as_str())||if p.predicate=="status"{!edges.contains(p.subject_key.as_str())}else{!nodes.contains(p.subject_key.as_str())}{return Err("invalid spec precondition reference".into());}if !["life_status","location","affiliation","owner","status"].contains(&p.predicate.as_str())||!["confirmed","reported","inferred"].contains(&p.assertion_kind.as_str()){return Err("invalid spec precondition vocabulary".into());}validate_value(&p.value)?;predicate_value(&p.predicate,&p.value)?;if p.value.kind=="entity_ref"&&!nodes.contains(p.value.value.as_deref().unwrap()){return Err("unknown spec precondition value identity".into());}}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,book_id,expected_line_version)?;
    tx.execute("UPDATE kg_specs SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![book_id,idx]).map_err(err)?;
    tx.execute("INSERT INTO kg_specs(book_id,chapter_idx,dependency_digest,line_version,data_json,preconditions_json,active) VALUES (?1,?2,?3,?4,?5,?6,1) ON CONFLICT(book_id,chapter_idx,dependency_digest) DO UPDATE SET data_json=excluded.data_json,preconditions_json=excluded.preconditions_json,line_version=excluded.line_version,active=1",params![book_id,idx,dependency_digest,expected_line_version,json(spec)?,json(&preconditions)?]).map_err(err)?;
    tx.commit().map_err(err)
}
pub fn spec_matches(conn:&Connection,book_id:i64,idx:i64,dependency_digest:&str,expected_line_version:i64)->Result<bool,String>{
    let id=conn.query_row("SELECT id FROM kg_specs WHERE book_id=?1 AND chapter_idx=?2 AND dependency_digest=?3",params![book_id,idx,dependency_digest],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    let Some(id)=id else{return Ok(false);};let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,book_id,expected_line_version)?;
    tx.execute("UPDATE kg_specs SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![book_id,idx]).map_err(err)?;
    tx.execute("UPDATE kg_specs SET active=1,line_version=?2 WHERE id=?1",params![id,expected_line_version]).map_err(err)?;tx.commit().map_err(err)?;Ok(true)
}
pub fn spec_preconditions(conn:&Connection,book_id:i64,idx:i64)->Result<Vec<Precondition>,String>{
    let value:Option<String>=conn.query_row("SELECT preconditions_json FROM kg_specs WHERE book_id=?1 AND chapter_idx=?2 AND active=1",params![book_id,idx],|r|r.get(0)).optional().map_err(err)?;
    value.as_deref().map(parse).transpose().map(|v|v.unwrap_or_default())
}
// Bump when deterministic identity/refinement/qualification rules change even if
// the model's output schema is unchanged. Old revisions then require re-extraction.
const REFINEMENT_VERSION: &str = "kg-refinement/v2-evidence-identity-continuity";
fn schema_digest()->String{digest(concat!("kg-refinement/v2-evidence-identity-continuity",include_str!("../../schemas/kg-stage-a.schema.json"),include_str!("../../schemas/kg-stage-b.schema.json"),include_str!("../../schemas/kg-identity.schema.json"),include_str!("../../schemas/kg-adjudication.schema.json")).as_bytes())}
fn extraction_config_digest(config:&RuntimeConfig)->Result<String,String>{
    Ok(digest(json(&(config,REFINEMENT_VERSION,include_str!("../../schemas/kg-stage-a.schema.json"),include_str!("../../schemas/kg-stage-b.schema.json"),include_str!("../../schemas/kg-identity.schema.json"),include_str!("../../schemas/kg-adjudication.schema.json")))?.as_bytes()))
}
fn prior_digest(conn:&Connection,text:&TextVersion)->Result<String,String>{
    let mut dependencies=Vec::new();
    for idx in 1..text.chapter_idx {
        let prior=if text.source=="original"{Some(text_version(conn,text.book_id,idx,"original",None)?)}else{selected_text(conn,text.book_id,idx)?};
        let record=prior.map(|t|{let bundle=active_bundle(conn,t.id)?;Ok::<_,String>((t.content_hash,t.source,bundle))}).transpose()?;
        dependencies.push((idx,record));
    }Ok(digest(json(&dependencies)?.as_bytes()))
}
pub fn extraction_matches(conn:&Connection,text_version_id:i64,config:&RuntimeConfig)->Result<bool,String>{let text=by_id(conn,text_version_id)?;conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_revisions WHERE text_version_id=?1 AND complete=1 AND active=1 AND config_digest=?2 AND prior_digest=?3)",params![text_version_id,extraction_config_digest(config)?,prior_digest(conn,&text)?],|r|r.get(0)).map_err(err)}
pub fn activate_cached(conn:&Connection,text_version_id:i64,dependency_digest:&str,expected_line_version:i64)->Result<(),String>{activate_cache(conn,text_version_id,dependency_digest,expected_line_version,None)}
pub fn activate_cached_for_config(conn:&Connection,text_version_id:i64,dependency_digest:&str,expected_line_version:i64,config:&RuntimeConfig)->Result<(),String>{activate_cache(conn,text_version_id,dependency_digest,expected_line_version,Some(config))}
fn activate_cache(conn:&Connection,text_version_id:i64,dependency_digest:&str,expected_line_version:i64,config:Option<&RuntimeConfig>)->Result<(),String>{
    let text=by_id(conn,text_version_id)?;if text.source!="generated"{return Err("cache activation requires generated text".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,text.book_id,expected_line_version)?;
    if divergence(&tx,text.book_id)?.is_none_or(|d|text.chapter_idx<d){return Err("generated cache not eligible on this reading line".into());}
    let stored_digest:String=tx.query_row("SELECT prompt_digest FROM generated_chapters WHERE id=?1 AND book_id=?2 AND chapter_idx=?3",params![text.generated_id,text.book_id,text.chapter_idx],|r|r.get(0)).map_err(err)?;
    if stored_digest!=dependency_digest || digest(text.content.as_bytes())!=text.content_hash {return Err("cache dependency or content mismatch".into());}
    select_generated(&tx,&text,expected_line_version)?;
    tx.execute("UPDATE kg_revisions SET active=0 WHERE text_version_id=?1",[text.id]).map_err(err)?;
    if let Some(config)=config {
        let revision=tx.query_row("SELECT id FROM kg_revisions WHERE text_version_id=?1 AND complete=1 AND config_digest=?2 AND schema_digest=?3 AND prior_digest=?4 ORDER BY id DESC LIMIT 1",params![text.id,extraction_config_digest(config)?,schema_digest(),prior_digest(&tx,&text)?],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
        if let Some(id)=revision {tx.execute("UPDATE kg_revisions SET active=1 WHERE id=?1",[id]).map_err(err)?;}
    }
    tx.commit().map_err(err)
}
pub fn record_actual_drift(conn:&Connection,text_version_id:i64,diff:f64,expected_line_version:i64)->Result<f64,String>{
    if !diff.is_finite() || !(0.0..=1.0).contains(&diff){return Err("invalid actual drift".into());}
    let text=by_id(conn,text_version_id)?;if text.source!="generated" || !ready(conn,text.id)?{return Err("drift requires generated actual extraction".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,text.book_id,expected_line_version)?;if selected_text(&tx,text.book_id,text.chapter_idx)?.is_none_or(|t|t.id!=text.id){return Err("drift text not selected".into());}
    tx.execute("INSERT INTO kg_actual_drift(text_version_id,diff) VALUES (?1,?2) ON CONFLICT(text_version_id) DO UPDATE SET diff=max(diff,excluded.diff)",params![text.id,diff]).map_err(err)?;
    let score=actual_drift(&tx,text.book_id,text.chapter_idx)?;tx.commit().map_err(err)?;Ok(score)
}
fn actual_drift(conn:&Connection,book:i64,upto:i64)->Result<f64,String>{let mut stmt=conn.prepare("SELECT d.diff FROM kg_actual_drift d JOIN kg_text_versions t ON t.id=d.text_version_id JOIN generated_chapters g ON g.id=t.generated_id WHERE t.book_id=?1 AND t.chapter_idx<=?2 AND g.active=1 ORDER BY t.chapter_idx").map_err(err)?;let rows=stmt.query_map(params![book,upto],|r|r.get::<_,f64>(0)).map_err(err)?;let mut residual=1.0;for row in rows{residual*=1.0-row.map_err(err)?;}Ok(1.0-residual)}
pub fn original_summaries(conn:&Connection,book_id:i64)->Result<Vec<(i64,String)>,String>{let mut stmt=conn.prepare("SELECT t.chapter_idx,r.summary FROM kg_revisions r JOIN kg_text_versions t ON t.id=r.text_version_id WHERE t.book_id=?1 AND t.source='original' AND r.active=1 AND r.complete=1 ORDER BY t.chapter_idx").map_err(err)?;let rows=stmt.query_map([book_id],|r|Ok((r.get(0)?,r.get(1)?))).map_err(err)?;rows.collect::<Result<_,_>>().map_err(err)}
pub fn save_original_summary(conn:&Connection,book_id:i64,synopsis:&str,themes:&[String],style_guide:&str)->Result<(),String>{ensure_book(conn,book_id)?;if !missing_original_chapters(conn,book_id)?.is_empty(){return Err("original extraction incomplete".into());}conn.execute("UPDATE books SET synopsis=?2,meta_json=json_set(coalesce(meta_json,'{}'),'$.themes',json(?3)),style_guide=?4 WHERE id=?1",params![book_id,synopsis,json(&themes)?,style_guide]).map_err(err)?;Ok(())}
pub fn identities(conn:&Connection,book_id:i64,source:&str,upto:i64)->Result<Vec<Identity>,String>{let purpose=match source{"original"=>"reference","generated"=>"actual",_=>return Err("invalid identity source".into())};let graph=internal_view(conn,&GraphQuery{book_id,scope:"global".into(),purpose:purpose.into(),chapter_idx:Some(upto)})?;Ok(graph.nodes.into_iter().map(|n|Identity{key:n.key,entity_type:n.entity_type,name:n.name,aliases:n.aliases}).collect())}
