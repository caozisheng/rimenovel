use crate::core::quick_read::{validate_quick_read_coverage, ChapterQuickRead, QuickReadCharacterCard, QuickReadReference, QuickReadSlice, QuickReadSourceKind, SourceChapter, SourceSpan};
use crate::web_search::{EditionProof, EditionProofMethod, WebOutlineProvenance};
use rusqlite::{params, Connection, OptionalExtension};
use serde::Serialize;

fn error(value: impl std::fmt::Display) -> String { value.to_string() }
fn encode<T: Serialize>(value: &T) -> Result<String, String> { serde_json::to_string(value).map_err(error) }
fn decode<T: serde::de::DeserializeOwned>(value: &str) -> Result<T, String> { serde_json::from_str(value).map_err(error) }

pub fn source_chapters(conn: &Connection, book: i64) -> Result<Vec<SourceChapter>, String> {
    let mut statement = conn.prepare("SELECT idx,original_text FROM chapters WHERE book_id=?1 ORDER BY idx").map_err(error)?;
    let rows = statement.query_map([book], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(error)?;
    rows.map(|row| {
        let (chapter_idx, text) = row.map_err(error)?;
        Ok(SourceChapter { chapter_idx, char_count: text.chars().count(), content_hash: crate::core::kg::digest(text.as_bytes()) })
    }).collect()
}

fn chapter_headings(conn:&Connection,book:i64)->Result<Vec<(i64,String)>,String>{
    let mut statement=conn.prepare("SELECT idx,title FROM chapters WHERE book_id=?1 ORDER BY idx").map_err(error)?;
    let rows=statement.query_map([book],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?))).map_err(error)?;
    rows.collect::<Result<Vec<_>,_>>().map_err(error)
}

pub fn source_digest(sources: &[SourceChapter]) -> String {
    crate::core::kg::digest(&serde_json::to_vec(sources).expect("source chapter hashes serialize"))
}

pub fn prepare_revision(
    conn: &Connection, book: i64, sources: &[SourceChapter], dependency: &str, provider_id: i64,
    model: &str, prompt: &str, schema: &str, slices: &[Vec<SourceSpan>],
) -> Result<i64, String> {
    if sources.is_empty() || slices.is_empty() { return Err("quick-read source and slices must not be empty".into()); }
    let source_hash = source_digest(sources);
    let mut by_chapter: std::collections::HashMap<i64, Vec<(usize, usize)>> = std::collections::HashMap::new();
    for spans in slices {
        if spans.is_empty() { return Err("quick-read slice must cover source text".into()); }
        for span in spans {
            let source = sources.iter().find(|source| source.chapter_idx == span.chapter_idx)
                .ok_or_else(|| "quick-read plan contains an unknown chapter".to_string())?;
            if span.content_hash != source.content_hash || span.start_char >= span.end_char || span.end_char > source.char_count {
                return Err("quick-read plan contains an invalid source range".into());
            }
            by_chapter.entry(span.chapter_idx).or_default().push((span.start_char, span.end_char));
        }
    }
    for source in sources {
        let mut ranges = by_chapter.remove(&source.chapter_idx).unwrap_or_default();
        ranges.sort_unstable();
        let mut cursor = 0;
        for (start, end) in ranges {
            if start < cursor { return Err("quick-read plan overlaps source text".into()); }
            if start != cursor { return Err("quick-read plan leaves a source coverage gap".into()); }
            cursor = end;
        }
        if cursor != source.char_count { return Err("quick-read plan does not cover the full source".into()); }
    }

    let tx = conn.unchecked_transaction().map_err(error)?;
    if let Some(id) = tx.query_row(
        "SELECT id FROM quick_read_revisions WHERE book_id=?1 AND source_digest=?2 AND dependency_digest=?3 AND source_kind='local_full_text' AND status IN ('pending','running','failed','ready','stale') ORDER BY id DESC LIMIT 1",
        params![book, source_hash, dependency], |row| row.get::<_, i64>(0),
    ).optional().map_err(error)? {
        tx.execute("UPDATE quick_read_revisions SET active=0,status='stale' WHERE book_id=?1 AND active=1 AND id<>?2", params![book, id]).map_err(error)?;
        tx.execute("UPDATE quick_read_revisions SET active=1,status=CASE WHEN status='stale' THEN 'pending' ELSE status END WHERE id=?1", [id]).map_err(error)?;
        // Older workers accidentally persisted the slice schema JSON here. Correct
        // only that exact metadata mistake; never recreate or discard checkpoints.
        if schema=="quick-read-summary/v1" {
            let legacy=encode(crate::core::quick_read::slice_schema())?;
            tx.execute("UPDATE quick_read_revisions SET schema_version=?2 WHERE id=?1 AND schema_version=?3 AND status<>'ready'",params![id,schema,legacy]).map_err(error)?;
        }
        tx.commit().map_err(error)?;
        return Ok(id);
    }
    tx.execute("UPDATE quick_read_revisions SET active=0,status='stale' WHERE book_id=?1 AND active=1", [book]).map_err(error)?;
    tx.execute(
        "INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,total_chars,total_chapters,total_slices,status,active) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,'pending',1)",
        params![book, source_hash, dependency, provider_id, model, prompt, schema, sources.iter().map(|source| source.char_count).sum::<usize>() as i64, sources.len() as i64, slices.len() as i64],
    ).map_err(error)?;
    let revision = tx.last_insert_rowid();
    for (slice_idx, spans) in slices.iter().enumerate() {
        let slice_hash = crate::core::kg::digest(&serde_json::to_vec(spans).map_err(error)?);
        let first = spans.first().expect("non-empty slice validated");
        let last = spans.last().expect("non-empty slice validated");
        tx.execute(
            "INSERT INTO quick_read_slices(revision_id,slice_idx,source_hash,start_chapter,start_char,end_chapter,end_char,spans_json,status) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,'pending')",
            params![revision, slice_idx as i64, slice_hash, first.chapter_idx, first.start_char as i64, last.chapter_idx, last.end_char as i64, encode(spans)?],
        ).map_err(error)?;
    }
    tx.commit().map_err(error)?;
    Ok(revision)
}

