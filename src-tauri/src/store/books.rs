// books/chapters 仓储（dev-plan Task 1.3）
// 事务化批量写入；读路径为书架与阅读器服务。

use crate::store::Db;
use rusqlite::{params, Connection};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Book {
    pub id: i64,
    pub title: String,
    pub author: Option<String>,
    pub format: String,
    pub source_path: String,
}

#[derive(Debug, Clone)]
pub struct NewChapter {
    pub idx: i64, // 从 1 起
    pub title: String,
    pub text: String,
    pub token_est: i64,
}

/// 分章结果 → 建书（单事务：books 行 + 全部 chapters 行）。
pub fn create_book_with_chapters(
    conn: &Connection,
    title: &str,
    author: Option<&str>,
    format: &str,
    source_path: &str,
    chapters: &[NewChapter],
) -> rusqlite::Result<i64> {
    let _db = Db::new(conn); // 标记归属；本函数以连接维度组织
    conn.execute_batch("BEGIN")?;
    let result = (|| -> rusqlite::Result<i64> {
        conn.execute(
            "INSERT INTO books (title, author, source_path, format) VALUES (?1, ?2, ?3, ?4)",
            params![title, author, source_path, format],
        )?;
        let book_id = conn.last_insert_rowid();
        let mut stmt = conn.prepare(
            "INSERT INTO chapters (book_id, idx, title, original_text, token_est)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for ch in chapters {
            stmt.execute(params![book_id, ch.idx, ch.title, ch.text, ch.token_est])?;
        }
        conn.execute(
            "INSERT INTO reader_intent (book_id, drift_pref) VALUES (?1, 0)",
            params![book_id],
        )?;
        Ok(book_id)
    })();
    match result {
        Ok(id) => {
            conn.execute_batch("COMMIT")?;
            Ok(id)
        }
        Err(e) => {
            let _ = conn.execute_batch("ROLLBACK");
            Err(e)
        }
    }
}

pub fn list_books(conn: &Connection) -> rusqlite::Result<Vec<Book>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, author, format, source_path FROM books ORDER BY id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Book {
            id: r.get(0)?,
            title: r.get(1)?,
            author: r.get(2)?,
            format: r.get(3)?,
            source_path: r.get(4)?,
        })
    })?;
    rows.collect()
}

pub fn get_chapter(
    conn: &Connection,
    book_id: i64,
    idx: i64,
) -> rusqlite::Result<Option<(String, String)>> {
    conn.query_row(
        "SELECT title, original_text FROM chapters WHERE book_id = ?1 AND idx = ?2",
        params![book_id, idx],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

pub fn count_chapters(conn: &Connection, book_id: i64) -> rusqlite::Result<i64> {
    conn.query_row(
        "SELECT COUNT(*) FROM chapters WHERE book_id = ?1",
        params![book_id],
        |r| r.get(0),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db_with_book() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::apply_pragmas(&conn).unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn
    }

    fn fixture_chapters() -> Vec<NewChapter> {
        (1..=3)
            .map(|i| NewChapter {
                idx: i,
                title: format!("第{i}章"),
                text: format!("第{i}章正文。"),
                token_est: 10,
            })
            .collect()
    }

    #[test]
    fn create_and_list_books() {
        let conn = db_with_book();
        let id = create_book_with_chapters(
            &conn, "武动乾坤", Some("天蚕土豆"), "txt", "/tmp/wu.txt", &fixture_chapters(),
        )
        .unwrap();
        assert!(id > 0);
        let books = list_books(&conn).unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0].title, "武动乾坤");
        assert_eq!(count_chapters(&conn, id).unwrap(), 3);
    }

    #[test]
    fn batch_insert_is_atomic() {
        let conn = db_with_book();
        let mut bad = fixture_chapters();
        bad.push(NewChapter { idx: 1, title: "重复idx触发UNIQUE失败".into(), text: "x".into(), token_est: 1 });
        let err = create_book_with_chapters(&conn, "坏书", None, "txt", "/tmp/bad.txt", &bad);
        assert!(err.is_err());
        // 回滚干净：书架为空
        assert!(list_books(&conn).unwrap().is_empty());
    }

    #[test]
    fn get_chapter_hit_and_miss() {
        let conn = db_with_book();
        let id = create_book_with_chapters(&conn, "书", None, "txt", "/x", &fixture_chapters()).unwrap();
        let (title, text) = get_chapter(&conn, id, 2).unwrap().unwrap();
        assert_eq!(title, "第2章");
        assert!(text.contains("第2章正文"));
        assert!(get_chapter(&conn, id, 99).unwrap().is_none());
    }
}
