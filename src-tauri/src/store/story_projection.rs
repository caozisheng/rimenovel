use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use crate::core::kg::{digest, ChapterSnapshot, TextVersion};

/// Adapt original extraction into the same per-text memory format as joint rewrites.
/// Saving an original reference never changes selected text or reading progress.
pub fn save_chapter_memory(conn: &Connection, text: &TextVersion, snapshot: &ChapterSnapshot, dependency_digest: &str) -> Result<i64, String> {
    if snapshot.nodes.iter().any(|node| node.chapter_idx != text.chapter_idx) { return Err("chapter Wiki contains unauthorized source chapters".into()); }
    let names = snapshot.nodes.iter().map(|node| (node.key.as_str(), node.name.as_str())).collect::<HashMap<_, _>>();
    let mut related = HashMap::<&str, Vec<String>>::new();
    for edge in &snapshot.edges {
        related.entry(edge.src.as_str()).or_default().push(edge.dst.clone());
        related.entry(edge.dst.as_str()).or_default().push(edge.src.clone());
    }
    let entries = snapshot.nodes.iter().map(|node| {
        let mut state_items = Vec::new();
        if let Some(summary) = node.summary.as_deref().filter(|value| !value.trim().is_empty()) { state_items.push(summary.to_owned()); }
        if let Some(status) = node.life_status.as_deref() { state_items.push(format!("生存状态：{status}")); }
        if let Some(location) = node.location.as_deref() { state_items.push(format!("所在地点：{}", names.get(location).copied().unwrap_or(location))); }
        if let Some(owner) = node.owner.as_deref() { state_items.push(format!("持有者：{}", names.get(owner).copied().unwrap_or(owner))); }
        json!({"id":node.key,"type":node.entity_type,"name":node.name,"aliases":node.aliases,
            "summary":node.summary.clone().unwrap_or_default(),"state_items":state_items,
            "related_entries":related.get(node.key.as_str()).cloned().unwrap_or_default(),
            "open_threads":snapshot.open_threads,"source_chapters":[text.chapter_idx]})
    }).collect::<Vec<_>>();
    let wiki = json!({"headline":snapshot.summary,"narrative":snapshot.summary,"ending":{"open_threads":snapshot.open_threads},"entries":entries,"open_threads":snapshot.open_threads});
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let revision = save_memory_in_transaction(&tx, text, &wiki, dependency_digest)?;
    tx.commit().map_err(|e| e.to_string())?;
    Ok(revision)
}

fn nonempty_string<'a>(value: &'a Value, field: &str) -> Result<&'a str, String> {
    value.get(field).and_then(Value::as_str).filter(|s| !s.trim().is_empty()).ok_or_else(|| format!("Wiki {field} must be a nonempty string"))
}

fn string_array(value: &Value, field: &str) -> Result<(), String> {
    if !value.get(field).and_then(Value::as_array).is_some_and(|values| values.iter().all(Value::is_string)) {
        return Err(format!("Wiki {field} must be a string array"));
    }
    Ok(())
}

