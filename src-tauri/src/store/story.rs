use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::core::story::{InterventionInput, NextChapterWiki};

pub fn save_intervention(conn: &Connection, input: &InterventionInput) -> Result<i64, String> {
    let selected = serde_json::to_string(&input.selected_option_ids).map_err(|e| e.to_string())?;
    let overridden = serde_json::to_string(&input.overridden_option_ids).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT INTO story_interventions (book_id,chapter_idx,batch_index,question_batch_id,selected_json,free_text,overridden_json,state,effective_from,reading_line_version) VALUES (?1,?2,?3,?4,?5,?6,?7,'active',?2,?8)",
        params![input.book_id, input.chapter_idx, input.batch_index, input.question_batch_id, selected, input.free_text, overridden, input.reading_line_version],
    ).map_err(|e| e.to_string())?;
    Ok(conn.last_insert_rowid())
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

#[cfg(test)]
#[path="story_tests.rs"]
mod tests;
