// 任务队列/状态机（dev-plan Task 4.3 后半）
// queued→running→(paused|done|failed|cancelled); payload 记录断点(已完成窗口号);
// 启动时遗留 running 归位 queued。

use rusqlite::{params, Connection, OptionalExtension};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobState {
    Queued,
    Running,
    Paused,
    Done,
    Failed,
    Cancelled,
}

impl JobState {
    pub fn as_str(self) -> &'static str {
        match self {
            JobState::Queued => "queued",
            JobState::Running => "running",
            JobState::Paused => "paused",
            JobState::Done => "done",
            JobState::Failed => "failed",
            JobState::Cancelled => "cancelled",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    GlobalExtract,
    ChapterExtract,
    Generate,
    Reconcile,
}

impl JobKind {
    pub fn as_str(self) -> &'static str {
        match self {
            JobKind::GlobalExtract => "global_extract",
            JobKind::ChapterExtract => "chapter_extract",
            JobKind::Generate => "generate",
            JobKind::Reconcile => "reconcile",
        }
    }
}

#[derive(Debug, Clone)]
pub struct Job {
    pub id: i64,
    pub book_id: Option<i64>,
    pub kind: JobKind,
    pub state: JobState,
    pub progress: f64,
    pub payload: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub fn enqueue(
    conn: &Connection,
    book_id: i64,
    kind: JobKind,
    payload: serde_json::Value,
) -> rusqlite::Result<i64> {
    // Non-global work is idempotent per chapter; an older active chapter cannot
    // swallow a newer reading request after recovery.
    let chapter = if kind == JobKind::GlobalExtract { None } else { payload.get("chapter_idx").and_then(serde_json::Value::as_i64) };
    let existing = conn.query_row(
        "SELECT id FROM jobs WHERE book_id=?1 AND kind=?2 AND state IN ('queued','running','paused')
         AND (?3 IS NULL OR json_extract(payload_json,'$.chapter_idx')=?3) ORDER BY id LIMIT 1",
        params![book_id, kind.as_str(), chapter], |r| r.get(0),
    );
    match existing {
        Ok(id) => return Ok(id),
        Err(rusqlite::Error::QueryReturnedNoRows) => {}
        Err(error) => return Err(error),
    }
    conn.execute(
        "INSERT INTO jobs (book_id, kind, state, progress, payload_json) VALUES (?1, ?2, 'queued', 0, ?3)",
        params![book_id, kind.as_str(), payload.to_string()],
    )?;
    Ok(conn.last_insert_rowid())
}

/// Returns whether automatic extraction/generation may enqueue work for this book.
/// A failed extraction/generation remains a durable gate until a later successful
/// explicit job for the same book. Rows without origin metadata are intentionally
/// treated as failures requiring explicit recovery (historical conservative policy).
fn blocking_failure(conn: &Connection, book_id: i64) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT COALESCE(failed.error,'自动预取任务失败')
         FROM jobs failed
         WHERE failed.book_id=?1
           AND failed.state='failed'
           AND failed.kind IN ('global_extract','chapter_extract','generate')
           AND NOT EXISTS(
             SELECT 1 FROM jobs recovery
             WHERE recovery.book_id=failed.book_id
               AND recovery.id>failed.id
               AND recovery.state='done'
               AND json_extract(recovery.payload_json,'$.origin')='explicit'
           )
         ORDER BY failed.id DESC LIMIT 1",
        [book_id],
        |row| row.get(0),
    ).optional()
}

pub fn prefetch_blocked(conn: &Connection, book_id: i64) -> rusqlite::Result<bool> {
    Ok(blocking_failure(conn, book_id)?.is_some())
}

