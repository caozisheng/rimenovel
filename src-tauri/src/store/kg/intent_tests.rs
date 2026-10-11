use super::*;
use crate::core::kg::{empty_view, ChapterSnapshot, GraphNode, GraphQuery, GraphView};
use crate::store::{books::{create_book_with_chapters, NewChapter}, Db};

fn fixture() -> (Connection, i64) {
    let conn = Connection::open_in_memory().unwrap();
    Db::apply_pragmas(&conn).unwrap();
    Db::new(&conn).migrate().unwrap();
    let chapters = (1..=4).map(|idx| NewChapter {
        idx,
        title: format!("Chapter {idx}"),
        text: "甲沿路走过城门。".into(),
        token_est: 20,
    }).collect::<Vec<_>>();
    let book = create_book_with_chapters(&conn, "state references", None, "txt", "test", &chapters).unwrap();
    (conn, book)
}

fn config() -> RuntimeConfig {
    RuntimeConfig { provider_id: 1, model_extract: "model".into(), context_limit: 32768, output_reserve: 4096, prompt_version: "kg-lite/v1".into() }
}

fn node(key: &str, entity_type: &str, name: &str, chapter_idx: i64, provenance: &str) -> GraphNode {
    GraphNode {
        key: key.into(), entity_type: entity_type.into(), name: name.into(), aliases: vec![], summary: None,
        life_status: (entity_type == "character").then(|| "alive".into()), location: None, owner: None,
        chapter_idx, provenance: provenance.into(),
    }
}

fn install_actual_only_references(conn: &Connection, book: i64) -> (String, String) {
    let original = text_version(conn, book, 1, "original", None).unwrap();
    let chapter_one = ChapterSnapshot {
        nodes: vec![node("reference-hero", "character", "原作人物", 1, "ai_original")],
        ..ChapterSnapshot::default()
    };
    let line = line_version(conn, book).unwrap();
    commit_snapshot(conn, &original, &chapter_one, "original", line, None, &config()).unwrap();
    mark_read(conn, book, 1).unwrap();

    edit(conn, &EditInput {
        book_id: book, scope: "global".into(), chapter_idx: None, kind: "directive".into(),
        action: "upsert".into(), key: None,
        data: serde_json::json!({"text": "继续当前故事线", "polarity": "positive"}),
    }).unwrap();
    let line = line_version(conn, book).unwrap();
    let generated = save_generated(conn, book, 2, "当前故事线正文", "writer", "generated", line).unwrap();
    let chapter_two = ChapterSnapshot {
        nodes: vec![
            node("actual-owner", "character", "当前人物", 2, "ai_generated"),
            node("actual-location", "location", "当前地点", 2, "ai_generated"),
        ],
        ..ChapterSnapshot::default()
    };
    commit_snapshot(conn, &generated, &chapter_two, "generated", line, None, &config()).unwrap();
    mark_read(conn, book, 2).unwrap();
    (public_key(book, "n", "actual-owner"), public_key(book, "n", "actual-location"))
}

fn install_hidden_future_spec(conn: &Connection, book: i64) -> String {
    let hidden_key = "spec-hidden-future-entity-0001";
    let mut hidden = GraphView { chapter_idx: Some(3), ..empty_view("spec") };
    hidden.nodes.push(node(hidden_key, "character", "未揭幕人物", 3, "ai_adapted"));
    save_spec(conn, book, 3, &hidden, "hidden-spec", line_version(conn, book).unwrap()).unwrap();
    hidden_key.into()
}

fn verify_future_references(conn: &Connection, book: i64, item_name: &str, internal_ref: &str, public_ref: &str, owner: bool) {
    let context = generation_context(conn, book, 3).unwrap();
    assert!(context.actual.nodes.iter().any(|node| node.key == "actual-owner"));
    assert!(context.actual.nodes.iter().any(|node| node.key == "actual-location"));
    let context_item = context.spec.nodes.iter().find(|node| node.name == item_name).unwrap();
    let context_ref = if owner { context_item.owner.as_deref() } else { context_item.location.as_deref() };
    assert_eq!(context_ref, Some(internal_ref));

    let public = get_view(conn, &GraphQuery { book_id: book, scope: "global".into(), purpose: "spec".into(), chapter_idx: Some(3) }).unwrap();
    let public_item = public.nodes.iter().find(|node| node.name == item_name).unwrap();
    let public_ref_key = if owner { public_item.owner.as_deref() } else { public_item.location.as_deref() };
    assert_eq!(public_ref_key, Some(public_ref));
    assert!(public.nodes.iter().any(|node| node.key == public_ref));
}

#[test]
fn actual_only_character_can_own_revealed_future_item_without_hidden_future_leak() {
    let (conn, book) = fixture();
    let (owner_key, location_key) = install_actual_only_references(&conn, book);
    let hidden_key = install_hidden_future_spec(&conn, book);

    let hidden = get_view(&conn, &GraphQuery { book_id: book, scope: "global".into(), purpose: "spec".into(), chapter_idx: Some(3) }).unwrap();
    assert!(hidden.nodes.is_empty());
    assert_eq!(hidden.fog.as_ref().unwrap().entities, 1);
    assert!(view::authorized_key(&conn, book, "node", &public_key(book, "n", &hidden_key)).is_err());

    let reference = get_view(&conn, &GraphQuery { book_id: book, scope: "global".into(), purpose: "reference".into(), chapter_idx: Some(3) }).unwrap();
    assert!(reference.nodes.iter().any(|node| node.key == public_key(book, "n", "reference-hero")));
    assert!(!reference.nodes.iter().any(|node| node.key == owner_key || node.key == location_key));

    reveal(&conn, book, 3).unwrap();
    edit(&conn, &EditInput {
        book_id: book, scope: "global".into(), chapter_idx: None, kind: "node".into(),
        action: "upsert".into(), key: None,
        data: serde_json::json!({"type": "item", "name": "人物之物", "owner": owner_key}),
    }).unwrap();
    verify_future_references(&conn, book, "人物之物", "actual-owner", &owner_key, true);
}

#[test]
fn actual_only_location_can_locate_revealed_future_item() {
    let (conn, book) = fixture();
    let (_, location_key) = install_actual_only_references(&conn, book);
    install_hidden_future_spec(&conn, book);
    reveal(&conn, book, 3).unwrap();
    edit(&conn, &EditInput {
        book_id: book, scope: "global".into(), chapter_idx: None, kind: "node".into(),
        action: "upsert".into(), key: None,
        data: serde_json::json!({"type": "item", "name": "地点之物", "location": location_key}),
    }).unwrap();
    verify_future_references(&conn, book, "地点之物", "actual-location", &location_key, false);
}