/// Resume against the durable plan, even if a new context budget would size slices differently.
pub fn slice_plan(conn:&Connection,revision:i64)->Result<Vec<Vec<SourceSpan>>,String>{
    let mut statement=conn.prepare("SELECT slice_idx,spans_json FROM quick_read_slices WHERE revision_id=?1 ORDER BY slice_idx").map_err(error)?;
    let rows=statement.query_map([revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?))).map_err(error)?;
    let mut plan=Vec::new();
    for row in rows {let (idx,spans)=row.map_err(error)?;if idx!=plan.len() as i64{return Err("quick-read persisted slice plan has an index gap".into());}plan.push(decode(&spans)?);}
    if plan.is_empty(){return Err("quick-read persisted slice plan is empty".into());}
    Ok(plan)
}

pub fn pending_slices(conn: &Connection, revision: i64) -> Result<Vec<usize>, String> {
    let mut statement = conn.prepare("SELECT slice_idx FROM quick_read_slices WHERE revision_id=?1 AND status<>'ready' ORDER BY slice_idx").map_err(error)?;
    let rows = statement.query_map([revision], |row| row.get::<_, i64>(0)).map_err(error)?;
    rows.map(|row| row.map(|idx| idx as usize).map_err(error)).collect()
}

pub fn load_slices(conn: &Connection, revision: i64) -> Result<Vec<QuickReadSlice>, String> {
    let mut statement = conn.prepare("SELECT slice_idx,spans_json,result_json,status FROM quick_read_slices WHERE revision_id=?1 ORDER BY slice_idx").map_err(error)?;
    let rows = statement.query_map([revision], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?, row.get::<_, String>(3)?))).map_err(error)?;
    let mut result = Vec::new();
    for row in rows {
        let (idx, spans, payload, status) = row.map_err(error)?;
        if status != "ready" { continue; }
        let slice: QuickReadSlice = decode(&payload.ok_or("ready quick-read slice has no result")?)?;
        if slice.slice_idx != idx as usize || slice.source_spans != decode::<Vec<SourceSpan>>(&spans)? {
            return Err("persisted quick-read slice identity mismatch".into());
        }
        result.push(slice);
    }
    Ok(result)
}