/// Reject a queued automatic job again immediately before provider work. This
/// closes the race where it was enqueued before a predecessor failed.
pub fn ensure_prefetch_allowed(conn: &Connection, job_id: i64) -> Result<(), String> {
    let Some((book_id, kind, payload)) = conn.query_row(
        "SELECT book_id,kind,payload_json FROM jobs WHERE id=?1",
        [job_id],
        |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, String>(1)?, row.get::<_, Option<String>>(2)?)),
    ).optional().map_err(|e| e.to_string())? else { return Ok(()); };
    if !matches!(kind.as_str(), "global_extract" | "chapter_extract" | "generate") {
        return Ok(());
    }
    let origin = payload.and_then(|raw| serde_json::from_str::<serde_json::Value>(&raw).ok())
        .and_then(|value| value.get("origin").and_then(serde_json::Value::as_str).map(str::to_owned));
    if origin.as_deref() == Some("explicit") { return Ok(()); }
    let Some(book_id) = book_id else { return Ok(()); };
    if let Some(error) = blocking_failure(conn, book_id).map_err(|e| e.to_string())? {
        return Err(format!("自动预取已阻止：{error}"));
    }
    Ok(())
}

/// Enqueue only automatic background work. A blocked book is left untouched,
/// including any terminal error/checkpoint rows already visible to the user.
pub fn enqueue_prefetch(
    conn: &Connection,
    book_id: i64,
    kind: JobKind,
    mut payload: serde_json::Value,
) -> rusqlite::Result<Option<i64>> {
    if prefetch_blocked(conn, book_id)? {
        return Ok(None);
    }
    if let Some(object) = payload.as_object_mut() {
        object.insert("origin".into(), serde_json::Value::String("prefetch".into()));
    }
    Ok(Some(enqueue(conn, book_id, kind, payload)?))
}

/// An explicit request may join a queued background job. Promote that shared
/// active row so its successful completion is a durable recovery checkpoint.
pub fn mark_explicit(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET payload_json=json_set(COALESCE(payload_json,'{}'),'$.origin','explicit'), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        [job_id],
    )?;
    Ok(())
}

pub fn mark_running(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='running', updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        [job_id],
    )?;
    Ok(())
}

pub fn mark_done(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='done', progress=1.0, payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase','done','$.retry_after_ms',NULL), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        [job_id],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, job_id: i64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='failed', error=?2, payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase','failed','$.retry_after_ms',NULL), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        params![job_id, error],
    )?;
    Ok(())
}

/// A failed dependency is not resumable without user action. Keep its successor's
/// checkpoint but expose the cause instead of leaving an apparently active pause.
pub fn mark_blocked(conn: &Connection, job_id: i64, predecessor_chapter: i64, error: &str) -> rusqlite::Result<()> {
    mark_failed(conn, job_id, &format!("前置第{predecessor_chapter}章失败，后续预取未执行：{error}"))
}

/// 暂停（断点信息存 payload, 例如 {"done_windows":[1,2,3]}）
pub fn mark_paused(
    conn: &Connection,
    job_id: i64,
    payload: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='paused', payload_json=json_patch(COALESCE(payload_json,'{}'),?2), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        params![job_id, payload.to_string()],
    )?;
    Ok(())
}

pub fn set_progress(
    conn: &Connection,
    job_id: i64,
    progress: f64,
    payload: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET progress=?2, payload_json=json_patch(COALESCE(payload_json,'{}'),?3), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        params![job_id, progress, payload.to_string()],
    )?;
    Ok(())
}

/// Request metadata is patched independently from chapter/window checkpoints.
pub fn set_phase(conn: &Connection, job_id: i64, phase: &str, attempt: Option<u32>, retry_after_ms: Option<u64>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase',?2,'$.attempt',?3,'$.retry_after_ms',?4), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        params![job_id, phase, attempt, retry_after_ms],
    )?;
    Ok(())
}

pub fn mark_cancelled(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='cancelled', payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase','cancelled','$.retry_after_ms',NULL), updated_at=datetime('now') WHERE id=?1 AND state IN ('queued','running','paused')",
        [job_id],
    )?;
    Ok(())
}

/// Cancel all unfinished work, including jobs not yet claimed by a worker.
pub fn cancel_book(conn: &Connection, book_id: i64) -> rusqlite::Result<usize> {
    conn.execute(
        "UPDATE jobs SET state='cancelled', payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase','cancelled','$.retry_after_ms',NULL), updated_at=datetime('now') WHERE book_id=?1 AND state IN ('queued','running','paused')",
        [book_id],
    )
}

