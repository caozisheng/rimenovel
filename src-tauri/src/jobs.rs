// 任务队列/状态机（dev-plan Task 4.3 后半）
// queued→running→(paused|done|failed|cancelled); payload 记录断点(已完成窗口号);
// 启动时遗留 running 归位 queued。

use rusqlite::{params, Connection};

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
    // 每书同 kind 最多 1 个活跃 job（幂等入队）
    let active: i64 = conn.query_row(
        "SELECT COUNT(*) FROM jobs WHERE book_id=?1 AND kind=?2 AND state IN ('queued','running','paused')",
        params![book_id, kind.as_str()],
        |r| r.get(0),
    )?;
    if active > 0 {
        return conn.query_row(
            "SELECT id FROM jobs WHERE book_id=?1 AND kind=?2 AND state IN ('queued','running','paused') LIMIT 1",
            params![book_id, kind.as_str()],
            |r| r.get(0),
        );
    }
    conn.execute(
        "INSERT INTO jobs (book_id, kind, state, progress, payload_json) VALUES (?1, ?2, 'queued', 0, ?3)",
        params![book_id, kind.as_str(), payload.to_string()],
    )?;
    Ok(conn.last_insert_rowid())
}

pub fn mark_running(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='running', updated_at=datetime('now') WHERE id=?1",
        [job_id],
    )?;
    Ok(())
}

pub fn mark_done(conn: &Connection, job_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='done', progress=1.0, updated_at=datetime('now') WHERE id=?1",
        [job_id],
    )?;
    Ok(())
}

pub fn mark_failed(conn: &Connection, job_id: i64, error: &str) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='failed', error=?2, updated_at=datetime('now') WHERE id=?1",
        params![job_id, error],
    )?;
    Ok(())
}

/// 暂停（断点信息存 payload, 例如 {"done_windows":[1,2,3]}）
pub fn mark_paused(
    conn: &Connection,
    job_id: i64,
    payload: &serde_json::Value,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE jobs SET state='paused', payload_json=?2, updated_at=datetime('now') WHERE id=?1",
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
        "UPDATE jobs SET progress=?2, payload_json=?3, updated_at=datetime('now') WHERE id=?1",
        params![job_id, progress, payload.to_string()],
    )?;
    Ok(())
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