pub fn save_slice(conn: &Connection, revision: i64, slice: &QuickReadSlice, usage: (i64, i64)) -> Result<(), String> {
    if usage.0 < 0 || usage.1 < 0 { return Err("quick-read usage cannot be negative".into()); }
    let tx = conn.unchecked_transaction().map_err(error)?;
    let raw: Option<(String, String)> = tx.query_row(
        "SELECT spans_json,status FROM quick_read_slices WHERE revision_id=?1 AND slice_idx=?2",
        params![revision, slice.slice_idx as i64], |row| Ok((row.get(0)?, row.get(1)?)),
    ).optional().map_err(error)?;
    let (spans, status) = raw.ok_or("quick-read slice does not belong to revision")?;
    if decode::<Vec<SourceSpan>>(&spans)? != slice.source_spans { return Err("quick-read response source span mismatch".into()); }
    let payload = encode(slice)?;
    if status == "ready" {
        let existing: Option<String> = tx.query_row("SELECT result_json FROM quick_read_slices WHERE revision_id=?1 AND slice_idx=?2", params![revision, slice.slice_idx as i64], |row| row.get(0)).map_err(error)?;
        if existing.as_deref() == Some(payload.as_str()) { tx.commit().map_err(error)?; return Ok(()); }
        return Err("completed quick-read slice cannot be overwritten".into());
    }
    tx.execute(
        "UPDATE quick_read_slices SET result_json=?3,tokens_in=?4,tokens_out=?5,status='ready',error=NULL,updated_at=datetime('now') WHERE revision_id=?1 AND slice_idx=?2",
        params![revision, slice.slice_idx as i64, payload, usage.0, usage.1],
    ).map_err(error)?;
    let completed: i64 = tx.query_row("SELECT COUNT(*) FROM quick_read_slices WHERE revision_id=?1 AND status='ready'", [revision], |row| row.get(0)).map_err(error)?;
    tx.execute("UPDATE quick_read_revisions SET completed_slices=?2,status='running' WHERE id=?1 AND status<>'stale'", params![revision, completed]).map_err(error)?;
    tx.commit().map_err(error)
}
fn validate_chapter_summary_map(sources:&[SourceChapter],chapters:&[ChapterQuickRead])->Result<(),String>{
    let source_indices:std::collections::HashSet<_>=sources.iter().map(|source|source.chapter_idx).collect();
    let chapter_indices:std::collections::HashSet<_>=chapters.iter().map(|chapter|chapter.chapter_idx).collect();
    if chapter_indices.len()!=chapters.len()||chapter_indices!=source_indices||chapters.iter().any(|chapter|chapter.summary.trim().is_empty()){return Err("quick-read chapter map is incomplete or invalid".into());}Ok(())
}

fn validate_reference_baseline(sources:&[SourceChapter],reference:&QuickReadReference)->Result<(),String>{
    validate_chapter_summary_map(sources,&reference.chapter_summaries)?;
    let source_indices:std::collections::HashSet<_>=sources.iter().map(|source|source.chapter_idx).collect();
    let mut members=std::collections::HashSet::new();
    for card in &reference.characters {if card.id.trim().is_empty()||card.name.trim().is_empty()||card.identity_evidence.is_empty()||card.identity_evidence.iter().any(|value|value.trim().is_empty())||!source_indices.contains(&card.first_appearance_chapter)||card.source_chapters.is_empty()||card.source_chapters.iter().any(|chapter|!source_indices.contains(chapter))||card.revelations.iter().any(|reveal|!source_indices.contains(&reveal.chapter_idx)){return Err("quick-read character card has invalid source evidence".into());}if !members.insert(&card.id){return Err("quick-read character IDs must be unique".into());}}
    for note in &reference.plot_notes {if note.summary.trim().is_empty()||note.source_chapters.is_empty()||note.source_chapters.iter().any(|chapter|!source_indices.contains(chapter)){return Err("quick-read plot note has invalid source chapters".into());}}
    if reference.plot_summary.trim().is_empty()||reference.characters.is_empty(){return Err("quick-read summary lacks plot or character baseline".into());}
    Ok(())
}

fn validate_web_provenance(provenance:&[WebOutlineProvenance])->Result<(),String>{
    let mut sources=std::collections::HashSet::new();
    for source in provenance {let parsed=reqwest::Url::parse(&source.url).map_err(|_|"web outline has an invalid provenance URL")?;if !matches!(parsed.scheme(),"http"|"https")||source.title.trim().is_empty()||source.evidence.trim().is_empty()||source.evidence.chars().count()>280||!sources.insert((&source.url,&source.title,&source.evidence)){return Err("web outline has invalid or duplicate provenance".into());}}
    if provenance.is_empty(){return Err("web outline lacks provenance".into());}Ok(())
}

