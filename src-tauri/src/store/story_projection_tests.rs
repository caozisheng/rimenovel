use rusqlite::{params, Connection};
use serde_json::{json, Value};
use crate::core::kg::{ChapterSnapshot, GraphNode};
use crate::core::story::NextChapterWiki;
use crate::store::{books::{create_book_with_chapters, NewChapter}, kg, story, Db};
use super::*;

fn database() -> (Connection, i64) {
    let conn = Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();
    Db::new(&conn).migrate().unwrap();
    let chapters = (1..=3).map(|idx| NewChapter { idx, title: format!("第{idx}章"), text: format!("原作第{idx}章"), token_est: 10 }).collect::<Vec<_>>();
    let book = create_book_with_chapters(&conn, "来源隔离", None, "txt", "memory", &chapters).unwrap();
    kg::mark_read(&conn, book, 1).unwrap();
    (conn, book)
}

fn original(conn: &Connection, book: i64, chapter: i64, key: &str, summary: &str) -> i64 {
    let text = kg::text_version(conn, book, chapter, "original", None).unwrap();
    let snapshot = ChapterSnapshot { summary: summary.into(), nodes: vec![GraphNode { key: key.into(), entity_type: "character".into(), name: key.into(), aliases: vec![], summary: Some(summary.into()), life_status: None, location: None, owner: None, chapter_idx: chapter, provenance: "ai_original".into() }], ..ChapterSnapshot::default() };
    save_chapter_memory(conn, &text, &snapshot, "original").unwrap()
}

fn wiki(chapter: i64, key: &str, summary: &str) -> Value {
    json!({"headline":summary,"narrative":summary,"ending":{"open_threads":[]},"entries":[{"id":key,"type":"character","name":key,"aliases":[],"summary":summary,"related_entries":[],"source_chapters":[chapter]}],"open_threads":[]})
}

fn plan(conn: &Connection, book: i64, chapter: i64) {
    let previous = kg::selected_text(conn, book, chapter - 1).unwrap().unwrap();
    let next = NextChapterWiki { chapter_idx: chapter, goals: vec!["继续".into()], state_directions: vec![], open_threads: vec![], avoid: vec![], ending_direction: "继续".into(), related_entry_ids: vec![] };
    story::save_next_wiki(conn, book, chapter, previous.id, 0, "rewrite", &next).unwrap();
}

#[test]
fn preextracted_second_original_is_private_until_visited() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    original(&conn, book, 2, "original-only", "原作秘密");
    let public = story_map(&conn, book, 3, 0).unwrap();
    assert_eq!(public["entries"].as_array().unwrap().len(), 1);
    assert_eq!(public["chapter_cards"][1]["summary"], Value::Null);
    assert!(original_reference_wiki(&conn, book, 2).unwrap().is_some());
    assert!(active_chapter_memory(&conn, book, 2).unwrap().is_none());
    assert!(!chapter_memory_ready(&conn, book, 2).unwrap());
}

#[test]
fn joint_rewrite_is_ready_without_snapshot_and_original_reextract_does_not_leak() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    original(&conn, book, 2, "original-only", "原作摘要");
    plan(&conn, book, 2);
    let text = story::save_rewritten_chapter(&conn, book, 2, "改写正文", "writer", "rewrite", kg::line_version(&conn, book).unwrap(), &wiki(2, "rewritten-only", "改写摘要")).unwrap();
    kg::mark_read(&conn, book, 2).unwrap();
    original(&conn, book, 2, "original-only", "重抽原作摘要");
    let public = story_map(&conn, book, 2, kg::line_version(&conn, book).unwrap()).unwrap();
    assert_eq!(public["entries"].as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect::<Vec<_>>(), vec!["first", "rewritten-only"]);
    assert_eq!(public["chapter_cards"][1]["summary"], "改写摘要");
    assert_eq!(public["chapter_cards"][1]["source"], "generated");
    assert_eq!(public["chapter_cards"][1]["text_version_id"], text.id);
    assert!(chapter_memory_ready(&conn, book, 2).unwrap());
    assert!(!kg::ready(&conn, text.id).unwrap());
    assert_eq!(original_reference_wiki(&conn, book, 2).unwrap().unwrap()["chapter"]["narrative"], "重抽原作摘要");
}

