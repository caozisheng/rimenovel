use rusqlite::{params, Connection, OptionalExtension};
use crate::core::kg::{ChapterSnapshot, TextVersion};

pub fn save_chapter_memory(conn: &Connection, text: &TextVersion, snapshot: &ChapterSnapshot, dependency_digest: &str) -> Result<i64, String> {
    let names = snapshot.nodes.iter().map(|node| (node.key.as_str(), node.name.as_str())).collect::<std::collections::HashMap<_, _>>();
    let mut related = std::collections::HashMap::<&str, Vec<String>>::new();
    for edge in &snapshot.edges {
        related.entry(edge.src.as_str()).or_default().push(edge.dst.clone());
        related.entry(edge.dst.as_str()).or_default().push(edge.src.clone());
    }
    let data = serde_json::json!({
        "chapter": {"headline": snapshot.summary, "narrative": snapshot.summary, "ending": {"open_threads": snapshot.open_threads}},
        "entry_ids": snapshot.nodes.iter().map(|node| node.key.clone()).collect::<Vec<_>>()
    });
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    tx.execute("UPDATE story_chapter_revisions SET active=0,status='stale' WHERE text_version_id=?1 AND active=1", params![text.id]).map_err(|e| e.to_string())?;
    tx.execute("INSERT INTO story_chapter_revisions (book_id,text_version_id,dependency_digest,data_json,status) VALUES (?1,?2,?3,?4,'ready')", params![text.book_id, text.id, dependency_digest, data.to_string()]).map_err(|e| e.to_string())?;
    let revision_id = tx.last_insert_rowid();
    for node in &snapshot.nodes {
        let kind = match node.entity_type.as_str() { "character"=>"character", "faction"=>"faction", "location"=>"location", "item"=>"item", _=>"setting" };
        tx.execute("INSERT INTO story_wiki_entries (book_id,entry_kind,stable_key) VALUES (?1,?2,?3) ON CONFLICT(book_id,stable_key) DO NOTHING", params![text.book_id, kind, node.key]).map_err(|e| e.to_string())?;
        let entry_id: i64 = tx.query_row("SELECT id FROM story_wiki_entries WHERE book_id=?1 AND stable_key=?2", params![text.book_id, node.key], |row| row.get(0)).map_err(|e| e.to_string())?;
        let mut state_items = Vec::new();
        if let Some(summary) = node.summary.as_deref().filter(|value| !value.trim().is_empty()) { state_items.push(summary.to_owned()); }
        if let Some(status) = node.life_status.as_deref() { state_items.push(format!("生存状态：{status}")); }
        if let Some(location) = node.location.as_deref() { state_items.push(format!("所在地点：{}", names.get(location).copied().unwrap_or(location))); }
        if let Some(owner) = node.owner.as_deref() { state_items.push(format!("持有者：{}", names.get(owner).copied().unwrap_or(owner))); }
        let public = serde_json::json!({
            "id":node.key,"kind":kind,"name":node.name,"aliases":node.aliases,
            "summary":node.summary.clone().unwrap_or_default(),"state_items":state_items,
            "related_entries":related.get(node.key.as_str()).cloned().unwrap_or_default(),
            "open_threads":snapshot.open_threads,"source_chapters":[node.chapter_idx]
        });
        tx.execute("INSERT INTO story_wiki_observations (chapter_revision_id,entry_id,data_json,source_chapter) VALUES (?1,?2,?3,?4)", params![revision_id, entry_id, public.to_string(), text.chapter_idx]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| e.to_string())?;
    Ok(revision_id)
}

pub fn story_map(conn: &Connection, book_id: i64, progress: i64, line_version: i64) -> Result<serde_json::Value, String> {
    let mut entries = Vec::new();
    let mut stmt = conn.prepare("SELECT o.data_json FROM story_wiki_observations o JOIN story_chapter_revisions r ON r.id=o.chapter_revision_id WHERE r.book_id=?1 AND r.active=1 AND r.status='ready' AND o.source_chapter<=?2 ORDER BY o.source_chapter,o.id").map_err(|e| e.to_string())?;
    let rows = stmt.query_map(params![book_id, progress], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())?;
    let mut positions = std::collections::HashMap::<String, usize>::new();
    for row in rows {
        let raw = row.map_err(|e| e.to_string())?;
        let value: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
        let id = value["id"].as_str().unwrap_or_default().to_owned();
        if let Some(position) = positions.get(&id).copied() { entries[position] = value; } else { positions.insert(id, entries.len()); entries.push(value); }
    }
    let mut cards = Vec::new();
    let mut chapters = conn.prepare("SELECT idx,title FROM chapters WHERE book_id=?1 ORDER BY idx").map_err(|e| e.to_string())?;
    let chapter_rows = chapters.query_map([book_id], |row| Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))).map_err(|e| e.to_string())?;
    for row in chapter_rows {
        let (idx, title) = row.map_err(|e| e.to_string())?;
        let revision: Option<(String, String)> = if idx <= progress { conn.query_row("SELECT r.data_json,r.status FROM story_chapter_revisions r JOIN kg_text_versions t ON t.id=r.text_version_id WHERE r.book_id=?1 AND t.chapter_idx=?2 AND r.active=1 ORDER BY r.id DESC LIMIT 1", params![book_id, idx], |r| Ok((r.get(0)?, r.get(1)?))).optional().map_err(|e| e.to_string())? } else { None };
        let (headline, summary, entry_ids, ready) = match revision {
            Some((raw, status)) if status == "ready" => {
                let data: serde_json::Value = serde_json::from_str(&raw).map_err(|e| e.to_string())?;
                (data["chapter"]["headline"].as_str().map(str::to_owned), data["chapter"]["narrative"].as_str().map(str::to_owned), data["entry_ids"].clone(), true)
            }
            _ => (None, None, serde_json::json!([]), false)
        };
        cards.push(serde_json::json!({"chapter_idx":idx,"title":title,"headline":headline,"summary":summary,"entry_ids":entry_ids,"ready":ready}));
    }
    let total = cards.len() as i64;
    let ready = cards.iter().filter_map(|card| card["ready"].as_bool().and_then(|is_ready| is_ready.then(|| card["chapter_idx"].as_i64()).flatten())).collect::<Vec<_>>();
    Ok(serde_json::json!({"entries":entries,"chapter_cards":cards,"coverage":{"visited":(1..=progress).collect::<Vec<_>>(),"ready":ready,"missing":((progress+1)..=total).collect::<Vec<_>>()},"wiki_revision":progress,"reading_line_version":line_version,"ready":!entries.is_empty()}))
}