pub fn is_active(conn: &Connection, job_id: i64) -> rusqlite::Result<bool> {
    conn.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE id=?1 AND state IN ('queued','running','paused'))", [job_id], |row| row.get(0))
}

/// app 启动恢复: 遗留 running/queued 保持, paused 保持断点（由调度器续跑）
pub fn recover_orphans(conn: &Connection) -> rusqlite::Result<usize> {
    // running 状态在进程重启后不可能仍有效——回 queued 从断点续跑
    let n = conn.execute(
        "UPDATE jobs SET state='queued', updated_at=datetime('now') WHERE state='running'",
        [],
    )?;
    Ok(n)
}

// （job 读取走各 command 的具体查询; 通用 get 待有真实调用方再补）

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::apply_pragmas(&conn).unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn
    }

    fn snapshot(conn: &Connection, id: i64) -> (String, f64, serde_json::Value, Option<String>) {
        conn.query_row("SELECT state,progress,payload_json,error FROM jobs WHERE id=?1", [id], |row| {
            let payload: String = row.get(2)?;
            Ok((row.get(0)?, row.get(1)?, serde_json::from_str(&payload).unwrap(), row.get(3)?))
        }).unwrap()
    }

    #[test]
    fn blocked_prefetch_preserves_checkpoint_and_allows_explicit_retry() {
        let conn = db();
        let payload = serde_json::json!({"chapter_idx":3,"provider_id":8,"committed_revisions":{"2":{"revision":4}}});
        let id = enqueue(&conn, 1, JobKind::Generate, payload.clone()).unwrap();
        mark_blocked(&conn, id, 2, "HTTP 504: gateway timeout").unwrap();
        let failed = snapshot(&conn, id);
        assert_eq!(failed.0, "failed");
        assert_eq!(failed.2["committed_revisions"], payload["committed_revisions"]);
        assert_eq!(failed.2["provider_id"], 8);
        assert_eq!(failed.2["chapter_idx"], 3);
        assert_eq!(failed.2["phase"], "failed");
        assert!(failed.3.unwrap().contains("第2章"));
        assert_eq!(recover_orphans(&conn).unwrap(), 0);
        assert!(!is_active(&conn, id).unwrap());
        assert_ne!(enqueue(&conn, 1, JobKind::Generate, payload).unwrap(), id);
    }

    #[test]
    fn cancellation_cannot_be_overwritten_by_blocked_prefetch_error() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::ChapterExtract, serde_json::json!({"chapter_idx":3})).unwrap();
        cancel_book(&conn, 1).unwrap();
        let before = snapshot(&conn, id);
        mark_blocked(&conn, id, 2, "cancelled predecessor").unwrap();
        assert_eq!(snapshot(&conn, id), before);
    }

    #[test]
    fn cancellation_is_terminal_and_preserves_completed_checkpoint() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::GlobalExtract,
            serde_json::json!({"provider_id":7,"committed_chapters":[1]})).unwrap();
        mark_running(&conn, id).unwrap();
        set_progress(&conn, id, 0.25, &serde_json::json!({"committed_chapters":[1]})).unwrap();
        assert_eq!(cancel_book(&conn, 1).unwrap(), 1);
        let cancelled = snapshot(&conn, id);
        assert_eq!(cancelled.0, "cancelled");
        assert_eq!(cancelled.1, 0.25);
        assert_eq!(cancelled.2["provider_id"], 7);
        assert_eq!(cancelled.2["committed_chapters"], serde_json::json!([1]));
        assert_eq!(cancelled.2["phase"], "cancelled");
        mark_running(&conn, id).unwrap();
        mark_done(&conn, id).unwrap();
        mark_failed(&conn, id, "late provider failure").unwrap();
        mark_paused(&conn, id, &serde_json::json!({"committed_chapters":[1,2]})).unwrap();
        set_progress(&conn, id, 1.0, &serde_json::json!({"committed_chapters":[1,2]})).unwrap();
        set_phase(&conn, id, "request", Some(2), None).unwrap();
        assert_eq!(recover_orphans(&conn).unwrap(), 0);
        assert_eq!(snapshot(&conn, id), cancelled);
        assert_ne!(enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap(), id);
    }

    #[test]
    fn retry_metadata_merges_without_losing_checkpoint_or_provider() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::GlobalExtract,
            serde_json::json!({"provider_id":4,"committed_chapters":[1,2],"chapter_idx":3})).unwrap();
        set_phase(&conn, id, "retry_wait", Some(2), Some(1200)).unwrap();
        let payload = snapshot(&conn, id).2;
        assert_eq!(payload["committed_chapters"], serde_json::json!([1,2]));
        assert_eq!(payload["provider_id"], 4);
        assert_eq!(payload["chapter_idx"], 3);
        assert_eq!(payload["attempt"], 2);
        assert_eq!(payload["retry_after_ms"], 1200);
        set_progress(&conn, id, 0.75, &serde_json::json!({"committed_chapters":[1,2,3]})).unwrap();
        assert_eq!(snapshot(&conn, id).2["phase"], "retry_wait");
        set_phase(&conn, id, "request", Some(3), None).unwrap();
        assert!(snapshot(&conn, id).2["retry_after_ms"].is_null());
        mark_failed(&conn, id, "HTTP 504: gateway timeout").unwrap();
        assert_eq!(snapshot(&conn, id).3.as_deref(), Some("HTTP 504: gateway timeout"));
        set_phase(&conn, id, "request", Some(1), None).unwrap();
        assert_eq!(snapshot(&conn, id).2["phase"], "failed");
    }

    #[test]
    fn cancel_book_includes_queued_running_paused_only_for_that_book() {
        let conn = db();
        let queued = enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        let running = enqueue(&conn, 1, JobKind::ChapterExtract, serde_json::json!({"chapter_idx":1})).unwrap();
        mark_running(&conn, running).unwrap();
        let paused = enqueue(&conn, 1, JobKind::Generate, serde_json::json!({"chapter_idx":2})).unwrap();
        mark_paused(&conn, paused, &serde_json::json!({"chapter_idx":2})).unwrap();
        let other = enqueue(&conn, 2, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        let done = enqueue(&conn, 1, JobKind::Reconcile, serde_json::json!({"chapter_idx":1})).unwrap();
        mark_done(&conn, done).unwrap();
        assert_eq!(cancel_book(&conn, 1).unwrap(), 3);
        for id in [queued, running, paused] { assert_eq!(snapshot(&conn, id).0, "cancelled"); }
        assert_eq!(snapshot(&conn, other).0, "queued");
        assert_eq!(snapshot(&conn, done).0, "done");
    }

    #[test]
    fn blocked_prefetch_terminalizes_with_checkpoint_and_predecessor_error() {
        let conn = db();
        let auto = enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3})).unwrap().unwrap();
        set_progress(&conn, auto, 0.4, &serde_json::json!({"committed_windows":[1]})).unwrap();
        let predecessor = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap();
        mark_failed(&conn, predecessor, "HTTP 504: gateway timeout").unwrap();
        let error = ensure_prefetch_allowed(&conn, auto).unwrap_err();
        mark_failed(&conn, auto, &error).unwrap();
        let snapshot = snapshot(&conn, auto);
        assert_eq!(snapshot.0, "failed");
        assert_eq!(snapshot.2["committed_windows"], serde_json::json!([1]));
        assert!(snapshot.3.unwrap().contains("HTTP 504"));
    }

    #[test]
    fn queued_prefetch_is_blocked_again_after_predecessor_failure_and_restart() {
        let conn = db();
        let auto = enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3})).unwrap().unwrap();
        let predecessor = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2,"origin":"explicit"})).unwrap();
        mark_failed(&conn, predecessor, "HTTP 504: gateway timeout").unwrap();
        let error = ensure_prefetch_allowed(&conn, auto).unwrap_err();
        assert!(error.contains("HTTP 504"));
        mark_running(&conn, auto).unwrap();
        recover_orphans(&conn).unwrap();
        assert!(ensure_prefetch_allowed(&conn, auto).unwrap_err().contains("HTTP 504"));
    }

    #[test]
    fn explicit_job_bypasses_prefetch_gate_without_clearing_failure() {
        let conn = db();
        let failed = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap();
        mark_failed(&conn, failed, "upstream failure").unwrap();
        let explicit = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3,"origin":"explicit"})).unwrap();
        assert!(ensure_prefetch_allowed(&conn, explicit).is_ok());
        assert!(prefetch_blocked(&conn, 1).unwrap());
    }

    #[test]
    fn prefetch_failed_book_gate_survives_reopen_and_restart_without_new_rows() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2,"provider_id":8})).unwrap();
        mark_running(&conn, id).unwrap();
        set_progress(&conn, id, 0.5, &serde_json::json!({"committed_windows":[1,2]})).unwrap();
        mark_failed(&conn, id, "HTTP 504: gateway timeout").unwrap();
        let before = snapshot(&conn, id);
        assert_eq!(enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap(), None);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM jobs WHERE book_id=1", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
        assert_eq!(snapshot(&conn, id), before);
        recover_orphans(&conn).unwrap();
        assert_eq!(enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap(), None);
        assert_eq!(conn.query_row("SELECT COUNT(*) FROM jobs WHERE book_id=1", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    }

    #[test]
    fn explicit_successful_recovery_reopens_prefetch_gate() {
        let conn = db();
        let failed = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2})).unwrap();
        mark_failed(&conn, failed, "upstream timeout").unwrap();
        assert_eq!(enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3})).unwrap(), None);
        let recovered = enqueue(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":2,"origin":"explicit"})).unwrap();
        mark_done(&conn, recovered).unwrap();
        let next = enqueue_prefetch(&conn, 1, JobKind::ChapterExtract,
            serde_json::json!({"chapter_idx":3})).unwrap();
        assert!(next.is_some());
        assert_ne!(next.unwrap(), failed);
    }

    #[test]
    fn prefetch_failure_gate_isolated_per_book_and_blocks_legacy_rows() {
        let conn = db();
        let legacy = enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        mark_failed(&conn, legacy, "legacy extraction error").unwrap();
        assert_eq!(enqueue_prefetch(&conn, 1, JobKind::GlobalExtract,
            serde_json::json!({})).unwrap(), None);
        let other = enqueue_prefetch(&conn, 2, JobKind::GlobalExtract,
            serde_json::json!({})).unwrap();
        assert!(other.is_some());
    }

    #[test]
    fn enqueue_is_idempotent_per_book_kind() {
        let conn = db();
        let a = enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        let b = enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        assert_eq!(a, b, "同书同kind重复入队返回既有job");
        let c = enqueue(&conn, 2, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        assert_ne!(a, c);
        let d = enqueue(&conn, 1, JobKind::ChapterExtract, serde_json::json!({})).unwrap();
        assert_ne!(a, d);
    }

    #[test]
    fn lifecycle_transitions() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::Generate, serde_json::json!({})).unwrap();
        mark_running(&conn, id).unwrap();
        set_progress(&conn, id, 0.5, &serde_json::json!({"done_windows":[1,2]})).unwrap();
        mark_paused(&conn, id, &serde_json::json!({"done_windows":[1,2,3]})).unwrap();
        mark_running(&conn, id).unwrap();
        mark_done(&conn, id).unwrap();
        let state: String = conn
            .query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "done");
    }

    #[test]
    fn orphan_running_recovered_to_queued() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::GlobalExtract, serde_json::json!({})).unwrap();
        mark_running(&conn, id).unwrap();
        assert_eq!(recover_orphans(&conn).unwrap(), 1);
        let state: String = conn
            .query_row("SELECT state FROM jobs WHERE id=?1", [id], |r| r.get(0))
            .unwrap();
        assert_eq!(state, "queued");
    }

    #[test]
    fn failed_records_error() {
        let conn = db();
        let id = enqueue(&conn, 1, JobKind::Reconcile, serde_json::json!({})).unwrap();
        mark_running(&conn, id).unwrap();
        mark_failed(&conn, id, "LLM 429").unwrap();
        let (state, err): (String, String) = conn
            .query_row("SELECT state, error FROM jobs WHERE id=?1", [id], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .unwrap();
        assert_eq!(state, "failed");
        assert!(err.contains("429"));
    }
}