#[test]
fn exact_selected_original_pin_wins_over_latest_generated_memory() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    original(&conn, book, 2, "original-only", "原作摘要");
    plan(&conn, book, 2);
    story::save_rewritten_chapter(&conn, book, 2, "改写正文", "writer", "rewrite", kg::line_version(&conn, book).unwrap(), &wiki(2, "rewritten-only", "改写摘要")).unwrap();
    kg::mark_read(&conn, book, 2).unwrap();
    let rewritten = story_map(&conn, book, 2, 0).unwrap();
    let text = kg::text_version(&conn, book, 2, "original", None).unwrap();
    conn.execute("UPDATE kg_read_selection SET text_version_id=?3 WHERE book_id=?1 AND chapter_idx=?2", params![book, 2, text.id]).unwrap();
    let public = story_map(&conn, book, 2, 0).unwrap();
    assert_eq!(public["chapter_cards"][1]["source"], "original");
    assert_eq!(public["chapter_cards"][1]["summary"], "原作摘要");
    assert_ne!(public["wiki_revision"], rewritten["wiki_revision"]);
    assert!(!public["entries"].as_array().unwrap().iter().any(|v| v["id"] == "rewritten-only"));
}

#[test]
fn same_progress_reextraction_changes_revision_identity() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "旧摘要");
    let before = story_map(&conn, book, 1, 0).unwrap();
    original(&conn, book, 1, "first", "新摘要");
    let after = story_map(&conn, book, 1, 0).unwrap();
    assert_ne!(before["wiki_revision"], after["wiki_revision"]);
    assert_eq!(after["chapter_cards"][0]["summary"], "新摘要");
}


#[test]
fn paired_result_and_cached_memory_stay_private_until_read() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    plan(&conn, book, 2);
    let text = story::save_rewritten_chapter(&conn, book, 2, "暂存改写正文", "writer", "rewrite", kg::line_version(&conn, book).unwrap(), &wiki(2, "private-rewrite", "私有摘要")).unwrap();
    assert!(chapter_memory_for_text(&conn, text.id).unwrap().is_some());
    assert!(active_chapter_memory(&conn, book, 2).unwrap().is_none());
    let public = story_map(&conn, book, 3, 0).unwrap();
    assert_eq!(public["chapter_cards"][1]["headline"], Value::Null);
    assert!(!public["entries"].as_array().unwrap().iter().any(|entry| entry["id"] == "private-rewrite"));
}
#[test]
fn invalid_joint_wiki_rolls_back_prose_and_selection() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    plan(&conn, book, 2);
    let mut invalid = wiki(2, "rewrite", "摘要");
    invalid["entries"][0]["source_chapters"] = json!([3]);
    let result = story::save_rewritten_chapter(&conn, book, 2, "正文", "writer", "rewrite", kg::line_version(&conn, book).unwrap(), &invalid);
    assert!(result.is_err());
    let count: i64 = conn.query_row("SELECT count(*) FROM generated_chapters WHERE book_id=?1", [book], |r| r.get(0)).unwrap();
    assert_eq!(count, 0);
    assert_eq!(kg::selected_text(&conn, book, 2).unwrap().unwrap().source, "original");
}

#[test]
fn joint_wiki_cannot_reference_unselected_future_original_entry() {
    let (conn, book) = database();
    original(&conn, book, 1, "first", "第一章");
    original(&conn, book, 3, "future-secret", "未来秘密");
    plan(&conn, book, 2);
    let mut invalid = wiki(2, "rewrite", "摘要");
    invalid["entries"][0]["related_entries"] = json!(["future-secret"]);
    assert!(story::save_rewritten_chapter(&conn, book, 2, "正文", "writer", "rewrite", kg::line_version(&conn, book).unwrap(), &invalid).is_err());
}