fn validate_persisted_edition_proof(kind:&str,chapter_idx:i64,match_score:Option<f64>)->Result<(),String>{
    match kind {
        "ai_paragraph" if chapter_idx>0 && match_score.is_none()=>Ok(()),
        // The former lookup accepted chapter 1 at 82%; only migrated rows use this method.
        "text_similarity" if chapter_idx==1 && match_score.is_some_and(|score|score.is_finite()&&(0.82..=1.0).contains(&score))=>Ok(()),
        _=>Err("web outline has an invalid edition proof or match score".into()),
    }
}

fn load_exact_chapter_summaries(conn:&Connection,revision:i64,sources:&[SourceChapter])->Result<Vec<ChapterQuickRead>,String>{
    let mut statement=conn.prepare("SELECT chapter_idx,data_json FROM quick_read_chapter_summaries WHERE revision_id=?1 ORDER BY chapter_idx").map_err(error)?;
    let rows=statement.query_map([revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?))).map_err(error)?;
    let mut chapters=Vec::new();for row in rows {let (idx,data)=row.map_err(error)?;let chapter:ChapterQuickRead=decode(&data)?;if chapter.chapter_idx!=idx{return Err("web outline chapter summary identity mismatch".into());}chapters.push(chapter);}
    validate_chapter_summary_map(sources,&chapters)?;Ok(chapters)
}

fn validate_persisted_web_map(conn:&Connection,revision:i64,book:i64,sources:&[SourceChapter],provenance:&[WebOutlineProvenance],proof_chapter_idx:i64)->Result<bool,String>{
    let headings=chapter_headings(conn,book)?;if headings.len()!=sources.len(){return Ok(false);}
    let urls:std::collections::HashSet<_>=provenance.iter().map(|source|source.url.as_str()).collect();
    let mut statement=conn.prepare("SELECT chapter_idx,outline_heading,source_url FROM quick_read_web_outline_chapters WHERE revision_id=?1 ORDER BY chapter_idx").map_err(error)?;
    let rows=statement.query_map([revision],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,String>(1)?,row.get::<_,String>(2)?))).map_err(error)?;
    let rows=rows.collect::<Result<Vec<_>,_>>().map_err(error)?;if rows.len()!=sources.len()||!rows.iter().any(|(idx,_,_)|*idx==proof_chapter_idx){return Ok(false);}
    for (((idx,heading,url),source),(local_idx,local_heading)) in rows.iter().zip(sources).zip(&headings) {if *idx!=source.chapter_idx||*idx!=*local_idx||heading!=local_heading||!urls.contains(url.as_str()){return Ok(false);}}
    Ok(true)
}

pub fn active_reference(conn:&Connection,book:i64)->Result<Option<QuickReadReference>,String>{
    let row:Option<(i64,String,String,String,i64,String,String,String,String,String,i64,i64,i64,i64)>=conn.query_row("SELECT id,source_digest,plot_summary,plot_notes_json,provider_id,model,prompt_version,schema_version,characters_json,source_kind,total_chars,completed_chars,total_slices,completed_slices FROM quick_read_revisions WHERE book_id=?1 AND active=1 AND status='ready'",[book],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?,row.get(10)?,row.get(11)?,row.get(12)?,row.get(13)?))).optional().map_err(error)?;
    let Some((revision,source_digest,plot_summary,plot_notes_json,provider_id,model,prompt_version,schema_version,characters_json,kind,total_chars,completed_chars,total_slices,completed_slices))=row else{return Ok(None)};
    let sources=source_chapters(conn,book)?;if crate::store::quick_read::source_digest(&sources)!=source_digest{return Ok(None);}
    let characters=decode::<Vec<QuickReadCharacterCard>>(&characters_json)?;let plot_notes=decode(&plot_notes_json)?;
    match kind.as_str() {
        "local_full_text"=>{if total_slices<=0||completed_slices!=total_slices||completed_chars!=total_chars{return Ok(None);}let slices=load_slices(conn,revision)?;if slices.len()!=total_slices as usize{return Ok(None);}validate_quick_read_coverage(&sources,&slices)?;let chapter_summaries=crate::core::quick_read::canonical_chapter_summaries(&slices);let reference=QuickReadReference{schema_version,source_digest,source_kind:QuickReadSourceKind::LocalFullText,provider_id,model,prompt_version,chapter_summaries,characters,plot_summary,plot_notes,completed_spans:slices.iter().flat_map(|slice|slice.source_spans.clone()).collect(),source_provenance:vec![]};validate_reference_baseline(&sources,&reference)?;Ok(Some(reference))}
        "web_outline"=>{
            if total_chars!=0||completed_chars!=0||total_slices!=0||completed_slices!=0{return Ok(None);}
            let Some((match_score,provenance_json,proof_kind,proof_chapter_idx)):Option<(Option<f64>,String,String,i64)>=conn.query_row("SELECT match_score,provenance_json,proof_kind,proof_chapter_idx FROM quick_read_web_outlines WHERE revision_id=?1",[revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).optional().map_err(error)? else{return Ok(None)};
            validate_persisted_edition_proof(&proof_kind,proof_chapter_idx,match_score)?;
            let source_provenance:Vec<WebOutlineProvenance>=decode(&provenance_json)?;validate_web_provenance(&source_provenance)?;
            if !validate_persisted_web_map(conn,revision,book,&sources,&source_provenance,proof_chapter_idx)?{return Ok(None);}
            let chapter_summaries=load_exact_chapter_summaries(conn,revision,&sources)?;
            // Historical references remain inspectable; consumers check current readiness separately.
            let reference=QuickReadReference{schema_version,source_digest,source_kind:QuickReadSourceKind::WebOutline,provider_id,model,prompt_version,chapter_summaries,characters,plot_summary,plot_notes,completed_spans:vec![],source_provenance};validate_reference_baseline(&sources,&reference)?;Ok(Some(reference))
        }
        _=>Ok(None),
    }
}

