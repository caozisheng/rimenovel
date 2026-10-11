// 书架/设置 UI 支撑 commands（dev-plan Task 4.5 / 3.5 UI 侧）

use crate::commands::AppState;
use rusqlite::Connection;
use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct ExtractionStatus {
    pub book_id: i64,
    pub job_id: i64,
    pub kind: String,
    pub chapter_idx: Option<i64>,
    pub error: Option<String>,
    pub phase: Option<String>,
    pub attempt: Option<u32>,
    pub retry_after_ms: Option<u64>,
    pub state: String, // queued|running|paused|done|failed|cancelled|none
    pub progress: f64,
    pub coverage: Option<crate::core::kg::GraphCoverage>,
}

/// Per-book background status: unfinished work first, otherwise the latest result.
pub fn extraction_statuses(conn: &Connection) -> Result<Vec<ExtractionStatus>, String> {
    let mut stmt = conn.prepare(
        "SELECT book_id, state, progress, id, kind,
                json_extract(payload_json, '$.chapter_idx'), error,
                json_extract(payload_json, '$.phase'),
                json_extract(payload_json, '$.attempt'),
                json_extract(payload_json, '$.retry_after_ms')
         FROM (
             SELECT *, ROW_NUMBER() OVER (
                 PARTITION BY book_id
                 ORDER BY CASE state WHEN 'running' THEN 0 WHEN 'queued' THEN 1 WHEN 'paused' THEN 2 ELSE 3 END, id DESC
             ) AS priority
             FROM jobs WHERE kind IN ('global_extract','chapter_extract','generate','reconcile')
         ) WHERE priority = 1",
    ).map_err(|e| e.to_string())?;
    let rows = stmt.query_map([], |r| {
        Ok(ExtractionStatus {
            book_id: r.get(0)?,
            state: r.get(1)?,
            progress: r.get(2)?,
            job_id: r.get(3)?,
            kind: r.get(4)?,
            chapter_idx: r.get(5)?,
            error: r.get(6)?,
            phase: r.get(7)?,
            attempt: r.get(8)?,
            retry_after_ms: r.get(9)?,
            coverage: None,
        })
    }).map_err(|e| e.to_string())?;
    let mut statuses = rows.collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
    for status in &mut statuses {
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE id=?1)", [status.book_id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if exists { status.coverage = Some(crate::store::kg::coverage(conn, status.book_id)?); }
    }
    Ok(statuses)
}

