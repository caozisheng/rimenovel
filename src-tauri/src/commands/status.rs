// 书架/设置 UI 支撑 commands（dev-plan Task 4.5 / 3.5 UI 侧）

use crate::commands::AppState;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ExtractionStatus {
    pub book_id: i64,
    pub state: String, // queued|running|paused|done|failed|cancelled|none
    pub progress: f64,
}

/// 全部书的 global_extract job 状态（书架进度条数据源）
pub fn extraction_statuses(conn: &Connection) -> rusqlite::Result<Vec<ExtractionStatus>> {
    let mut stmt = conn.prepare(
        "SELECT book_id, state, progress FROM jobs
         WHERE kind = 'global_extract'
         AND id IN (SELECT MAX(id) FROM jobs WHERE kind='global_extract' GROUP BY book_id)",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(ExtractionStatus {
            book_id: r.get(0)?,
            state: r.get(1)?,
            progress: r.get(2)?,
        })
    })?;
    rows.collect()
}

#[tauri::command]
pub fn get_extraction_statuses(
    state: tauri::State<AppState>,
) -> Result<Vec<ExtractionStatus>, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    extraction_statuses(conn).map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn latest_job_per_book_returned() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::apply_pragmas(&conn).unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        // 书1: 两个历史 job（旧 failed + 新 running）; 书2: 一个 done
        for (book, st, pr) in [(1, "failed", 0.2), (1, "running", 0.6), (2, "done", 1.0)] {
            conn.execute(
                "INSERT INTO jobs (book_id, kind, state, progress) VALUES (?1, 'global_extract', ?2, ?3)",
                rusqlite::params![book, st, pr],
            )
            .unwrap();
        }
        let v = extraction_statuses(&conn).unwrap();
        assert_eq!(v.len(), 2);
        let b1 = v.iter().find(|s| s.book_id == 1).unwrap();
        assert_eq!(b1.state, "running");
        assert!((b1.progress - 0.6).abs() < 1e-9);
    }
}
