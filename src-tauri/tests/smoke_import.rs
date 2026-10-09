// 端到端冒烟：txt 内容 → split_txt → create_book_with_chapters → get_chapter 全链路
// 运行: cargo test --manifest-path src-tauri/Cargo.toml --test smoke_import

use rimenovel_lib::core::split::{split_txt, SplitOptions};
use rimenovel_lib::store::books::{create_book_with_chapters, get_chapter, NewChapter};
use rusqlite::Connection;

#[test]
fn import_to_read_pipeline() {
    let novel = "楔子\n远古大战的传说。\n第一章 起点\n林动站在山脚。\n他说：「这功法有第二章境界。」\n第二章 风波\n矿洞深处传来脚步声。\n第三章 破境\n石符苏醒了。\n";
    let chapters = split_txt(novel, &SplitOptions::default());
    assert_eq!(chapters.len(), 4, "楔子+3章, 对话中第二章不误切: {chapters:?}");

    let conn = Connection::open_in_memory().unwrap();
    rimenovel_lib::store::Db::apply_pragmas(&conn).unwrap();
    rimenovel_lib::store::Db::new(&conn).migrate().unwrap();

    let new: Vec<NewChapter> = chapters
        .into_iter()
        .map(|c| NewChapter { idx: c.idx, title: c.title, text: c.text, token_est: 10 })
        .collect();
    let id = create_book_with_chapters(&conn, "冒烟测试之书", None, "txt", "/smoke/novel.txt", &new).unwrap();

    let (t2, c2) = get_chapter(&conn, id, 2).unwrap().unwrap();
    assert_eq!(t2, "第一章 起点");
    assert!(c2.contains("林动站在山脚"));
    let (t4, c4) = get_chapter(&conn, id, 4).unwrap().unwrap();
    assert_eq!(t4, "第三章 破境");
    assert!(c4.contains("石符"));
}
