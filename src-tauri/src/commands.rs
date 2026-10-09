// IPC commands（薄层，dev-plan Task 2.3）
// 业务在 core/store，此处只做参数分派与状态桥接。

use crate::core::split::{split_txt, RawChapter, SplitOptions};
use crate::store::books::{create_book_with_chapters, list_books, NewChapter};
use rusqlite::Connection;
use std::path::Path;
use std::sync::Mutex;

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
            NewChapter { idx: c.idx, title: c.title, text: c.text, token_est: est }
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

    let (format, chapters) = match p.extension().and_then(|e| e.to_str()).map(str::to_lowercase) {
        Some(ext) if ext == "txt" => {
            let text = decode_txt(&bytes);
            ("txt", raw_to_new(split_txt(&text, &opts)))
        }
        Some(ext) if ext == "epub" => {
            let cs = crate::core::epub::split_epub(&bytes, &opts).map_err(|e| e)?;
            ("epub", raw_to_new(cs))
        }
        _ => return Err("仅支持 txt/epub".into()),
    };
    if chapters.is_empty() {
        return Err("未解析出任何章节".into());
    }
    let title = derive_title(p, &chapters);

    let mut guard = state.db.lock().map_err(|e| e.to_string())?;
    let conn = guard.as_mut().ok_or("数据库未初始化")?;
    create_book_with_chapters(conn, &title, None, format, &path, &chapters)
        .map_err(|e| format!("入库失败: {e}"))
}

/// txt 编码探测：优先 UTF-8，回退 GBK（windows-1252 不可靠，中文书常见 GBK）。
fn decode_txt(bytes: &[u8]) -> String {
    if let Ok(s) = std::str::from_utf8(bytes) {
        return s.to_string();
    }
    encoding_rs::GBK.decode(bytes).0.to_string()
}

#[tauri::command]
pub fn list_books_cmd(state: tauri::State<AppState>) -> Result<Vec<crate::store::books::Book>, String> {
    let guard = state.db.lock().map_err(|e| e.to_string())?;
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    list_books(conn).map_err(|e| e.to_string())
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
        let t = derive_title(Path::new("/books/武动乾坤.txt"), &[NewChapter { idx: 1, title: "a".into(), text: "t".into(), token_est: 1 }]);
        assert!(t.contains("武动乾坤") && t.contains("1章"));
    }

    #[test]
    fn decode_gbk_fallback() {
        // 「中文」GBK 编码
        let gbk = [0xd6, 0xd0, 0xce, 0xc4];
        assert_eq!(decode_txt(&gbk), "中文");
    }
}