/// Validate source indices independently of the model's semantic claims. Reference
/// qualification against selected history is additionally enforced by the writer.
pub(crate) fn normalize_wiki(wiki: &Value, chapter_idx: i64) -> Result<Value, String> {
    if !wiki.is_object() { return Err("chapter Wiki must be an object".into()); }
    if wiki.get("chapter_idx").is_some_and(|idx| idx.as_i64() != Some(chapter_idx)) { return Err("Wiki chapter mismatch".into()); }
    nonempty_string(wiki, "headline")?;
    nonempty_string(wiki, "narrative")?;
    if !wiki.get("ending").is_some_and(|ending| ending.is_object() || ending.is_string()) { return Err("Wiki ending must be an object or string".into()); }
    string_array(wiki, "open_threads")?;
    let entries = wiki.get("entries").and_then(Value::as_array).ok_or("Wiki entries must be an array")?;
    let mut ids = HashSet::new();
    let mut public = Vec::with_capacity(entries.len());
    for entry in entries {
        let id = nonempty_string(entry, "id")?;
        if !ids.insert(id) { return Err("Wiki entry IDs must be unique".into()); }
        nonempty_string(entry, "name")?;
        let kind = match entry.get("type").or_else(|| entry.get("kind")).and_then(Value::as_str) {
            Some("character") => "character", Some("faction") => "faction", Some("location") => "location", Some("item") => "item", Some("concept" | "setting") => "setting", _ => return Err("invalid Wiki entry type".into()),
        };
        if !entry.get("summary").is_some_and(Value::is_string) { return Err("Wiki entry summary must be a string".into()); }
        for field in ["aliases", "related_entries"] { string_array(entry, field)?; }
        for field in ["state_items", "open_threads"] { if entry.get(field).is_some() { string_array(entry, field)?; } }
        let sources = entry.get("source_chapters").and_then(Value::as_array).ok_or("Wiki source_chapters must be an array")?;
        if sources.is_empty() || sources.iter().any(|source| !source.as_i64().is_some_and(|idx| idx > 0 && idx <= chapter_idx)) { return Err("Wiki contains unauthorized future source chapters".into()); }
        public.push(json!({"id":id,"kind":kind,"name":entry["name"],"aliases":entry["aliases"],"summary":entry["summary"],
            "state_items":entry.get("state_items").cloned().unwrap_or_else(|| json!([])),"related_entries":entry["related_entries"],
            "open_threads":entry.get("open_threads").unwrap_or(&wiki["open_threads"]),"source_chapters":sources}));
    }
    Ok(json!({"chapter":{"headline":wiki["headline"],"narrative":wiki["narrative"],"ending":wiki["ending"]},
        "entry_ids":public.iter().map(|entry| entry["id"].clone()).collect::<Vec<_>>(),"entries":public}))
}

/// Caller owns the transaction, allowing prose, selection and Wiki to commit once.
pub(crate) fn save_memory_in_transaction(conn: &Connection, text: &TextVersion, wiki: &Value, dependency_digest: &str) -> Result<i64, String> {
    let data = normalize_wiki(wiki, text.chapter_idx)?;
    let belongs: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM kg_text_versions WHERE id=?1 AND book_id=?2 AND chapter_idx=?3 AND content_hash=?4)", params![text.id, text.book_id, text.chapter_idx, digest(text.content.as_bytes())], |row| row.get(0)).map_err(|e| e.to_string())?;
    if !belongs || digest(text.content.as_bytes()) != text.content_hash { return Err("Wiki text ownership mismatch".into()); }
    conn.execute("UPDATE story_chapter_revisions SET active=0,status='stale' WHERE text_version_id=?1 AND active=1", params![text.id]).map_err(|e| e.to_string())?;
    conn.execute("INSERT INTO story_chapter_revisions (book_id,text_version_id,dependency_digest,data_json,status) VALUES (?1,?2,?3,?4,'ready')", params![text.book_id, text.id, dependency_digest, data.to_string()]).map_err(|e| e.to_string())?;
    let revision_id = conn.last_insert_rowid();
    for entry in data["entries"].as_array().ok_or("invalid normalized Wiki")? {
        conn.execute("INSERT INTO story_wiki_entries (book_id,entry_kind,stable_key) VALUES (?1,?2,?3) ON CONFLICT(book_id,stable_key) DO NOTHING", params![text.book_id, entry["kind"].as_str(), entry["id"].as_str()]).map_err(|e| e.to_string())?;
        let entry_id: i64 = conn.query_row("SELECT id FROM story_wiki_entries WHERE book_id=?1 AND stable_key=?2", params![text.book_id, entry["id"].as_str()], |row| row.get(0)).map_err(|e| e.to_string())?;
        conn.execute("INSERT INTO story_wiki_observations (chapter_revision_id,entry_id,data_json,source_chapter) VALUES (?1,?2,?3,?4)", params![revision_id, entry_id, entry.to_string(), text.chapter_idx]).map_err(|e| e.to_string())?;
    }
    Ok(revision_id)
}