pub fn publish_reference(conn:&Connection,revision:i64,reference:&QuickReadReference)->Result<(),String>{
    if reference.source_kind!=QuickReadSourceKind::LocalFullText||!reference.source_provenance.is_empty(){return Err("local quick-read reference has an invalid source kind or provenance".into());}
    let (book,source_hash,provider,model,prompt,schema,total_chars,total_chapters,total_slices,status):(i64,String,i64,String,String,String,i64,i64,i64,String)=conn.query_row("SELECT book_id,source_digest,provider_id,model,prompt_version,schema_version,total_chars,total_chapters,total_slices,status FROM quick_read_revisions WHERE id=?1 AND active=1 AND source_kind='local_full_text'",[revision],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?,row.get(6)?,row.get(7)?,row.get(8)?,row.get(9)?))).map_err(error)?;
    let sources=source_chapters(conn,book)?;let slices=load_slices(conn,revision)?;
    if status=="stale"||source_digest(&sources)!=source_hash||reference.source_digest!=source_hash||reference.provider_id!=provider||reference.model!=model||reference.prompt_version!=prompt||reference.schema_version!=schema{return Err("quick-read source or dependency became stale".into());}
    if slices.len()!=total_slices as usize||sources.len()!=total_chapters as usize||sources.iter().map(|source|source.char_count).sum::<usize>() as i64!=total_chars{return Err("quick-read incomplete: one or more slices are pending".into());}
    validate_quick_read_coverage(&sources,&slices)?;
    if reference.chapter_summaries!=crate::core::quick_read::canonical_chapter_summaries(&slices){return Err("quick-read canonical chapter summary mismatch".into());}
    if reference.completed_spans!=slices.iter().flat_map(|slice|slice.source_spans.clone()).collect::<Vec<_>>(){return Err("quick-read source spans mismatch".into());}
    validate_reference_baseline(&sources,reference)?;
    let tx=conn.unchecked_transaction().map_err(error)?;
    for chapter in &reference.chapter_summaries {tx.execute("INSERT INTO quick_read_chapter_summaries(revision_id,chapter_idx,data_json) VALUES (?1,?2,?3)",params![revision,chapter.chapter_idx,encode(chapter)?]).map_err(error)?;}
    tx.execute("UPDATE quick_read_revisions SET plot_summary=?2,plot_notes_json=?3,characters_json=?4,status='ready',completed_chars=total_chars,completed_slices=total_slices WHERE id=?1 AND active=1",params![revision,reference.plot_summary,encode(&reference.plot_notes)?,encode(&reference.characters)?]).map_err(error)?;
    tx.commit().map_err(error)
}

