use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::core::story::{InterventionInput, NextChapterWiki};

pub fn save_intervention(conn: &Connection, input: &InterventionInput) -> Result<i64, String> {
    let selected = serde_json::to_string(&input.selected_option_ids).map_err(|e| e.to_string())?;
    let overridden = serde_json::to_string(&input.overridden_option_ids).map_err(|e| e.to_string())?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    if input.chapter_idx <= crate::store::kg::progress(&tx, input.book_id)? { return Err("已读章节不能修改未来干预".into()); }
    tx.execute(
        "INSERT INTO story_interventions (book_id,chapter_idx,batch_index,question_batch_id,selected_json,free_text,overridden_json,state,effective_from,reading_line_version) VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?2,?8)",
        params![input.book_id, input.chapter_idx, input.batch_index, input.question_batch_id, selected, input.free_text, overridden, input.reading_line_version],
    ).map_err(|e| e.to_string())?;
    let id = tx.last_insert_rowid();
    crate::store::kg::invalidate_generated_from(&tx, input.book_id, input.chapter_idx)?;
    tx.execute("UPDATE story_next_wiki_revisions SET active=0,status='stale' WHERE book_id=?1 AND chapter_idx>=?2 AND active=1",params![input.book_id,input.chapter_idx]).map_err(|e| e.to_string())?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(id)
}

pub fn active_interventions(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<Vec<InterventionInput>, String> {
    let mut stmt = conn.prepare("SELECT chapter_idx,batch_index,question_batch_id,selected_json,free_text,overridden_json,reading_line_version FROM story_interventions WHERE book_id=?1 AND chapter_idx=?2 AND state='active' ORDER BY id").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![book_id, chapter_idx], |row| {
        let selected: String = row.get(3)?;
        let overridden: String = row.get(5)?;
        Ok(InterventionInput {
            book_id,
            chapter_idx: row.get(0)?,
            batch_index: row.get(1)?,
            question_batch_id: row.get(2)?,
            selected_option_ids: serde_json::from_str(&selected).unwrap_or_default(),
            free_text: row.get(4)?,
            overridden_option_ids: serde_json::from_str(&overridden).unwrap_or_default(),
            reading_line_version: row.get(6)?,
        })
    }).map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())
}

pub fn save_next_wiki(conn: &Connection, book_id: i64, chapter_idx: i64, previous_text_version_id: i64, history_revision: i64, dependency_digest: &str, wiki: &NextChapterWiki) -> Result<i64, String> {
    let data = serde_json::to_string(wiki).map_err(|e| e.to_string())?;
    conn.execute("UPDATE story_next_wiki_revisions SET active=0,status='stale' WHERE book_id=?1 AND chapter_idx=?2 AND active=1", params![book_id, chapter_idx]).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO story_next_wiki_revisions (book_id,chapter_idx,history_revision,previous_text_version_id,dependency_digest,data_json,status) VALUES (?1,?2,?3,?4,?5,?6,'ready')", params![book_id, chapter_idx, history_revision, previous_text_version_id, dependency_digest, data]).map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn active_next_wiki(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<Option<NextChapterWiki>, String> {
    let data: Option<String> = conn.query_row("SELECT data_json FROM story_next_wiki_revisions WHERE book_id=?1 AND chapter_idx=?2 AND active=1 AND status='ready'", params![book_id, chapter_idx], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
    data.map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string())).transpose()
}