fn memory_for_text(conn: &Connection, text: &TextVersion) -> Result<Option<Value>, String> {
    if digest(text.content.as_bytes()) != text.content_hash { return Err("selected text content hash mismatch".into()); }
    let revision: Option<(i64, String)> = conn.query_row("SELECT id,data_json FROM story_chapter_revisions WHERE book_id=?1 AND text_version_id=?2 AND active=1 AND status='ready'", params![text.book_id, text.id], |row| Ok((row.get(0)?, row.get(1)?))).optional().map_err(|e| e.to_string())?;
    let Some((revision_id, raw)) = revision else { return Ok(None); };
    let mut data: Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
    if !data.is_object() { return Err("stored chapter memory is not an object".into()); }
    let mut stmt = conn.prepare("SELECT data_json FROM story_wiki_observations WHERE chapter_revision_id=?1 AND source_chapter=?2 ORDER BY id").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![revision_id, text.chapter_idx], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())?;
    let entries = rows.map(|row| serde_json::from_str::<Value>(&row.map_err(|e| e.to_string())?).map_err(|e| e.to_string())).collect::<Result<Vec<_>, _>>()?;
    // Also qualify older cached observations: malformed/future evidence never
    // becomes selected actual history merely because the parent revision is ready.
    for entry in &entries {
        let valid_sources = entry.get("source_chapters").and_then(Value::as_array).is_some_and(|sources| !sources.is_empty() && sources.iter().all(|source| source.as_i64().is_some_and(|idx| idx > 0 && idx <= text.chapter_idx)));
        if !valid_sources { return Err("stored chapter memory has unauthorized source chapters".into()); }
    }
    data["entries"] = json!(entries);
    data["revision_id"] = json!(revision_id);
    data["text_version_id"] = json!(text.id);
    data["chapter_idx"] = json!(text.chapter_idx);
    data["source"] = json!(text.source);
    Ok(Some(data))
}