pub fn publish_web_reference(conn:&Connection,book:i64,normalized_book_title:&str,proof:&EditionProof,dependency_digest:&str,search_engine:&str,chapter_map:&[(i64,String,String)],reference:&QuickReadReference)->Result<i64,String>{
    if reference.source_kind!=QuickReadSourceKind::WebOutline||!reference.completed_spans.is_empty()||normalized_book_title.trim().is_empty(){return Err("web outline must not claim local text coverage".into());}
    let sources=source_chapters(conn,book)?;if sources.is_empty()||source_digest(&sources)!=reference.source_digest{return Err("web outline source became stale".into());}
    validate_reference_baseline(&sources,reference)?;validate_web_provenance(&reference.source_provenance)?;
    if chapter_map.len()!=sources.len(){return Err("web outline chapter map is incomplete".into());}
    let headings=chapter_headings(conn,book)?;if headings.len()!=sources.len(){return Err("web outline chapter map is incomplete".into());}
    let provenance_urls:std::collections::HashSet<_>=reference.source_provenance.iter().map(|source|source.url.as_str()).collect();
    for (((idx,heading,url),source),(local_idx,local_heading)) in chapter_map.iter().zip(&sources).zip(&headings) {if *idx!=source.chapter_idx||*idx!=*local_idx||heading!=local_heading||!provenance_urls.contains(url.as_str()){return Err("web outline chapter map does not exactly match local chapters".into());}}
    let proof_chapter_idx=i64::try_from(proof.chapter_index).map_err(|_|"web outline edition proof chapter is invalid")?;
    if proof.method!=EditionProofMethod::AiParagraph||!reference.source_provenance.contains(&proof.provenance)||!chapter_map.iter().any(|(idx,_,url)|*idx==proof_chapter_idx&&url==&proof.provenance.url){return Err("web outline edition proof lacks an accepted mapped source".into());}
    let tx=conn.unchecked_transaction().map_err(error)?;tx.execute("UPDATE quick_read_revisions SET active=0,status='stale' WHERE book_id=?1 AND active=1",[book]).map_err(error)?;
    tx.execute("INSERT INTO quick_read_revisions(book_id,source_digest,dependency_digest,provider_id,model,prompt_version,schema_version,source_kind,total_chars,total_chapters,completed_chars,completed_slices,total_slices,status,active,plot_summary,plot_notes_json,characters_json) VALUES (?1,?2,?3,?4,?5,?6,?7,'web_outline',0,?8,0,0,0,'ready',1,?9,?10,?11)",params![book,reference.source_digest,dependency_digest,reference.provider_id,reference.model,reference.prompt_version,reference.schema_version,sources.len() as i64,reference.plot_summary,encode(&reference.plot_notes)?,encode(&reference.characters)?]).map_err(error)?;
    let revision=tx.last_insert_rowid();tx.execute("INSERT INTO quick_read_web_outlines(revision_id,normalized_book_title,match_score,provenance_json,proof_kind,proof_chapter_idx,search_engine) VALUES (?1,?2,NULL,?3,'ai_paragraph',?4,?5)",params![revision,normalized_book_title,encode(&reference.source_provenance)?,proof_chapter_idx,search_engine]).map_err(error)?;
    for chapter in &reference.chapter_summaries {tx.execute("INSERT INTO quick_read_chapter_summaries(revision_id,chapter_idx,data_json) VALUES (?1,?2,?3)",params![revision,chapter.chapter_idx,encode(chapter)?]).map_err(error)?;}
    for (idx,heading,url) in chapter_map {tx.execute("INSERT INTO quick_read_web_outline_chapters(revision_id,chapter_idx,outline_heading,source_url) VALUES (?1,?2,?3,?4)",params![revision,idx,heading,url]).map_err(error)?;}
    tx.commit().map_err(error)?;Ok(revision)
}

pub fn mark_failed(conn: &Connection, revision: i64, message: &str) -> Result<(), String> {
    conn.execute("UPDATE quick_read_revisions SET status='failed' WHERE id=?1 AND active=1 AND status<>'ready'", [revision]).map_err(error)?;
    conn.execute("UPDATE quick_read_slices SET status='failed',error=?2,updated_at=datetime('now') WHERE revision_id=?1 AND status<>'ready'", params![revision, message]).map_err(error)?;
    Ok(())
}

pub fn chapter_summary(conn: &Connection, book: i64, chapter: i64) -> Result<Option<ChapterQuickRead>, String> {
    let value: Option<String> = conn.query_row(
        "SELECT s.data_json FROM quick_read_chapter_summaries s JOIN quick_read_revisions r ON r.id=s.revision_id WHERE r.book_id=?1 AND r.active=1 AND r.status='ready' AND s.chapter_idx=?2",
        params![book, chapter], |row| row.get(0),
    ).optional().map_err(error)?;
    value.map(|value| decode(&value)).transpose()
}

#[cfg(test)]
#[path="quick_read_tests.rs"]
mod tests;