pub fn mark_next_wiki_stale(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<(), String> {
    conn.execute("UPDATE story_next_wiki_revisions SET status='stale',active=0 WHERE book_id=?1 AND chapter_idx=?2 AND active=1", params![book_id, chapter_idx]).map_err(|e| e.to_string())?;
    Ok(())
}

pub fn decode_json(raw: &str) -> Result<Value, String> { serde_json::from_str(raw).map_err(|e| e.to_string()) }

pub fn save_question_batch(conn: &Connection, book_id: i64, chapter_idx: i64, reading_line_version: i64, batch: &crate::core::story::QuestionBatch) -> Result<i64, String> {
    let data = serde_json::to_string(batch).map_err(|e| e.to_string())?;
    conn.execute("UPDATE story_question_batches SET active=0 WHERE book_id=?1 AND chapter_idx=?2 AND active=1", params![book_id, chapter_idx]).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO story_question_batches (book_id,chapter_idx,batch_index,question_batch_id,data_json,reading_line_version,active) VALUES (?1,?2,?3,?4,?5,?6,1) ON CONFLICT(book_id,chapter_idx,question_batch_id) DO UPDATE SET batch_index=excluded.batch_index,data_json=excluded.data_json,reading_line_version=excluded.reading_line_version,active=1", params![book_id, chapter_idx, batch.batch_index as i64, batch.question_batch_id, data, reading_line_version]).map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
}

pub fn question_batch(conn: &Connection, book_id: i64, chapter_idx: i64, batch_index: usize) -> Result<Option<crate::core::story::QuestionBatch>, String> {
    let data: Option<String> = conn.query_row("SELECT data_json FROM story_question_batches WHERE book_id=?1 AND chapter_idx=?2 AND batch_index=?3 AND active=1", params![book_id, chapter_idx, batch_index as i64], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
    data.map(|raw| serde_json::from_str(&raw).map_err(|e| e.to_string())).transpose()
}

pub fn question_batches(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<Vec<crate::core::story::QuestionBatch>, String> {
    let mut stmt = conn.prepare("SELECT data_json FROM story_question_batches WHERE book_id=?1 AND chapter_idx=?2 ORDER BY batch_index").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![book_id, chapter_idx], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())?;
    rows.map(|row| {
        let raw = row.map_err(|e| e.to_string())?;
        serde_json::from_str(&raw).map_err(|e| e.to_string())
    }).collect()
}

/// Publish the complete model result once: immutable prose, exact selection and
/// its actual Wiki share a transaction. An original reference is never a fallback.
pub fn save_rewritten_chapter(conn: &Connection, book_id: i64, chapter_idx: i64, prose: &str, model: &str, dependency_digest: &str, expected_line_version: i64, wiki: &Value) -> Result<crate::core::kg::TextVersion, String> {
    save_rewritten_chapter_with_checkpoint(conn, book_id, chapter_idx, prose, model, dependency_digest, expected_line_version, wiki, None)
}

pub fn save_rewritten_chapter_with_checkpoint(conn: &Connection, book_id: i64, chapter_idx: i64, prose: &str, model: &str, dependency_digest: &str, expected_line_version: i64, wiki: &Value, job_id: Option<i64>) -> Result<crate::core::kg::TextVersion, String> {
    if wiki.get("chapter_idx").is_some_and(|idx| idx.as_i64() != Some(chapter_idx)) { return Err("Wiki chapter mismatch".into()); }
    // Paired output nests its chapter card; original extraction keeps its flat
    // input. Both are adapted to the same existing memory normalization path.
    let memory_input = if let Some(chapter) = wiki.get("chapter") {
        serde_json::json!({"chapter_idx":chapter_idx,"headline":chapter["headline"],"narrative":chapter["narrative"],"ending":chapter["ending"],"open_threads":chapter["ending"]["open_threads"],"entries":wiki["entries"]})
    } else { wiki.clone() };
    if prose.trim().is_empty() || model.trim().is_empty() || dependency_digest.trim().is_empty() { return Err("incomplete joint chapter result".into()); }
    let normalized = crate::store::story_projection::normalize_wiki(&memory_input, chapter_idx)?;
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    if crate::store::kg::line_version(&tx, book_id)? != expected_line_version { return Err("reading line changed; stale result cannot activate".into()); }
    let checkpoint = if let Some(job) = job_id {
        let (owner, kind, state): (Option<i64>, String, String) = tx.query_row("SELECT book_id,kind,state FROM jobs WHERE id=?1", [job], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).map_err(|error| error.to_string())?;
        let payload = crate::store::kg::job_checkpoint(&tx, job)?;
        if owner != Some(book_id) || kind != "generate" || !["queued", "running", "paused"].contains(&state.as_str()) || payload["chapter_idx"].as_i64() != Some(chapter_idx) { return Err("terminal or foreign job cannot publish rewritten chapter".into()); }
        Some((job, payload))
    } else { None };
    let progress = crate::store::kg::progress(&tx, book_id)?;
    if chapter_idx < 2 || chapter_idx > progress + 1 { return Err("only a visited chapter or the authorized next chapter can be rewritten".into()); }
    let previous = crate::store::kg::selected_text(&tx, book_id, chapter_idx - 1)?.ok_or("selected previous chapter text unavailable")?;
    if !crate::store::story_projection::chapter_memory_ready(&tx, book_id, chapter_idx - 1)? { return Err("selected previous chapter Wiki is not ready".into()); }
    let planned_previous: Option<i64> = tx.query_row("SELECT previous_text_version_id FROM story_next_wiki_revisions WHERE book_id=?1 AND chapter_idx=?2 AND active=1 AND status='ready'", params![book_id, chapter_idx], |row| row.get(0)).optional().map_err(|e| e.to_string())?;
    if planned_previous != Some(previous.id) { return Err("working Wiki does not match selected previous text".into()); }
    let history = crate::store::story_projection::story_map(&tx, book_id, chapter_idx - 1, expected_line_version)?;
    let mut allowed = history["entries"].as_array().ok_or("invalid actual history")?.iter().filter_map(|entry| entry["id"].as_str()).collect::<std::collections::HashSet<_>>();
    let entries = normalized["entries"].as_array().ok_or("invalid normalized Wiki")?;
    allowed.extend(entries.iter().filter_map(|entry| entry["id"].as_str()));
    for entry in entries {
        if entry["related_entries"].as_array().ok_or("invalid Wiki references")?.iter().any(|id| !id.as_str().is_some_and(|id| allowed.contains(id))) { return Err("Wiki reference is not qualified by selected actual history".into()); }
    }
    let previous_selection = crate::store::kg::selected_text(&tx, book_id, chapter_idx)?;
    let text = crate::store::kg::save_generated_in_transaction(&tx, book_id, chapter_idx, prose, model, dependency_digest, expected_line_version)?;
    let existing = crate::store::story_projection::chapter_memory_for_text(&tx, text.id)?;
    let memory_unchanged = existing.as_ref().is_some_and(|memory| memory["chapter"] == normalized["chapter"] && memory["entries"] == normalized["entries"]);
    let revision = if memory_unchanged {
        existing.as_ref().and_then(|memory| memory["revision_id"].as_i64()).ok_or("cached chapter memory has no revision")?
    } else {
        crate::store::story_projection::save_memory_in_transaction(&tx, &text, &memory_input, dependency_digest)?
    };
    if previous_selection.as_ref().is_none_or(|previous| previous.id != text.id) || !memory_unchanged {
        tx.execute("UPDATE story_next_wiki_revisions SET active=0,status='stale' WHERE book_id=?1 AND chapter_idx>?2 AND active=1", params![book_id, chapter_idx]).map_err(|error| error.to_string())?;
        tx.execute("UPDATE generated_chapters SET active=0 WHERE book_id=?1 AND chapter_idx>?2", params![book_id, chapter_idx]).map_err(|error| error.to_string())?;
        tx.execute("DELETE FROM kg_read_selection WHERE book_id=?1 AND chapter_idx>?2", params![book_id, chapter_idx]).map_err(|error| error.to_string())?;
    }
    if let Some((job, mut payload)) = checkpoint {
        payload["paired_commit"] = serde_json::json!({"text_version_id":text.id,"wiki_revision":revision,"dependency_digest":dependency_digest});
        payload.as_object_mut().ok_or("invalid rewrite checkpoint")?.remove("paired_draft");
        if tx.execute("UPDATE jobs SET payload_json=?2,progress=1.0,updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')", params![job, payload.to_string()]).map_err(|error| error.to_string())? != 1 { return Err("terminal job cannot commit rewritten chapter".into()); }
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(text)
}

#[cfg(test)]
#[path="story_tests.rs"]
mod tests;
