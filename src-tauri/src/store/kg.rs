//! SQLite ownership boundary for immutable texts and replace-whole extraction revisions.
use crate::core::kg::*;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

mod view;
mod intent;
pub use view::{get_view,internal_view,generation_context};
pub use intent::{edit,revert,active_edits,divergence,drift_pref,intent_constraints};
#[cfg(test)] mod tests;
#[cfg(test)] mod intent_tests;
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
    // Original jumps do not materialize skipped text versions. After divergence,
    // every chosen generated predecessor must remain available and pinned.
    let start=divergence(&tx,book_id)?.filter(|from|*from<=idx).unwrap_or(idx);
    for chapter in start..=idx {let text=selected_text(&tx,book_id,chapter)?.ok_or("selected chapter text unavailable")?;tx.execute("INSERT OR IGNORE INTO kg_read_selection(book_id,chapter_idx,text_version_id) VALUES (?1,?2,?3)",params![book_id,chapter,text.id]).map_err(err)?;}
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
pub fn active_generated_text(conn:&Connection,book_id:i64,idx:i64)->Result<Option<TextVersion>,String>{
    let id=conn.query_row("SELECT id FROM generated_chapters WHERE book_id=?1 AND chapter_idx=?2 AND active=1 ORDER BY id DESC LIMIT 1",params![book_id,idx],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    id.map(|id|text_version(conn,book_id,idx,"generated",Some(id))).transpose()
}
pub fn has_generated_before(conn:&Connection,book_id:i64,idx:i64)->Result<bool,String>{
    conn.query_row("SELECT EXISTS(SELECT 1 FROM generated_chapters WHERE book_id=?1 AND chapter_idx<?2 AND active=1)",params![book_id,idx],|r|r.get(0)).map_err(err)
}
pub fn ready(conn:&Connection,text_version_id:i64)->Result<bool,String>{conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_lite_snapshots WHERE text_version_id=?1 AND active=1 AND legacy=0)",[text_version_id],|r|r.get(0)).map_err(err)}
pub fn active_snapshot(conn:&Connection,text_version_id:i64)->Result<Option<ChapterSnapshot>,String>{let data=conn.query_row("SELECT data_json FROM kg_lite_snapshots WHERE text_version_id=?1 AND active=1",[text_version_id],|r|r.get::<_,String>(0)).optional().map_err(err)?;data.as_deref().map(parse).transpose()}
pub fn active_revision(conn:&Connection,text_version_id:i64)->Result<Option<i64>,String>{conn.query_row("SELECT id FROM kg_lite_snapshots WHERE text_version_id=?1 AND active=1",[text_version_id],|r|r.get(0)).optional().map_err(err)}
// Replacement of one immutable text retains only unambiguous exact record matches.
// This is target-ID continuity, not semantic identity merging across chapters.
fn retain_snapshot_keys<'a>(fresh:&'a ChapterSnapshot,old:Option<&ChapterSnapshot>)->std::borrow::Cow<'a,ChapterSnapshot>{
    let Some(old)=old else{return std::borrow::Cow::Borrowed(fresh);};let mut out=fresh.clone();let mut remap=std::collections::HashMap::new();
    for n in &fresh.nodes{let mut matching=old.nodes.iter().filter(|p|p.entity_type==n.entity_type&&p.name==n.name);if let Some(prior)=matching.next(){if matching.next().is_none()&&fresh.nodes.iter().filter(|p|p.entity_type==n.entity_type&&p.name==n.name).count()==1{remap.insert(n.key.clone(),prior.key.clone());}}}
    let rewrite=|key:&mut String|{if let Some(target)=remap.get(key){*key=target.clone();}};
    for n in &mut out.nodes{rewrite(&mut n.key);if let Some(k)=&mut n.location{rewrite(k);}if let Some(k)=&mut n.owner{rewrite(k);}}
    for e in &mut out.edges{rewrite(&mut e.src);rewrite(&mut e.dst);}
    for e in &mut out.events{for k in &mut e.participants{rewrite(k);}if let Some(k)=&mut e.location{rewrite(k);}}
    for index in 0..out.edges.len(){let e=&out.edges[index];let mut matching=old.edges.iter().filter(|p|p.src==e.src&&p.dst==e.dst&&p.rel_type==e.rel_type);let retained=matching.next().filter(|_|matching.next().is_none()&&out.edges.iter().filter(|p|p.src==e.src&&p.dst==e.dst&&p.rel_type==e.rel_type).count()==1).map(|e|e.key.clone());if let Some(key)=retained{out.edges[index].key=key;}}
    for index in 0..out.events.len(){let e=&out.events[index];let mut matching=old.events.iter().filter(|p|p.chapter_idx==e.chapter_idx&&p.order==e.order&&p.title==e.title);let retained=matching.next().filter(|_|matching.next().is_none()&&out.events.iter().filter(|p|p.chapter_idx==e.chapter_idx&&p.order==e.order&&p.title==e.title).count()==1).map(|e|e.key.clone());if let Some(key)=retained{out.events[index].key=key;}}
    std::borrow::Cow::Owned(out)
}
pub fn commit_snapshot(conn:&Connection,text:&TextVersion,snapshot:&ChapterSnapshot,dependency_digest:&str,expected_line_version:i64,job_id:Option<i64>,config:&RuntimeConfig)->Result<i64,String>{
    if by_id(conn,text.id)?!=*text{return Err("immutable text source/id conflict".into());}
    let retained=retain_snapshot_keys(snapshot,active_snapshot(conn,text.id)?.as_ref());let snapshot=&retained;validate_snapshot(snapshot)?;
    let provenance=if text.source=="original"{"ai_original"}else{"ai_generated"};
    if snapshot.nodes.iter().any(|n|n.chapter_idx>text.chapter_idx||!["ai_original","ai_generated","user"].contains(&n.provenance.as_str()))||snapshot.edges.iter().any(|e|e.chapter_idx!=text.chapter_idx||e.provenance!=provenance)||snapshot.events.iter().any(|e|e.chapter_idx!=text.chapter_idx||e.provenance!=provenance){return Err("snapshot ownership mismatch".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,text.book_id,expected_line_version)?;
    if text.source=="generated" && selected_text(&tx,text.book_id,text.chapter_idx)?.is_none_or(|t|t.id!=text.id){return Err("generated revision belongs to unselected text".into());}
    let checkpoint=if let Some(job)=job_id{let(owner,state,payload)=tx.query_row("SELECT book_id,state,payload_json FROM jobs WHERE id=?1",[job],|r|Ok((r.get::<_,Option<i64>>(0)?,r.get::<_,String>(1)?,r.get::<_,Option<String>>(2)?))).map_err(err)?;if owner!=Some(text.book_id){return Err("foreign job checkpoint".into());}if !["queued","running","paused"].contains(&state.as_str()){return Err("terminal job cannot commit extraction".into());}let payload:Value=payload.as_deref().map(parse).transpose()?.unwrap_or_else(||serde_json::json!({}));if !payload.is_object(){return Err("invalid job checkpoint payload".into());}Some((job,payload))}else{None};
    let data=json(snapshot)?;let fingerprint=digest(data.as_bytes());let config_digest=extraction_config_digest(config)?;
    let previous=tx.query_row("SELECT digest FROM kg_lite_snapshots WHERE text_version_id=?1 AND active=1",[text.id],|r|r.get::<_,String>(0)).optional().map_err(err)?;
    if previous.as_deref()!=Some(fingerprint.as_str()){
        // Sparse reference samples are independent. Only changed chosen story invalidates writing caches.
        tx.execute("UPDATE kg_lite_specs SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![text.book_id,text.chapter_idx]).map_err(err)?;
        if text.source=="generated" || previous.is_some() && selected_text(&tx,text.book_id,text.chapter_idx)?.is_some_and(|t|t.id==text.id){invalidate_generated_from(&tx,text.book_id,text.chapter_idx+1)?;}
    }
    let prior=prior_digest(&tx,text)?;
    tx.execute("UPDATE kg_lite_snapshots SET active=0 WHERE text_version_id=?1",[text.id]).map_err(err)?;
    tx.execute("INSERT INTO kg_lite_snapshots(book_id,text_version_id,digest,dependency_digest,config_digest,schema_digest,prior_digest,data_json,legacy,active) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,0,1) ON CONFLICT(text_version_id,digest,config_digest) DO UPDATE SET dependency_digest=excluded.dependency_digest,prior_digest=excluded.prior_digest,active=1",params![text.book_id,text.id,fingerprint,dependency_digest,config_digest,schema_digest(),prior,data]).map_err(err)?;
    let revision=active_revision(&tx,text.id)?.ok_or("snapshot did not activate")?;
    if let Some((job,mut payload))=checkpoint{
        payload.as_object_mut().unwrap().entry("committed_revisions").or_insert_with(||serde_json::json!({})).as_object_mut().ok_or("invalid checkpoint revisions")?.insert(text.chapter_idx.to_string(),serde_json::json!({"text_version_id":text.id,"revision":revision,"dependency_digest":dependency_digest}));
        let c=coverage(&tx,text.book_id)?;let done=c.selected_chapters.iter().filter(|i|c.completed_chapters.contains(i)).count();let progress=if c.selected_chapters.is_empty(){0.0}else{done as f64/c.selected_chapters.len() as f64};
        if tx.execute("UPDATE jobs SET payload_json=?2,progress=?3,updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",params![job,json(&payload)?,progress]).map_err(err)?!=1{return Err("terminal job cannot update checkpoint".into());}
    }
    tx.commit().map_err(err)?;Ok(revision)
}
fn check_line(conn:&Connection,book:i64,expected:i64)->Result<(),String>{if line_version(conn,book)?!=expected {Err("reading line changed; stale result cannot activate".into())}else{Ok(())}}
pub fn missing_original_chapters(conn:&Connection,book_id:i64)->Result<Vec<i64>,String>{Ok(coverage(conn,book_id)?.missing_chapters)}
pub fn job_checkpoint(conn:&Connection,job_id:i64)->Result<Value,String>{let payload=conn.query_row("SELECT payload_json FROM jobs WHERE id=?1",[job_id],|r|r.get::<_,Option<String>>(0)).map_err(err)?;payload.as_deref().map(parse).transpose().map(|p|p.unwrap_or_else(||serde_json::json!({})))}
pub fn save_generated(conn:&Connection,book_id:i64,idx:i64,content:&str,model:&str,dependency_digest:&str,expected_line_version:i64)->Result<TextVersion,String>{
    let tx=conn.unchecked_transaction().map_err(err)?;
    let text=save_generated_in_transaction(&tx,book_id,idx,content,model,dependency_digest,expected_line_version)?;
    tx.commit().map_err(err)?;Ok(text)
}

pub(crate) fn save_generated_in_transaction(conn:&Connection,book_id:i64,idx:i64,content:&str,model:&str,dependency_digest:&str,expected_line_version:i64)->Result<TextVersion,String>{
    ensure_chapter(conn,book_id,idx)?;let planned=crate::store::story::active_next_wiki(conn,book_id,idx)?.is_some();if idx==1 || (!planned && divergence(conn,book_id)?.is_none_or(|d|idx<d)){return Err("chapter does not belong to generated reading line".into());}
    check_line(conn,book_id,expected_line_version)?;
    let hash=digest(content.as_bytes());
    let existing=conn.query_row("SELECT t.generated_id FROM kg_text_versions t JOIN generated_chapters g ON g.id=t.generated_id WHERE t.book_id=?1 AND t.chapter_idx=?2 AND t.source='generated' AND t.content_hash=?3 AND g.prompt_digest=?4 AND g.model=?5 ORDER BY t.id DESC LIMIT 1",params![book_id,idx,hash,dependency_digest,model],|r|r.get::<_,i64>(0)).optional().map_err(err)?;
    let id=if let Some(id)=existing{id}else{conn.execute("INSERT INTO generated_chapters(book_id,chapter_idx,content,model,prompt_digest,active,line_version) VALUES (?1,?2,?3,?4,?5,0,?6)",params![book_id,idx,content,model,dependency_digest,expected_line_version]).map_err(err)?;conn.last_insert_rowid()};
    let text=text_version(conn,book_id,idx,"generated",Some(id))?;select_generated(conn,&text,expected_line_version)?;Ok(text)
}
pub fn cached_generated(conn:&Connection,book_id:i64,idx:i64,dependency_digest:&str)->Result<Option<TextVersion>,String>{
    ensure_chapter(conn,book_id,idx)?;let id=conn.query_row("SELECT id FROM generated_chapters WHERE book_id=?1 AND chapter_idx=?2 AND prompt_digest=?3 ORDER BY id DESC LIMIT 1",params![book_id,idx,dependency_digest],|r|r.get::<_,i64>(0)).optional().map_err(err)?;id.map(|id|text_version(conn,book_id,idx,"generated",Some(id))).transpose()
}
pub(crate) fn invalidate_generated_from(conn:&Connection,book:i64,idx:i64)->Result<(),String>{
    conn.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;
    conn.execute("UPDATE kg_lite_snapshots SET active=0 WHERE text_version_id IN(SELECT id FROM kg_text_versions WHERE book_id=?1 AND chapter_idx>=?2 AND source='generated')",params![book,idx]).map_err(err)?;
    conn.execute("DELETE FROM kg_read_selection WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;
    conn.execute("UPDATE kg_lite_specs SET active=0 WHERE book_id=?1 AND chapter_idx>=?2",params![book,idx]).map_err(err)?;Ok(())
}
fn select_generated(conn:&Connection,text:&TextVersion,line:i64)->Result<(),String>{
    let previous=selected_text(conn,text.book_id,text.chapter_idx)?;
    if previous.as_ref().is_none_or(|old|old.id!=text.id){invalidate_generated_from(conn,text.book_id,text.chapter_idx+1)?;}
    conn.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![text.book_id,text.chapter_idx]).map_err(err)?;
    conn.execute("UPDATE generated_chapters SET active=1,line_version=?2 WHERE id=?1",params![text.generated_id,line]).map_err(err)?;
    conn.execute("INSERT INTO kg_read_selection(book_id,chapter_idx,text_version_id) VALUES (?1,?2,?3) ON CONFLICT(book_id,chapter_idx) DO UPDATE SET text_version_id=excluded.text_version_id",params![text.book_id,text.chapter_idx,text.id]).map_err(err)?;Ok(())}
pub fn save_spec(conn:&Connection,book_id:i64,idx:i64,spec:&GraphView,dependency_digest:&str,expected_line_version:i64)->Result<(),String>{
    ensure_chapter(conn,book_id,idx)?;if spec.purpose!="spec"||spec.chapter_idx!=Some(idx){return Err("spec ownership mismatch".into());}view::validate_spec(conn,book_id,idx,spec)?;
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,book_id,expected_line_version)?;
    tx.execute("UPDATE kg_lite_specs SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![book_id,idx]).map_err(err)?;
    tx.execute("INSERT INTO kg_lite_specs(book_id,chapter_idx,dependency_digest,line_version,data_json,active) VALUES (?1,?2,?3,?4,?5,1) ON CONFLICT(book_id,chapter_idx,dependency_digest) DO UPDATE SET data_json=excluded.data_json,line_version=excluded.line_version,legacy=0,active=1",params![book_id,idx,dependency_digest,expected_line_version,json(spec)?]).map_err(err)?;tx.commit().map_err(err)
}
pub fn spec_matches(conn:&Connection,book_id:i64,idx:i64,dependency_digest:&str,expected_line_version:i64)->Result<bool,String>{
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,book_id,expected_line_version)?;
    let id=tx.query_row("SELECT id FROM kg_lite_specs WHERE book_id=?1 AND chapter_idx=?2 AND dependency_digest=?3 AND legacy=0",params![book_id,idx,dependency_digest],|r|r.get::<_,i64>(0)).optional().map_err(err)?;let Some(id)=id else{return Ok(false);};
    tx.execute("UPDATE kg_lite_specs SET active=0 WHERE book_id=?1 AND chapter_idx=?2",params![book_id,idx]).map_err(err)?;tx.execute("UPDATE kg_lite_specs SET active=1,line_version=?2 WHERE id=?1",params![id,expected_line_version]).map_err(err)?;tx.commit().map_err(err)?;Ok(true)
}
const FORMAT_VERSION:&str="kg-lite/v1";
fn schema_digest()->String{digest(include_str!("../../schemas/kg-chapter.schema.json").as_bytes())}
fn extraction_config_digest(config:&RuntimeConfig)->Result<String,String>{Ok(digest(json(&(FORMAT_VERSION,config.provider_id,&config.model_extract,config.context_limit,config.output_reserve.min(1024),&config.prompt_version,schema_digest()))?.as_bytes()))}
fn prior_digest(conn:&Connection,text:&TextVersion)->Result<String,String>{
    if text.source=="original"{return Ok(String::new());}
    let graph=internal_view(conn,&GraphQuery{book_id:text.book_id,scope:"global".into(),purpose:"actual".into(),chapter_idx:Some(text.chapter_idx-1)})?;
    let mut stmt=conn.prepare("SELECT chapter_idx,text_version_id FROM kg_read_selection WHERE book_id=?1 AND chapter_idx<?2 ORDER BY chapter_idx").map_err(err)?;
    let pins=stmt.query_map(params![text.book_id,text.chapter_idx],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?))).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    Ok(digest(json(&(graph.nodes,graph.edges,graph.events,graph.summary,graph.open_threads,pins,intent_constraints(conn,text.book_id,text.chapter_idx)?))?.as_bytes()))
}
pub fn extraction_matches(conn:&Connection,text_version_id:i64,config:&RuntimeConfig)->Result<bool,String>{let text=by_id(conn,text_version_id)?;if digest(text.content.as_bytes())!=text.content_hash{return Ok(false);}conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_lite_snapshots WHERE text_version_id=?1 AND active=1 AND legacy=0 AND config_digest=?2 AND schema_digest=?3 AND prior_digest=?4)",params![text.id,extraction_config_digest(config)?,schema_digest(),prior_digest(conn,&text)?],|r|r.get(0)).map_err(err)}
pub fn activate_cached_for_config(conn:&Connection,text_version_id:i64,dependency_digest:&str,expected_line_version:i64,config:&RuntimeConfig)->Result<(),String>{
    let text=by_id(conn,text_version_id)?;if text.source!="generated"{return Err("cache activation requires generated text".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,text.book_id,expected_line_version)?;
    if divergence(&tx,text.book_id)?.is_none_or(|d|text.chapter_idx<d){return Err("generated cache not eligible on this reading line".into());}
    let stored:String=tx.query_row("SELECT prompt_digest FROM generated_chapters WHERE id=?1 AND book_id=?2 AND chapter_idx=?3",params![text.generated_id,text.book_id,text.chapter_idx],|r|r.get(0)).map_err(err)?;
    if stored!=dependency_digest||digest(text.content.as_bytes())!=text.content_hash{return Err("cache dependency or content mismatch".into());}
    let prior=prior_digest(&tx,&text)?;select_generated(&tx,&text,expected_line_version)?;
    tx.execute("UPDATE kg_lite_snapshots SET active=0 WHERE text_version_id=?1",[text.id]).map_err(err)?;
    let id=tx.query_row("SELECT id FROM kg_lite_snapshots WHERE text_version_id=?1 AND legacy=0 AND config_digest=?2 AND schema_digest=?3 AND prior_digest=?4 ORDER BY id DESC LIMIT 1",params![text.id,extraction_config_digest(config)?,schema_digest(),prior],|r|r.get::<_,i64>(0)).optional().map_err(err)?;if let Some(id)=id{tx.execute("UPDATE kg_lite_snapshots SET active=1 WHERE id=?1",[id]).map_err(err)?;}tx.commit().map_err(err)
}
pub fn record_actual_drift(conn:&Connection,text_version_id:i64,diff:f64,expected_line_version:i64)->Result<f64,String>{
    if !diff.is_finite() || !(0.0..=1.0).contains(&diff){return Err("invalid actual drift".into());}
    let text=by_id(conn,text_version_id)?;if text.source!="generated" || !ready(conn,text.id)?{return Err("drift requires generated actual extraction".into());}
    let tx=conn.unchecked_transaction().map_err(err)?;check_line(&tx,text.book_id,expected_line_version)?;if selected_text(&tx,text.book_id,text.chapter_idx)?.is_none_or(|t|t.id!=text.id){return Err("drift text not selected".into());}
    tx.execute("INSERT INTO kg_actual_drift(text_version_id,diff) VALUES (?1,?2) ON CONFLICT(text_version_id) DO UPDATE SET diff=max(diff,excluded.diff)",params![text.id,diff]).map_err(err)?;
    let score=actual_drift(&tx,text.book_id,text.chapter_idx)?;tx.commit().map_err(err)?;Ok(score)
}
pub fn actual_drift(conn:&Connection,book:i64,upto:i64)->Result<f64,String>{let mut stmt=conn.prepare("SELECT d.diff FROM kg_actual_drift d JOIN kg_text_versions t ON t.id=d.text_version_id WHERE t.book_id=?1 AND t.chapter_idx<=?2 AND (EXISTS(SELECT 1 FROM kg_read_selection p WHERE p.book_id=t.book_id AND p.chapter_idx=t.chapter_idx AND p.text_version_id=t.id) OR NOT EXISTS(SELECT 1 FROM kg_read_selection p WHERE p.book_id=t.book_id AND p.chapter_idx=t.chapter_idx) AND EXISTS(SELECT 1 FROM generated_chapters g WHERE g.id=t.generated_id AND g.active=1)) ORDER BY t.chapter_idx").map_err(err)?;let rows=stmt.query_map(params![book,upto],|r|r.get::<_,f64>(0)).map_err(err)?;let mut residual=1.0;for row in rows{residual*=1.0-row.map_err(err)?;}Ok(1.0-residual)}
pub fn registry_nodes(conn:&Connection,book_id:i64,source:&str,upto:i64)->Result<Vec<GraphNode>,String>{let purpose=match source{"original"=>"reference","generated"=>"actual",_=>return Err("invalid registry source".into())};Ok(internal_view(conn,&GraphQuery{book_id,scope:"global".into(),purpose:purpose.into(),chapter_idx:Some(upto)})?.nodes.into_iter().filter(|n|n.chapter_idx>0&&n.chapter_idx<=upto).collect())}
pub fn extraction_plan_exists(conn:&Connection,book_id:i64)->Result<bool,String>{ensure_book(conn,book_id)?;conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_extraction_plans WHERE book_id=?1)",[book_id],|r|r.get(0)).map_err(err)}
pub fn set_extraction_plan(conn:&Connection,book_id:i64,selected:&[i64],calls_used:usize,elapsed_ms:u64)->Result<(),String>{ensure_book(conn,book_id)?;let mut selected=selected.to_vec();selected.sort_unstable();selected.dedup();for idx in &selected{ensure_chapter(conn,book_id,*idx)?;}let calls=i64::try_from(calls_used).map_err(err)?;let elapsed=i64::try_from(elapsed_ms).map_err(err)?;conn.execute("INSERT INTO kg_extraction_plans(book_id,selected_json,calls_used,elapsed_ms) VALUES (?1,?2,?3,?4) ON CONFLICT(book_id) DO UPDATE SET selected_json=excluded.selected_json,calls_used=max(calls_used,excluded.calls_used),elapsed_ms=max(elapsed_ms,excluded.elapsed_ms)",params![book_id,json(&selected)?,calls,elapsed]).map_err(err)?;Ok(())}
pub fn record_extraction_attempt(conn:&Connection,book_id:i64,calls_used:usize,elapsed_ms:u64)->Result<(),String>{ensure_book(conn,book_id)?;let updated=conn.execute("UPDATE kg_extraction_plans SET calls_used=max(calls_used,?2),elapsed_ms=max(elapsed_ms,?3) WHERE book_id=?1",params![book_id,i64::try_from(calls_used).map_err(err)?,i64::try_from(elapsed_ms).map_err(err)?]).map_err(err)?;if updated!=1{return Err("extraction plan not initialized".into());}Ok(())}
pub fn coverage(conn:&Connection,book_id:i64)->Result<GraphCoverage,String>{
    ensure_book(conn,book_id)?;let mut stmt=conn.prepare("SELECT idx FROM chapters WHERE book_id=?1 ORDER BY idx").map_err(err)?;let all=stmt.query_map([book_id],|r|r.get::<_,i64>(0)).map_err(err)?.collect::<Result<Vec<_>,_>>().map_err(err)?;
    let plan=conn.query_row("SELECT selected_json,calls_used,elapsed_ms FROM kg_extraction_plans WHERE book_id=?1",[book_id],|r|Ok((r.get::<_,String>(0)?,r.get::<_,i64>(1)?,r.get::<_,i64>(2)?))).optional().map_err(err)?;
    let mut stmt=conn.prepare("SELECT t.chapter_idx,s.legacy FROM kg_lite_snapshots s JOIN kg_text_versions t ON t.id=s.text_version_id WHERE t.book_id=?1 AND t.source='original' AND s.active=1 ORDER BY t.chapter_idx").map_err(err)?;let mut completed=Vec::new();let mut legacy=Vec::new();for row in stmt.query_map([book_id],|r|Ok((r.get::<_,i64>(0)?,r.get::<_,bool>(1)?))).map_err(err)?{let(idx,old)=row.map_err(err)?;if old{legacy.push(idx);}else{completed.push(idx);}}
    let(selected,calls,elapsed)=match plan{Some((data,calls,elapsed))=>(parse::<Vec<i64>>(&data)?,calls as usize,elapsed as u64),None=>(Vec::new(),0,0)};
    let target_complete=!selected.is_empty()&&selected.iter().all(|i|completed.contains(i));let missing=all.iter().copied().filter(|i|!completed.contains(i)).collect();let mode=if target_complete{if completed.len()==all.len(){"full"}else{"sampled"}}else{"partial"};
    Ok(GraphCoverage{mode:mode.into(),total_chapters:all.len(),selected_chapters:selected,completed_chapters:completed,missing_chapters:missing,legacy_chapters:legacy,calls_used:calls,elapsed_ms:elapsed,target_complete})
}
pub fn save_original_summary(conn:&Connection,book_id:i64)->Result<(),String>{let c=coverage(conn,book_id)?;if !c.target_complete{return Err("selected extraction plan incomplete".into());}let graph=internal_view(conn,&GraphQuery{book_id,scope:"global".into(),purpose:"reference".into(),chapter_idx:None})?;conn.execute("UPDATE books SET synopsis=?2 WHERE id=?1",params![book_id,graph.summary]).map_err(err)?;Ok(())}