/// Backend-only hidden original reference. Never expose this via public Wiki APIs.
pub(crate) fn original_reference_wiki(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<Option<Value>, String> {
    let text = crate::store::kg::text_version(conn, book_id, chapter_idx, "original", None)?;
    memory_for_text(conn, &text)
}

/// Backend-only exact-text lookup for paired cache completeness. It does not
/// grant selected-history status or reveal a future chapter to public consumers.
pub fn chapter_memory_for_text(conn: &Connection, text_version_id: i64) -> Result<Option<Value>, String> {
    let text: Option<TextVersion> = conn.query_row("SELECT id,book_id,chapter_idx,source,content,content_hash,generated_id FROM kg_text_versions WHERE id=?1", [text_version_id], |row| Ok(TextVersion { id:row.get(0)?, book_id:row.get(1)?, chapter_idx:row.get(2)?, source:row.get(3)?, content:row.get(4)?, content_hash:row.get(5)?, generated_id:row.get(6)? })).optional().map_err(|e| e.to_string())?;
    text.as_ref().map(|text| memory_for_text(conn, text)).transpose().map(Option::flatten)
}

pub fn text_memory_ready(conn: &Connection, text_version_id: i64) -> Result<bool, String> {
    Ok(chapter_memory_for_text(conn, text_version_id)?.is_some())
}

/// Actual history always follows exact selected TextVersion and visited boundary.
pub fn active_chapter_memory(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<Option<Value>, String> {
    if chapter_idx < 1 || chapter_idx > crate::store::kg::progress(conn, book_id)? { return Ok(None); }
    let Some(text) = crate::store::kg::selected_text(conn, book_id, chapter_idx)? else { return Ok(None); };
    if text.book_id != book_id || text.chapter_idx != chapter_idx { return Err("selected text belongs to another chapter".into()); }
    let Some(mut memory) = memory_for_text(conn, &text)? else { return Ok(None); };
    let actual = story_map(conn, book_id, chapter_idx, 0)?;
    let allowed = actual["entries"].as_array().ok_or("invalid actual history")?.iter().filter_map(|entry| entry["id"].as_str()).collect::<HashSet<_>>();
    for entry in memory["entries"].as_array_mut().ok_or("invalid stored Wiki entries")? {
        if let Some(related) = entry.get_mut("related_entries").and_then(Value::as_array_mut) { related.retain(|id| id.as_str().is_some_and(|id| allowed.contains(id))); }
    }
    Ok(Some(memory))
}

pub fn chapter_memory_ready(conn: &Connection, book_id: i64, chapter_idx: i64) -> Result<bool, String> {
    Ok(active_chapter_memory(conn, book_id, chapter_idx)?.is_some())
}

pub fn story_map(conn: &Connection, book_id: i64, progress: i64, _line_version: i64) -> Result<Value, String> {
    // Callers may ask for a historical prefix, never for an unvisited future.
    let progress = progress.max(0).min(crate::store::kg::progress(conn, book_id)?);
    let line_version = crate::store::kg::line_version(conn, book_id)?;
    let mut entries = Vec::<Value>::new();
    let mut positions = HashMap::<String, usize>::new();
    let mut cards = Vec::new();
    let mut ready = Vec::new();
    let mut visited = Vec::new();
    let mut missing = Vec::new();
    let mut identities = Vec::new();
    let mut chapters = conn.prepare("SELECT idx,title FROM chapters WHERE book_id=?1 ORDER BY idx").map_err(|e| e.to_string())?;
    let chapter_rows = chapters.query_map([book_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
    for row in chapter_rows {
        let (idx, title) = row.map_err(|e| e.to_string())?;
        let mut card = json!({"chapter_idx":idx,"title":title,"headline":null,"summary":null,"entry_ids":[],"ready":false,"source":null,"text_version_id":null});
        if idx <= progress {
            visited.push(idx);
            if let Some(text) = crate::store::kg::selected_text(conn, book_id, idx)? {
                if text.book_id != book_id || text.chapter_idx != idx { return Err("selected text belongs to another chapter".into()); }
                card["source"] = json!(text.source);
                card["text_version_id"] = json!(text.id);
                let memory = memory_for_text(conn, &text)?;
                identities.push(json!({"chapter_idx":idx,"text_version_id":text.id,"memory":memory}));
                if let Some(data) = memory {
                    card["headline"] = data["chapter"]["headline"].clone();
                    card["summary"] = data["chapter"]["narrative"].clone();
                    card["entry_ids"] = data["entry_ids"].clone();
                    card["ready"] = json!(true);
                    ready.push(idx);
                    for value in data["entries"].as_array().ok_or("invalid stored Wiki entries")? {
                        let id = nonempty_string(value, "id")?.to_owned();
                        if let Some(position) = positions.get(&id).copied() { entries[position] = value.clone(); }
                        else { positions.insert(id, entries.len()); entries.push(value.clone()); }
                    }
                }
            }
        }
        if card["ready"] != true { missing.push(idx); }
        cards.push(card);
    }
    // Links from old cached observations are qualified after the selected-history
    // merge, so an original-only entry cannot leak via a generated relation.
    let visible = entries.iter().filter_map(|entry| entry["id"].as_str().map(str::to_owned)).collect::<HashSet<_>>();
    for entry in &mut entries {
        if let Some(related) = entry.get_mut("related_entries").and_then(Value::as_array_mut) {
            related.retain(|id| id.as_str().is_some_and(|id| visible.contains(id)));
        }
    }
    let hash = digest(json!({"visited":visited,"identities":identities}).to_string().as_bytes());
    let revision = i64::from_str_radix(&hash[..13], 16).map_err(|e| e.to_string())?;
    let complete = !visited.is_empty() && ready.len() == visited.len();
    Ok(json!({"entries":entries,"chapter_cards":cards,"coverage":{"visited":visited,"ready":ready,"missing":missing},"wiki_revision":revision,"reading_line_version":line_version,"ready":complete}))
}

#[cfg(test)]
#[path = "story_projection_tests.rs"]
mod tests;