#[tauri::command]
pub fn get_extraction_statuses(
    state: tauri::State<AppState>,
) -> Result<Vec<ExtractionStatus>, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    extraction_statuses(conn)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn completed_sample_status_does_not_claim_full_book_coverage() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        let chapters: Vec<_> = (1..=100).map(|idx| crate::store::books::NewChapter { idx, title: format!("Chapter {idx}"), text: format!("Prose {idx}"), token_est: 10 }).collect();
        let book = crate::store::books::create_book_with_chapters(&conn, "Sampled", None, "txt", "test", &chapters).unwrap();
        let config = crate::core::kg::RuntimeConfig { provider_id: 1, model_extract: "test".into(), context_limit: 32768, output_reserve: 4096, prompt_version: "kg-lite/v1".into() };
        crate::store::kg::set_extraction_plan(&conn, book, &[1, 100], 2, 300).unwrap();
        for idx in [1, 100] {
            let text = crate::store::kg::text_version(&conn, book, idx, "original", None).unwrap();
            crate::store::kg::commit_snapshot(&conn, &text, &crate::core::kg::ChapterSnapshot { summary: format!("Sample {idx}"), ..Default::default() }, "test", 0, None, &config).unwrap();
        }
        let id = crate::jobs::enqueue(&conn, book, crate::jobs::JobKind::GlobalExtract, serde_json::json!({"graph_format":"kg-lite/v1"})).unwrap();
        crate::jobs::mark_done(&conn, id).unwrap();
        let status = extraction_statuses(&conn).unwrap().remove(0);
        assert_eq!(status.state, "done");
        let coverage = status.coverage.unwrap();
        assert_eq!(coverage.mode, "sampled");
        assert!(coverage.target_complete);
        assert_eq!(coverage.total_chapters, 100);
        assert_eq!(coverage.completed_chapters, vec![1, 100]);
        assert_eq!(coverage.missing_chapters.len(), 98);
        assert_eq!(coverage.calls_used, 2);
    }

    #[test]
    fn blocked_successor_surfaces_predecessor_error_without_active_orphan() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        let predecessor = crate::jobs::enqueue(&conn, 1, crate::jobs::JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap();
        let successor = crate::jobs::enqueue(&conn, 1, crate::jobs::JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3,"provider_id":9})).unwrap();
        crate::jobs::mark_failed(&conn, predecessor, "HTTP 504: gateway timeout").unwrap();
        crate::jobs::mark_blocked(&conn, successor, 2, "HTTP 504: gateway timeout").unwrap();
        let status = extraction_statuses(&conn).unwrap().remove(0);
        assert_eq!(status.job_id, successor);
        assert_eq!(status.state, "failed");
        assert_eq!(status.phase.as_deref(), Some("failed"));
        assert_eq!(status.chapter_idx, Some(3));
        assert!(status.error.as_deref().unwrap().contains("第2章"));
        assert!(status.error.as_deref().unwrap().contains("504"));
        let active: i64 = conn.query_row("SELECT COUNT(*) FROM jobs WHERE state IN ('queued','running','paused')", [], |row| row.get(0)).unwrap();
        assert_eq!(active, 0);
        let retry = crate::jobs::enqueue(&conn, 1, crate::jobs::JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        assert_eq!(extraction_statuses(&conn).unwrap().remove(0).job_id, retry);
    }

    #[test]
    fn cancelled_checkpoint_and_failed_error_survive_database_reopening() {
        let unique = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("rimenovel-job-status-{}-{unique}.sqlite", std::process::id()));
        let cancelled;
        let before = {
            let conn = Connection::open(&path).unwrap();
            crate::store::Db::new(&conn).migrate().unwrap();
            cancelled = crate::jobs::enqueue(&conn, 1, crate::jobs::JobKind::GlobalExtract,
                serde_json::json!({"provider_id":8,"committed_chapters":[1],"committed_revisions":{"1":{"revision":3}}})).unwrap();
            crate::jobs::mark_running(&conn, cancelled).unwrap();
            crate::jobs::set_progress(&conn, cancelled, 0.25, &serde_json::json!({"chapter_idx":2})).unwrap();
            crate::jobs::set_phase(&conn, cancelled, "retry_wait", Some(2), Some(1000)).unwrap();
            crate::jobs::cancel_book(&conn, 1).unwrap();
            let failed = crate::jobs::enqueue(&conn, 2, crate::jobs::JobKind::ChapterExtract,
                serde_json::json!({"chapter_idx":4})).unwrap();
            crate::jobs::mark_failed(&conn, failed, "HTTP 504: gateway timeout; request-id=req-7").unwrap();
            let mut statuses = extraction_statuses(&conn).unwrap();
            statuses.sort_by_key(|status| status.book_id);
            serde_json::to_value(statuses).unwrap()
        };
        {
            let conn = Connection::open(&path).unwrap();
            assert_eq!(crate::jobs::recover_orphans(&conn).unwrap(), 0);
            let mut statuses = extraction_statuses(&conn).unwrap();
            statuses.sort_by_key(|status| status.book_id);
            assert_eq!(serde_json::to_value(statuses).unwrap(), before);
            assert_eq!(before[0]["state"], "cancelled");
            assert_eq!(before[0]["progress"], 0.25);
            assert_eq!(before[0]["phase"], "cancelled");
            assert_eq!(before[1]["error"], "HTTP 504: gateway timeout; request-id=req-7");
            let raw: String = conn.query_row("SELECT payload_json FROM jobs WHERE id=?1", [cancelled], |row| row.get(0)).unwrap();
            let payload: serde_json::Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(payload["committed_chapters"], serde_json::json!([1]));
            assert_eq!(payload["committed_revisions"]["1"]["revision"], 3);
            assert_eq!(payload["provider_id"], 8);
        }
        std::fs::remove_file(path).unwrap();
    }

    #[test]
    fn retry_and_cancellation_metadata_survive_status_reload() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        let id = crate::jobs::enqueue(&conn, 1, crate::jobs::JobKind::GlobalExtract,
            serde_json::json!({"chapter_idx":2,"committed_chapters":[1],"provider_id":9})).unwrap();
        crate::jobs::set_phase(&conn, id, "retry_wait", Some(2), Some(900)).unwrap();
        let status = extraction_statuses(&conn).unwrap().remove(0);
        assert_eq!(status.phase.as_deref(), Some("retry_wait"));
        assert_eq!(status.attempt, Some(2));
        assert_eq!(status.retry_after_ms, Some(900));
        assert_eq!(status.chapter_idx, Some(2));
        crate::jobs::cancel_book(&conn, 1).unwrap();
        crate::jobs::recover_orphans(&conn).unwrap();
        let data = serde_json::to_value(extraction_statuses(&conn).unwrap().remove(0)).unwrap();
        assert_eq!(data["state"], "cancelled");
        assert_eq!(data["phase"], "cancelled");
        assert_eq!(data["attempt"], 2);
        assert!(data["retry_after_ms"].is_null());
        assert_eq!(data["chapter_idx"], 2);
    }

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

    #[test]
    fn failed_chapter_task_remains_visible_after_reopening() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn.execute("INSERT INTO jobs(book_id,kind,state,progress,payload_json,error) VALUES(1,'chapter_extract','failed',0,'{\"chapter_idx\":1}','HTTP 504: gateway timeout')", []).unwrap();
        let statuses = extraction_statuses(&conn).unwrap();
        let status = statuses.iter().find(|status| status.book_id == 1)
            .expect("failed chapter extraction must be visible to the workbench");
        let data = serde_json::to_value(status).unwrap();
        assert_eq!(data["state"], "failed");
        assert_eq!(data["error"], "HTTP 504: gateway timeout");
        assert_eq!(data["kind"], "chapter_extract");
        assert_eq!(data["chapter_idx"], 1);
        assert_eq!(data["job_id"], conn.last_insert_rowid());
    }

    #[test]
    fn unfinished_book_job_is_not_hidden_by_completed_foreground_chapter() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn.execute("INSERT INTO jobs(book_id,kind,state,progress) VALUES(1,'global_extract','running',0.25)", []).unwrap();
        let running = conn.last_insert_rowid();
        conn.execute("INSERT INTO jobs(book_id,kind,state,progress) VALUES(1,'chapter_extract','done',1)", []).unwrap();
        let data = serde_json::to_value(&extraction_statuses(&conn).unwrap()[0]).unwrap();
        assert_eq!(data["state"], "running");
        assert_eq!(data["progress"], 0.25);
        assert_eq!(data["job_id"], running);
    }

    #[test]
    fn running_extraction_is_visible_even_when_prefetch_successor_is_queued() {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn.execute("INSERT INTO jobs(book_id,kind,state,progress,payload_json) VALUES(1,'global_extract','running',0.25,'{\"phase\":\"chapter\",\"attempt\":1}')", []).unwrap();
        conn.execute("INSERT INTO jobs(book_id,kind,state,progress,payload_json) VALUES(1,'chapter_extract','queued',0,'{\"phase\":\"queued\",\"chapter_idx\":2}')", []).unwrap();
        let statuses = extraction_statuses(&conn).unwrap();
        let status = &statuses[0];
        assert_eq!(status.state, "running");
        assert_eq!(status.phase.as_deref(), Some("chapter"));
        assert_eq!(status.attempt, Some(1));
    }
}
