// IPC commands（薄层，dev-plan Task 2.3/2.4）
// 业务在 core/store，此处只做参数分派与状态桥接。

use crate::core::split::{split_txt, RawChapter, SplitOptions};
use crate::store::books::{create_book_with_chapters, get_chapter, list_books, NewChapter};
use parking_lot::Mutex;
use rusqlite::Connection;
use std::path::Path;

// 全局连接（单写多读；Tauri 状态管理在 lib.rs 注册）
pub struct AppState {
    pub db: Mutex<Option<Connection>>,
}

fn token_est(text: &str) -> i64 {
    // 粗估：中文 ~1.5 字/token → 2/3；保守取字符数
    (text.chars().count() as i64 * 2 / 3).max(1)
}

fn raw_to_new(rc: Vec<RawChapter>) -> Vec<NewChapter> {
    rc.into_iter()
        .map(|c| {
            let est = token_est(&c.text);
            NewChapter {
                idx: c.idx,
                title: c.title,
                text: c.text,
                token_est: est,
            }
        })
        .collect()
}

fn derive_title(path: &Path, chapters: &[NewChapter]) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "未命名".into())
        + &format!("（{}章）", chapters.len())
}

#[tauri::command]
pub fn import_book(state: tauri::State<AppState>, path: String) -> Result<i64, String> {
    let p = Path::new(&path);
    let bytes = std::fs::read(p).map_err(|e| format!("读取失败: {e}"))?;
    let opts = SplitOptions::default();

    let (format, chapters) = match p
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase)
    {
        Some(ext) if ext == "txt" => {
            let text = decode_txt(&bytes);
            ("txt", raw_to_new(split_txt(&text, &opts)))
        }
        Some(ext) if ext == "epub" => {
            let cs = crate::core::epub::split_epub(&bytes, &opts)?;
            ("epub", raw_to_new(cs))
        }
        _ => return Err("仅支持 txt/epub".into()),
    };
    if chapters.is_empty() {
        return Err("未解析出任何章节".into());
    }
    let title = derive_title(p, &chapters);

    let mut guard = state.db.lock();
    let conn = guard.as_mut().ok_or("数据库未初始化")?;
    create_book_with_chapters(conn, &title, None, format, &path, &chapters)
        .map_err(|e| format!("入库失败: {e}"))
}

/// txt 编码探测：优先 UTF-8，回退 GBK（中文书常见）。
fn decode_txt(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    encoding_rs::GBK.decode(bytes).0.to_string()
}

#[tauri::command]
pub fn list_books_cmd(
    state: tauri::State<AppState>,
) -> Result<Vec<crate::store::books::Book>, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    list_books(conn).map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct ChapterMeta {
    pub idx: i64,
    pub title: String,
}

#[tauri::command]
pub fn get_chapter_titles(
    state: tauri::State<AppState>,
    book_id: i64,
) -> Result<Vec<ChapterMeta>, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    let mut stmt = conn
        .prepare("SELECT idx, title FROM chapters WHERE book_id = ?1 ORDER BY idx")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([book_id], |r| {
            Ok(ChapterMeta {
                idx: r.get(0)?,
                title: r.get(1)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

#[derive(serde::Serialize)]
pub struct ChapterContent {
    pub idx: i64,
    pub title: String,
    pub content: String,
    pub source: String, // M1 阶段恒 "original"；阅读线接入后按 source(n) 解析
}

#[tauri::command]
pub fn get_chapter_cmd(
    state: tauri::State<AppState>,
    book_id: i64,
    idx: i64,
) -> Result<ChapterContent, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    match get_chapter(conn, book_id, idx) {
        Ok(Some((title, content))) => Ok(ChapterContent {
            idx,
            title,
            content,
            source: "original".into(),
        }),
        Ok(None) => Err(format!("章节不存在: {idx}")),
        Err(e) => Err(e.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_est_rough_count() {
        assert!(token_est("正文四个字") >= 2);
        assert_eq!(token_est(""), 1);
    }

    #[test]
    fn derive_title_contains_chapter_count() {
        let t = derive_title(
            Path::new("/books/武动乾坤.txt"),
            &[NewChapter {
                idx: 1,
                title: "a".into(),
                text: "t".into(),
                token_est: 1,
            }],
        );
        assert!(t.contains("武动乾坤") && t.contains("1章"));
    }

    #[test]
    fn decode_gbk_fallback() {
        // 「中文」GBK 编码
        let gbk = [0xd6, 0xd0, 0xce, 0xc4];
        assert_eq!(decode_txt(&gbk), "中文");
    }
}

pub mod providers;
