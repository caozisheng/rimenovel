//! Real KG IPC and book-scoped execution. SQLite locks never span provider awaits.
use super::{providers, AppState};
use crate::core::kg::{EditInput, EditRecord, GraphQuery, GraphView, RuntimeConfig, TextVersion};
use crate::llm::{ProviderConfig, TaskKind};
use crate::llm::policy::{CallControl, CallObservation};
use tokio_util::sync::CancellationToken;
use crate::{jobs, store::kg as repo};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};
use std::future::Future;
use tauri::{Emitter, Manager};

static BOOK_GATES: LazyLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static RUNNING_JOBS: LazyLock<Mutex<HashMap<i64, ActiveJob>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
// Bounded cross-book work. The lock order is book gate, then global permit.
pub(crate) static PROVIDER_LIMIT: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(2));
const BOOK_CALL_LIMIT: usize = 45;

pub(crate) fn epoch_ms() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default().as_millis().min(u64::MAX as u128) as u64
}

fn book_elapsed(payload: &Value, persisted: u64, now: u64) -> u64 {
    let started = payload.get("budget_started_ms").and_then(Value::as_u64).unwrap_or(now);
    let base = payload.get("budget_elapsed_base_ms").and_then(Value::as_u64).unwrap_or(persisted);
    persisted.max(base.saturating_add(now.saturating_sub(started)))
}

fn book_remaining(_elapsed: u64) -> Result<std::time::Duration, String> {
    // Elapsed time is recorded for performance reporting only. It is not a
    // book-level timeout; each provider request keeps its own bounded timeout.
    Ok(std::time::Duration::from_secs(365 * 24 * 60 * 60))
}

fn selected_remaining(original: bool, _elapsed: u64) -> Result<std::time::Duration, String> {
    if original { Ok(crate::core::kg_extract::REQUEST_TIMEOUT) } else { book_remaining(0) }
}

// Whole chapters only: endpoints get reserved first, then current/next, then
// deterministic sample order. Zero-cost cached samples are always retained.
fn trim_plan(costs: &[(i64, usize)], current: Option<i64>, calls: usize) -> Vec<i64> {
    let mut order = Vec::with_capacity(costs.len());
    if let Some((idx, _)) = costs.first() { order.push(*idx); }
    if let Some((idx, _)) = costs.last() { order.push(*idx); }
    if let Some(idx) = current { order.extend([idx, idx.saturating_add(1)]); }
    order.extend(costs.iter().map(|(idx, _)| *idx));
    let mut selected = Vec::new();
    let mut remaining = calls;
    for idx in order {
        if selected.contains(&idx) { continue; }
        if let Some((_, cost)) = costs.iter().find(|(candidate, _)| *candidate == idx) {
            if *cost <= remaining { selected.push(idx); remaining -= cost; }
        }
    }
    selected.sort_unstable();
    selected
}

fn retire_legacy_book_jobs(conn: &Connection) -> Result<(), String> {
    conn.execute("UPDATE jobs SET state='failed',error='旧版全书任务已停止，请显式启动限额采样概览',payload_json=json_set(COALESCE(payload_json,'{}'),'$.phase','failed'),updated_at=datetime('now') WHERE kind='global_extract' AND state IN ('queued','running','paused') AND COALESCE(json_extract(payload_json,'$.graph_format'),'')<>'kg-lite/v1'", [])
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn book_gate(book_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    BOOK_GATES.lock().entry(book_id).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
}

struct ActiveJob { book_id: i64, control: CallControl, users: usize }
struct RunningJob { id: i64, control: CallControl, finished: bool }
impl RunningJob {
    fn finish(&mut self, active: &mut HashMap<i64, ActiveJob>) -> usize {
        if self.finished { return active.get(&self.id).map_or(0, |job| job.users); }
        self.finished = true;
        let remaining = if let Some(job) = active.get_mut(&self.id) {
            job.users -= 1;
            job.users
        } else { 0 };
        if remaining == 0 { active.remove(&self.id); }
        remaining
    }

    fn finish_blocked(&mut self, active: &mut HashMap<i64, ActiveJob>, conn: &Connection, predecessor: i64, error: &str) -> rusqlite::Result<()> {
        // Only this prefetch interest is blocked. A foreground/other prefetch
        // owner can still complete the same registered job successfully.
        if self.finish(active) == 0 { jobs::mark_blocked(conn, self.id, predecessor, error)?; }
        Ok(())
    }
}
impl Drop for RunningJob {
    fn drop(&mut self) {
        if !self.finished { self.finish(&mut RUNNING_JOBS.lock()); }
    }
}

// All enqueue/registration and cancellation paths take registry before SQLite.
// Never hold SQLite while calling control.cancel(): a core commit holds its gate
// before taking SQLite, so reversing that order would deadlock cancellation.
fn register_job(active: &mut HashMap<i64, ActiveJob>, app: &tauri::AppHandle, book_id: i64, id: i64) -> RunningJob {
    let entry = active.entry(id).or_insert_with(|| {
        let app = app.clone();
        let observer = Arc::new(move |observation: CallObservation| {
            job_phase(&app, id, &observation.phase, Some(observation.attempt), observation.retry_after_ms, None);
        });
        ActiveJob { book_id, control: CallControl::new(CancellationToken::new(), Some(observer)), users: 0 }
    });
    entry.users += 1;
    RunningJob { id, control: entry.control.clone(), finished: false }
}

async fn cancellable<F: Future>(token: &CancellationToken, future: F) -> Result<F::Output, String> {
    tokio::select! {
        biased;
        _ = token.cancelled() => Err(crate::llm::LlmError::Cancelled.to_string()),
        result = future => Ok(result),
    }
}

async fn bounded<F: Future>(token: &CancellationToken, deadline: tokio::time::Instant, future: F) -> Result<F::Output, String> {
    if tokio::time::Instant::now() >= deadline { return Err("图谱任务安全期限已到，已提交结果保留".into()); }
    tokio::time::timeout_at(deadline, cancellable(token, future)).await
        .map_err(|_| "图谱任务安全期限已到，已提交结果保留".to_string())?
}

fn require_active(conn: &Connection, running: &RunningJob) -> Result<(), String> {
    running.control.check().map_err(|e| e.to_string())?;
    if !jobs::is_active(conn, running.id).map_err(|e| e.to_string())? {
        return Err("任务已结束或取消".into());
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_book_jobs(app: tauri::AppHandle, state: tauri::State<AppState>, book_id: i64) -> Result<(), String> {
    crate::commands::quick_read::cancel_book(book_id);
    let active = RUNNING_JOBS.lock();
    let ids = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let mut stmt = conn.prepare("SELECT id FROM jobs WHERE book_id=?1 AND state IN ('queued','running','paused')").map_err(|e| e.to_string())?;
        let ids = stmt.query_map([book_id], |row| row.get::<_, i64>(0)).map_err(|e| e.to_string())?
            .collect::<Result<Vec<_>, _>>().map_err(|e| e.to_string())?;
        jobs::cancel_book(conn, book_id).map_err(|e| e.to_string())?;
        ids
    };
    drop(active);
    for id in ids { notify_job(&app, id); }
    Ok(())
}

pub fn runtime_config(conn: &Connection, provider_id: Option<i64>) -> Result<(ProviderConfig, RuntimeConfig), String> {
    let id = match provider_id {
        Some(id) => id,
        None => conn.query_row(
            "SELECT id FROM llm_providers WHERE model_extract IS NOT NULL AND trim(model_extract) <> '' ORDER BY id LIMIT 1",
            [], |r| r.get(0),
        ).optional().map_err(|e| e.to_string())?.ok_or("请先配置 Provider 和抽取模型")?,
    };
    let cfg = providers::load_config(conn, id)?;
    let model_extract = cfg.model_for(TaskKind::Extract).map_err(|e| e.to_string())?.to_owned();
    let raw: Option<String> = conn.query_row("SELECT params_json FROM llm_providers WHERE id=?1", [id], |r| r.get(0)).map_err(|e| e.to_string())?;
    let params: Value = raw.map(|raw| serde_json::from_str(&raw)).transpose().map_err(|e| format!("Provider 参数无效: {e}"))?.unwrap_or_else(|| json!({}));
    let number = |key: &str, default: usize| -> Result<usize, String> {
        match params.get(key) {
            None => Ok(default),
            Some(value) => value.as_u64().and_then(|v| usize::try_from(v).ok()).filter(|v| *v > 0).ok_or_else(|| format!("Provider {key} 必须为正整数")),
        }
    };
    let context_limit = number("context_limit", 32768)?;
    let output_reserve = number("output_reserve", 4096)?;
    if output_reserve >= context_limit { return Err("输出预留必须小于模型上下文预算".into()); }
    Ok((cfg, RuntimeConfig { provider_id: id, model_extract, context_limit, output_reserve, prompt_version: "kg-lite/v1".into() }))
}

#[tauri::command]
pub fn get_graph(state: tauri::State<AppState>, query: GraphQuery) -> Result<GraphView, String> {
    let guard = state.db.lock();
    repo::get_view(guard.as_ref().ok_or("数据库未初始化")?, &query)
}


#[tauri::command]
pub fn reveal_graph(app: tauri::AppHandle, state: tauri::State<AppState>, book_id: i64, chapter_idx: i64) -> Result<(), String> {
    {
        let guard = state.db.lock();
        repo::reveal(guard.as_ref().ok_or("数据库未初始化")?, book_id, chapter_idx)?;
    }
    let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":chapter_idx}));
    Ok(())
}

#[tauri::command]
pub fn apply_graph_edit(app: tauri::AppHandle, state: tauri::State<AppState>, input: EditInput) -> Result<i64, String> {
    let (id, effective_from) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let id = repo::edit(conn, &input)?;
        (id, repo::progress(conn, input.book_id)? + 1)
    };
    let _ = app.emit("reading_line:changed", json!({"book_id":input.book_id,"effective_from":effective_from}));
    prefetch_next(app, input.book_id);
    Ok(id)
}

#[tauri::command]
pub fn revert_graph_edit(app: tauri::AppHandle, state: tauri::State<AppState>, book_id: i64, edit_id: i64) -> Result<(), String> {
    let effective_from = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let from = conn.query_row("SELECT effective_from FROM edits_log WHERE id=?1 AND book_id=?2", params![edit_id,book_id], |r| r.get::<_,i64>(0)).map_err(|e| e.to_string())?;
        repo::revert(conn, book_id, edit_id)?;
        from
    };
    let _ = app.emit("reading_line:changed", json!({"book_id":book_id,"effective_from":effective_from}));
    prefetch_next(app, book_id);
    Ok(())
}

#[tauri::command]
pub fn list_graph_edits(state: tauri::State<AppState>, book_id: i64) -> Result<Vec<EditRecord>, String> {
    let guard = state.db.lock();
    repo::active_edits(guard.as_ref().ok_or("数据库未初始化")?, book_id)
}

fn notify_job(app: &tauri::AppHandle, id: i64) {
    let snapshot = {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let Some(conn) = guard.as_ref() else { return; };
        conn.query_row("SELECT book_id,kind,state,progress,payload_json,error FROM jobs WHERE id=?1", [id], |row| {
            let raw: Option<String> = row.get(4)?;
            let payload: Value = raw.and_then(|raw| serde_json::from_str(&raw).ok()).unwrap_or_else(|| json!({}));
            Ok(json!({"book_id":row.get::<_, i64>(0)?,"job_id":id,"kind":row.get::<_, String>(1)?,
                "state":row.get::<_, String>(2)?,"progress":row.get::<_, f64>(3)?,"error":row.get::<_, Option<String>>(5)?,
                "chapter_idx":payload.get("chapter_idx"),"phase":payload.get("phase"),
                "attempt":payload.get("attempt"),"retry_after_ms":payload.get("retry_after_ms")}))
        }).ok()
    };
    if let Some(snapshot) = snapshot { let _ = app.emit("job:progress", snapshot); }
}

fn job_phase(app: &tauri::AppHandle, id: i64, phase: &str, attempt: Option<u32>, delay: Option<u64>, chapter: Option<i64>) {
    {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let Some(conn) = guard.as_ref() else { return; };
        if let Some(chapter) = chapter {
            // Only the chapter key is patched; completed windows/chapters survive.
            let _ = conn.execute("UPDATE jobs SET payload_json=json_set(COALESCE(payload_json,'{}'),'$.chapter_idx',?2) WHERE id=?1 AND state IN ('queued','running','paused')", params![id, chapter]);
        }
        let _ = jobs::set_phase(conn, id, phase, attempt, delay);
    }
    notify_job(app, id);
}

fn finish_job<T>(app: &tauri::AppHandle, state: &AppState, running: &mut RunningJob, result: &Result<T, String>) -> Result<(), String> {
    {
        // Serializes terminal publication with explicit cancel; cancelled is sticky.
        let mut active = RUNNING_JOBS.lock();
        let remaining = running.finish(&mut active);
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        if running.control.cancellation.is_cancelled() {
            jobs::mark_cancelled(conn, running.id)
        } else {
            match result {
                Ok(_) if remaining > 0 => Ok(()),
                Ok(_) => jobs::mark_done(conn, running.id),
                Err(error) => jobs::mark_failed(conn, running.id, error),
            }
        }.map_err(|e| e.to_string())?;
    }
    notify_job(app, running.id);
    Ok(())
}

/// Queue a bounded original-reference sample plan. Repeated calls share the active job.
pub fn schedule_book(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>, budget_started_ms: u64) -> Result<i64, String> {
    let id = {
        let _active = RUNNING_JOBS.lock();
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE id=?1)", [book_id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !exists { return Err("书籍不存在".into()); }
        retire_legacy_book_jobs(conn)?;
        let coverage = repo::coverage(conn, book_id)?;
        let attempted = prior_book_attempts(conn, book_id)?;
        let id = jobs::enqueue(conn, book_id, jobs::JobKind::GlobalExtract, json!({"provider_id":provider_id,"committed_chapters":[],"attempted_chapters":attempted,"phase":"queued","origin":"explicit","graph_format":"kg-lite/v1","budget_started_ms":budget_started_ms,"budget_elapsed_base_ms":coverage.elapsed_ms})).map_err(|e| e.to_string())?;
        jobs::mark_explicit(conn, id).map_err(|e| e.to_string())?;
        id
    };
    spawn_book_job(app, book_id, id, provider_id);
    Ok(id)
}

#[tauri::command]
pub fn start_book_extraction(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>) -> Result<i64, String> {
    schedule_book(app, book_id, provider_id, epoch_ms())
}

fn spawn_book_job(app: tauri::AppHandle, book_id: i64, job_id: i64, provider_id: Option<i64>) {
    let running = {
        let mut active = RUNNING_JOBS.lock();
        if active.contains_key(&job_id) { return; }
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let Some(conn) = guard.as_ref() else { return; };
        if !jobs::is_active(conn, job_id).unwrap_or(false) { return; }
        register_job(&mut active, &app, book_id, job_id)
    };
    tauri::async_runtime::spawn(async move {
        let mut running = running;
        let _ = run_book_job(&app, book_id, &mut running, provider_id).await;
    });
}

fn job_payload(conn: &Connection, job_id: i64) -> Result<Value, String> {
    let raw: Option<String> = conn.query_row("SELECT payload_json FROM jobs WHERE id=?1", [job_id], |r| r.get(0)).map_err(|e| e.to_string())?;
    raw.map(|raw| serde_json::from_str(&raw)).transpose().map_err(|e| e.to_string()).map(|value| value.unwrap_or_else(|| json!({})))
}

fn prior_book_attempts(conn: &Connection, book_id: i64) -> Result<Vec<i64>, String> {
    let id = conn.query_row("SELECT id FROM jobs WHERE book_id=?1 AND kind='global_extract' ORDER BY id DESC LIMIT 1", [book_id], |r| r.get::<_, i64>(0)).optional().map_err(|e| e.to_string())?;
    id.map(|id| {
        job_payload(conn, id)?.get("attempted_chapters").cloned().map(serde_json::from_value).transpose().map_err(|e| e.to_string()).map(|value| value.unwrap_or_default())
    }).transpose().map(|value| value.unwrap_or_default())
}

fn checkpoint_book(conn: &Connection, book_id: i64, job_id: i64, calls: usize, elapsed: u64) -> Result<(), String> {
    conn.execute_batch("SAVEPOINT kg_budget_checkpoint").map_err(|e| e.to_string())?;
    let result = (|| {
        repo::record_extraction_attempt(conn, book_id, calls, elapsed)?;
        let coverage = repo::coverage(conn, book_id)?;
        let progress = if coverage.selected_chapters.is_empty() { 0.0 } else {
            coverage.completed_chapters.iter().filter(|idx| coverage.selected_chapters.contains(idx)).count() as f64 / coverage.selected_chapters.len() as f64
        };
        jobs::set_progress(conn, job_id, progress, &json!({"selected_chapters":coverage.selected_chapters,"committed_chapters":coverage.completed_chapters,"calls_used":coverage.calls_used,"elapsed_ms":coverage.elapsed_ms})).map_err(|e| e.to_string())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE kg_budget_checkpoint").map_err(|e| e.to_string()),
        Err(error) => { let _ = conn.execute_batch("ROLLBACK TO kg_budget_checkpoint; RELEASE kg_budget_checkpoint"); Err(error) }
    }
}

fn book_budget(app: &tauri::AppHandle, book_id: i64, job_id: i64, payload: &Value, calls: usize, elapsed: u64, chapter_idx: Option<i64>) -> Result<crate::core::kg_extract::ExtractionBudget, String> {
    let elapsed = book_elapsed(payload, elapsed, epoch_ms());
    let mut budget = crate::core::kg_extract::ExtractionBudget::new(BOOK_CALL_LIMIT, tokio::time::Instant::now() + book_remaining(elapsed)?);
    budget.calls_used = calls;
    let app = app.clone();
    let payload = payload.clone();
    budget.checkpoint = Some(Arc::new(move |calls| {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        if !jobs::is_active(conn, job_id).map_err(|e| e.to_string())? { return Err("任务已结束或取消".into()); }
        let coverage = repo::coverage(conn, book_id)?;
        if calls <= coverage.calls_used || calls > BOOK_CALL_LIMIT { return Err("图谱请求预算已变化，请恢复已有采样任务".into()); }
        let elapsed = book_elapsed(&payload, coverage.elapsed_ms, epoch_ms());
        book_remaining(elapsed)?;
        let job = job_payload(conn, job_id)?;
        let chapter = chapter_idx.or_else(|| job.get("chapter_idx").and_then(Value::as_i64));
        conn.execute_batch("SAVEPOINT kg_reserve_attempt").map_err(|e| e.to_string())?;
        let result = (|| {
            if let Some(chapter) = chapter {
                let mut attempted: Vec<i64> = job.get("attempted_chapters").cloned().map(serde_json::from_value).transpose().map_err(|e| e.to_string())?.unwrap_or_default();
                if !attempted.contains(&chapter) { attempted.push(chapter); attempted.sort_unstable(); }
                conn.execute("UPDATE jobs SET payload_json=json_set(COALESCE(payload_json,'{}'),'$.attempted_chapters',json(?2)) WHERE id=?1 AND state IN ('queued','running','paused')", params![job_id, serde_json::to_string(&attempted).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
                if !coverage.selected_chapters.contains(&chapter) {
                    let mut selected = coverage.selected_chapters.clone();
                    selected.push(chapter);
                    repo::set_extraction_plan(conn, book_id, &selected, coverage.calls_used, elapsed)?;
                }
            }
            checkpoint_book(conn, book_id, job_id, calls, elapsed)
        })();
        match result {
            Ok(()) => conn.execute_batch("RELEASE kg_reserve_attempt").map_err(|e| e.to_string()),
            Err(error) => { let _ = conn.execute_batch("ROLLBACK TO kg_reserve_attempt; RELEASE kg_reserve_attempt"); Err(error) }
        }
    }));
    Ok(budget)
}

fn prepare_book_plan(conn: &Connection, book_id: i64, config: &RuntimeConfig, payload: &Value) -> Result<Vec<i64>, String> {
    let coverage = repo::coverage(conn, book_id)?;
    if repo::extraction_plan_exists(conn, book_id)? && (!coverage.selected_chapters.is_empty() || coverage.calls_used > 0) { return Ok(coverage.selected_chapters); }
    let total = crate::store::books::count_chapters(conn, book_id).map_err(|e| e.to_string())?;
    let current = Some(repo::progress(conn, book_id)?.max(1));
    let candidates = crate::core::kg_extract::sample_chapters(total, current, BOOK_CALL_LIMIT);
    let mut costs = Vec::with_capacity(candidates.len());
    for idx in candidates {
        book_remaining(book_elapsed(payload, coverage.elapsed_ms, epoch_ms()))?;
        let text = repo::text_version(conn, book_id, idx, "original", None)?;
        let cost = if repo::extraction_matches(conn, text.id, config)? { 0 } else { crate::core::kg_extract::request_count(&text.content) };
        costs.push((idx, cost));
    }
    let selected = trim_plan(&costs, current, BOOK_CALL_LIMIT.saturating_sub(coverage.calls_used));
    repo::set_extraction_plan(conn, book_id, &selected, coverage.calls_used, book_elapsed(payload, coverage.elapsed_ms, epoch_ms()))?;
    Ok(selected)
}

fn checkpoint_book_clock(conn: &Connection, book_id: i64, job_id: i64, payload: &Value, now: u64) -> Result<u64, String> {
    let coverage = repo::coverage(conn, book_id)?;
    let elapsed = book_elapsed(payload, coverage.elapsed_ms, now);
    if !repo::extraction_plan_exists(conn, book_id)? { repo::set_extraction_plan(conn, book_id, &[], coverage.calls_used, elapsed)?; }
    checkpoint_book(conn, book_id, job_id, coverage.calls_used, elapsed)?;
    Ok(elapsed)
}

fn retain_foreground_target(conn: &Connection, book_id: i64, job_id: i64, chapter_idx: i64, error: Option<&str>) -> Result<(), String> {
    if !jobs::is_active(conn, job_id).map_err(|e| e.to_string())? { return Ok(()); }
    conn.execute_batch("SAVEPOINT kg_foreground_failure").map_err(|e| e.to_string())?;
    let result = (|| {
        let payload = job_payload(conn, job_id)?;
        let coverage = repo::coverage(conn, book_id)?;
        let mut selected = coverage.selected_chapters;
        if !selected.contains(&chapter_idx) { selected.push(chapter_idx); }
        let mut attempted: Vec<i64> = payload.get("attempted_chapters").cloned().map(serde_json::from_value).transpose().map_err(|e| e.to_string())?.unwrap_or_default();
        if !attempted.contains(&chapter_idx) { attempted.push(chapter_idx); attempted.sort_unstable(); }
        repo::set_extraction_plan(conn, book_id, &selected, coverage.calls_used, book_elapsed(&payload, coverage.elapsed_ms, epoch_ms()))?;
        conn.execute("UPDATE jobs SET payload_json=json_set(COALESCE(payload_json,'{}'),'$.attempted_chapters',json(?2)) WHERE id=?1 AND state IN ('queued','running','paused')", params![job_id, serde_json::to_string(&attempted).map_err(|e| e.to_string())?]).map_err(|e| e.to_string())?;
        checkpoint_book(conn, book_id, job_id, coverage.calls_used, book_elapsed(&payload, coverage.elapsed_ms, epoch_ms()))?;
        if let Some(error) = error { jobs::mark_failed(conn, job_id, &format!("前景抽取失败，采样目标保留：{error}")).map_err(|e| e.to_string())?; }
        Ok(())
    })();
    match result {
        Ok(()) => conn.execute_batch("RELEASE kg_foreground_failure").map_err(|e| e.to_string()),
        Err(error) => { let _ = conn.execute_batch("ROLLBACK TO kg_foreground_failure; RELEASE kg_foreground_failure"); Err(error) }
    }
}

fn fail_foreground_target(conn: &Connection, book_id: i64, job_id: i64, chapter_idx: i64, error: &str) -> Result<(), String> {
    retain_foreground_target(conn, book_id, job_id, chapter_idx, Some(error))
}

async fn run_book_job(app: &tauri::AppHandle, book_id: i64, running: &mut RunningJob, provider_id: Option<i64>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let mut result = run_book_samples(app, book_id, running, provider_id).await;
    let deadline = (|| {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let payload = job_payload(conn, running.id)?;
        let elapsed = checkpoint_book_clock(conn, book_id, running.id, &payload, epoch_ms())?;
        Ok::<_, String>(tokio::time::Instant::now() + book_remaining(elapsed)?)
    })();
    let gate = book_gate(book_id);
    let terminal_gate = match deadline {
        Ok(deadline) => bounded(&running.control.cancellation, deadline, gate.lock()).await,
        Err(error) => Err(error),
    };
    if result.is_ok() {
        result = match &terminal_gate {
            Err(error) => Err(error.clone()),
            Ok(_) => (|| {
                let _commit = running.control.commit_guard().map_err(|e| e.to_string())?;
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                require_active(conn, running)?;
                let payload = job_payload(conn, running.id)?;
                let elapsed = checkpoint_book_clock(conn, book_id, running.id, &payload, epoch_ms())?;
                book_remaining(elapsed)?;
                if !repo::coverage(conn, book_id)?.target_complete { return Err("预算内没有完整图谱样本，概览未完成".into()); }
                repo::save_original_summary(conn, book_id)?;
                book_remaining(book_elapsed(&payload, elapsed, epoch_ms()))?;
                Ok(())
            })(),
        };
    }
    // Keep the FIFO gate until the owner's terminal state is durable, so no
    // current/next caller can start using a budget between summary and done.
    finish_job(app, &state, running, &result)?;
    {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let payload = job_payload(conn, running.id)?;
        checkpoint_book_clock(conn, book_id, running.id, &payload, epoch_ms())?;
    }
    result
}

fn active_book_budget(conn: &Connection, book_id: i64) -> Result<Option<(i64, Value)>, String> {
    let row = conn.query_row("SELECT id,payload_json FROM jobs WHERE book_id=?1 AND kind='global_extract' AND state IN ('queued','running','paused') AND json_extract(payload_json,'$.graph_format')='kg-lite/v1' ORDER BY id LIMIT 1", [book_id], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?))).optional().map_err(|e| e.to_string())?;
    row.map(|(id, raw)| serde_json::from_str(&raw).map(|payload| (id, payload)).map_err(|e| e.to_string())).transpose()
}

fn fit_remaining_book_plan(conn: &Connection, book_id: i64, job_id: i64, config: &RuntimeConfig, payload: &Value) -> Result<Vec<i64>, String> {
    let coverage = repo::coverage(conn, book_id)?;
    let attempted: Vec<i64> = job_payload(conn, job_id)?.get("attempted_chapters").cloned().map(serde_json::from_value).transpose().map_err(|e| e.to_string())?.unwrap_or_default();
    let mut candidates = coverage.selected_chapters;
    candidates.extend(coverage.completed_chapters);
    candidates.extend(attempted.iter().copied());
    candidates.sort_unstable();
    candidates.dedup();
    let mut costs = Vec::with_capacity(candidates.len());
    let mut mandatory = Vec::new();
    let mut reserved = 0usize;
    for idx in candidates {
        book_remaining(book_elapsed(payload, coverage.elapsed_ms, epoch_ms()))?;
        let text = repo::text_version(conn, book_id, idx, "original", None)?;
        let cost = if repo::extraction_matches(conn, text.id, config)? { 0 } else { crate::core::kg_extract::request_count(&text.content) };
        if cost > 0 && attempted.contains(&idx) {
            mandatory.push(idx);
            reserved = reserved.saturating_add(cost);
        } else { costs.push((idx, cost)); }
    }
    let mut selected = trim_plan(&costs, Some(repo::progress(conn, book_id)?.max(1)), BOOK_CALL_LIMIT.saturating_sub(coverage.calls_used).saturating_sub(reserved));
    selected.extend(mandatory);
    selected.sort_unstable();
    repo::set_extraction_plan(conn, book_id, &selected, coverage.calls_used, book_elapsed(payload, coverage.elapsed_ms, epoch_ms()))?;
    Ok(selected)
}

async fn run_book_samples(app: &tauri::AppHandle, book_id: i64, running: &RunningJob, provider_id: Option<i64>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let token = &running.control.cancellation;
    let (cfg, config, payload, selected, calls, elapsed) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        require_active(conn, running)?;
        jobs::mark_running(conn, running.id).map_err(|e| e.to_string())?;
        let payload = job_payload(conn, running.id)?;
        if payload.get("budget_started_ms").and_then(Value::as_u64).is_none() { return Err("采样任务缺少持久时间预算，已停止自动恢复".into()); }
        let elapsed = checkpoint_book_clock(conn, book_id, running.id, &payload, epoch_ms())?;
        let coverage = repo::coverage(conn, book_id)?;
        book_remaining(elapsed)?;
        let (cfg, config) = runtime_config(conn, provider_id)?;
        let selected = prepare_book_plan(conn, book_id, &config, &payload)?;
        let elapsed = book_elapsed(&payload, elapsed, epoch_ms());
        checkpoint_book(conn, book_id, running.id, coverage.calls_used, elapsed)?;
        (cfg, config, payload, selected, coverage.calls_used, elapsed)
    };
    let client = providers::make_client_with_control(cfg, running.control.clone())?;
    let mut budget = book_budget(app, book_id, running.id, &payload, calls, elapsed, None)?;
    let result: Result<(), String> = async {
        for idx in &selected {
            // Foreground current/next jobs enter the same FIFO gate first. Never
            // hold a permit while yielding to them, and charge the waiting time.
            loop {
                let foreground = {
                    let guard = state.db.lock();
                    let conn = guard.as_ref().ok_or("数据库未初始化")?;
                    require_active(conn, running)?;
                    conn.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE book_id=?1 AND kind IN ('chapter_extract','generate','reconcile') AND state IN ('queued','running','paused'))", [book_id], |r| r.get::<_, bool>(0)).map_err(|e| e.to_string())?
                };
                if !foreground { break; }
                bounded(token, budget.deadline, tokio::time::sleep(std::time::Duration::from_millis(10))).await?;
            }
            job_phase(app, running.id, "book_wait", None, None, Some(*idx));
            let gate = book_gate(book_id);
            let _book = bounded(token, budget.deadline, gate.lock()).await?;
            let text = {
                let _commit = client.commit_guard().map_err(|e| e.to_string())?;
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                require_active(conn, running)?;
                jobs::ensure_prefetch_allowed(conn, running.id)?;
                let coverage = repo::coverage(conn, book_id)?;
                let selected_now = if coverage.calls_used > budget.calls_used {
                    fit_remaining_book_plan(conn, book_id, running.id, &config, &payload)?
                } else { coverage.selected_chapters };
                budget.calls_used = budget.calls_used.max(coverage.calls_used);
                if !selected_now.contains(idx) { continue; }
                let text = repo::text_version(conn, book_id, *idx, "original", None)?;
                if repo::extraction_matches(conn, text.id, &config)? { continue; }
                text
            };
            job_phase(app, running.id, "provider_wait", None, None, Some(*idx));
            let _permit = bounded(token, budget.deadline, PROVIDER_LIMIT.acquire()).await?.map_err(|e| e.to_string())?;
            job_phase(app, running.id, "chapter", None, None, Some(*idx));
            let deadline = budget.deadline;
            bounded(token, deadline, crate::core::kg_extract::extract_with_budget(&state, client.as_ref(), text, &config, Some(running.id), &mut budget)).await??;
            {
                let _commit = client.commit_guard().map_err(|e| e.to_string())?;
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                require_active(conn, running)?;
                checkpoint_book(conn, book_id, running.id, budget.calls_used, book_elapsed(&payload, elapsed, epoch_ms()))?;
            }
            notify_job(app, running.id);
            let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":idx}));
        }
        Ok(())
    }.await;
    {
        let guard = state.db.lock();
        if let Some(conn) = guard.as_ref() {
            checkpoint_book(conn, book_id, running.id, budget.calls_used, book_elapsed(&payload, elapsed, epoch_ms()))?;
        }
    }
    result
}

pub fn resume_jobs(app: &tauri::AppHandle) -> Result<(), String> {
    crate::commands::quick_read::resume_jobs(app)?;
    let queued = {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        retire_legacy_book_jobs(conn)?;
        jobs::recover_orphans(conn).map_err(|e| e.to_string())?;
        let mut stmt = conn.prepare("SELECT id,book_id,payload_json,kind FROM jobs WHERE kind IN ('global_extract','chapter_extract','generate','reconcile') AND state IN ('queued','paused') AND book_id IN (SELECT id FROM books) ORDER BY id").map_err(|e| e.to_string())?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_,i64>(0)?,r.get::<_,i64>(1)?,r.get::<_,Option<String>>(2)?,r.get::<_,String>(3)?))).map_err(|e| e.to_string())?;
        rows.collect::<Result<Vec<_>,_>>().map_err(|e| e.to_string())?
    };
    for (id, book_id, payload, kind) in queued {
        let value: Value = payload.map(|s| serde_json::from_str(&s)).transpose().map_err(|e| e.to_string())?.unwrap_or_else(||json!({}));
        let provider_id = value.get("provider_id").and_then(Value::as_i64);
        let blocked = {
            let state = app.state::<AppState>();
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            if !jobs::is_active(conn, id).map_err(|e| e.to_string())? {
                None
            } else {
                match jobs::ensure_prefetch_allowed(conn, id) {
                    Ok(()) => Some(false),
                    Err(error) => {
                        jobs::mark_failed(conn, id, &error).map_err(|e| e.to_string())?;
                        Some(true)
                    }
                }
            }
        };
        if blocked.is_none() { continue; }
        if blocked == Some(true) {
            notify_job(app, id);
            continue;
        }
        if kind == "global_extract" {
            spawn_book_job(app.clone(), book_id, id, provider_id);
        } else if let Some(idx) = value.get("chapter_idx").and_then(Value::as_i64) {
            let running = {
                let mut active = RUNNING_JOBS.lock();
                let state = app.state::<AppState>();
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                if !jobs::is_active(conn, id).map_err(|e| e.to_string())? { continue; }
                register_job(&mut active, app, book_id, id)
            };
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let _ = run_selected(&app, &state, book_id, idx, provider_id, running).await;
            });
        }
    }
    Ok(())
}

pub async fn ensure_selected(app: &tauri::AppHandle, state: &AppState, book_id: i64, idx: i64, provider_id: Option<i64>) -> Result<TextVersion, String> {
    let running = {
        let mut active = RUNNING_JOBS.lock();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        enqueue_selected(&mut active, app, conn, book_id, idx, provider_id)?
    };
    run_selected(app, state, book_id, idx, provider_id, running).await
}

fn story_generation_requested(conn:&Connection,book_id:i64,idx:i64)->Result<bool,String> {
    Ok(crate::store::kg::divergence(conn,book_id)?.is_some_and(|from|idx>=from) || crate::commands::story::eligible_next_wiki(conn,book_id,idx)?.is_some())
}

fn enqueue_selected(active:&mut HashMap<i64,ActiveJob>,app:&tauri::AppHandle,conn:&Connection,book_id:i64,idx:i64,provider_id:Option<i64>)->Result<RunningJob,String> {
    let generated = story_generation_requested(conn, book_id, idx)?;
    let kind = if generated { jobs::JobKind::Generate } else { jobs::JobKind::ChapterExtract };
    let id = jobs::enqueue(conn, book_id, kind, json!({"chapter_idx":idx,"provider_id":provider_id,"phase":"queued","origin":"explicit","selected_started_ms":epoch_ms(),"graph_format":"kg-lite/v1"})).map_err(|e| e.to_string())?;
    jobs::mark_explicit(conn, id).map_err(|e| e.to_string())?;
    Ok(register_job(active, app, book_id, id))
}

fn enqueue_selected_prefetch(active: &mut HashMap<i64, ActiveJob>, app: &tauri::AppHandle, conn: &Connection, book_id: i64, idx: i64, provider_id: Option<i64>) -> Result<Option<RunningJob>, String> {
    let generated = story_generation_requested(conn, book_id, idx)?;
    let kind = if generated { jobs::JobKind::Generate } else { jobs::JobKind::ChapterExtract };
    let id = jobs::enqueue_prefetch(conn, book_id, kind, json!({"chapter_idx":idx,"provider_id":provider_id,"phase":"queued","selected_started_ms":epoch_ms(),"graph_format":"kg-lite/v1"})).map_err(|e| e.to_string())?;
    Ok(id.map(|id| register_job(active, app, book_id, id)))
}

async fn run_selected(app: &tauri::AppHandle, state: &AppState, book_id: i64, idx: i64, provider_id: Option<i64>, mut running: RunningJob) -> Result<TextVersion, String> {
    let result: Result<TextVersion, String> = async {
        let token = &running.control.cancellation;
        let mut deadline;
        {
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, &running)?;
            let queued = job_payload(conn, running.id)?.get("selected_started_ms").and_then(Value::as_u64).unwrap_or_else(epoch_ms);
            let original = !story_generation_requested(conn, book_id, idx)?;
            let elapsed = epoch_ms().saturating_sub(queued);
            deadline = tokio::time::Instant::now() + selected_remaining(original, elapsed)?;
            if original {
                if let Some((_, payload)) = active_book_budget(conn, book_id)? {
                    deadline = deadline.min(tokio::time::Instant::now() + book_remaining(book_elapsed(&payload, repo::coverage(conn, book_id)?.elapsed_ms, epoch_ms()))?);
                }
            }
        }
        job_phase(app, running.id, "book_wait", None, None, Some(idx));
        let gate = book_gate(book_id);
        let _book = bounded(token, deadline, gate.lock()).await?;
        let (cfg, config, generated, shared_budget) = {
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, &running)?;
            jobs::ensure_prefetch_allowed(conn, running.id)?;
            jobs::mark_running(conn, running.id).map_err(|e| e.to_string())?;
            let (cfg, config) = runtime_config(conn, provider_id)?;
            let generated = story_generation_requested(conn, book_id, idx)?;
            let shared_budget = if !generated {
                if let Some((owner, payload)) = active_book_budget(conn, book_id)? {
                    prepare_book_plan(conn, book_id, &config, &payload)?;
                    retain_foreground_target(conn, book_id, owner, idx, None)?;
                    let coverage = repo::coverage(conn, book_id)?;
                    let elapsed = book_elapsed(&payload, coverage.elapsed_ms, epoch_ms());
                    Some((owner, payload, coverage.calls_used, elapsed))
                } else { None }
            } else { None };
            (cfg, config, generated, shared_budget)
        };
        let client = providers::make_client_with_control(cfg.clone(), running.control.clone())?;
        let mut budget = shared_budget.as_ref().map(|(owner, payload, calls, elapsed)| book_budget(app, book_id, *owner, payload, *calls, *elapsed, Some(idx))).transpose()?;
        if let Some(budget) = budget.as_mut() {
            deadline = deadline.min(budget.deadline);
            budget.deadline = deadline;
        } else if !generated {
            budget = Some(crate::core::kg_extract::ExtractionBudget::new(usize::MAX, deadline));
        }
        job_phase(app, running.id, "provider_wait", None, None, Some(idx));
        let _permit = bounded(token, deadline, PROVIDER_LIMIT.acquire()).await?.map_err(|e| e.to_string())?;
        job_phase(app, running.id, "chapter", None, None, Some(idx));
        let text = if generated {
            bounded(token, deadline, crate::core::kg_generate::ensure_rewritten_chapter(state, client.as_ref(), &cfg, &config, book_id, idx, Some(running.id))).await??
        } else {
            let text = {
                let _commit = client.commit_guard().map_err(|e| e.to_string())?;
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                require_active(conn, &running)?;
                repo::text_version(conn, book_id, idx, "original", None)?
            };
            let extracted = if let Some(budget) = budget.as_mut() {
                bounded(token, deadline, crate::core::kg_extract::extract_with_budget(state, client.as_ref(), text.clone(), &config, Some(running.id), budget)).await.and_then(|result| result)
            } else {
                bounded(token, deadline, crate::core::kg_extract::extract(state, client.as_ref(), text.clone(), &config, Some(running.id))).await.and_then(|result| result)
            };
            if let (Some((owner, payload, _, elapsed)), Some(budget)) = (&shared_budget, &budget) {
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                checkpoint_book(conn, book_id, *owner, budget.calls_used, book_elapsed(payload, *elapsed, epoch_ms()))?;
            }
            if let (Some((owner, _, _, _)), Err(error)) = (&shared_budget, &extracted) {
                let guard = state.db.lock();
                let conn = guard.as_ref().ok_or("数据库未初始化")?;
                fail_foreground_target(conn, book_id, *owner, idx, error)?;
            }
            extracted?;
            text
        };
        running.control.check().map_err(|e| e.to_string())?;
        Ok(text)
    }.await;
    finish_job(app, state, &mut running, &result)?;
    if result.is_ok() && !running.control.cancellation.is_cancelled() {
        let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":idx}));
    }
    running.control.check().map_err(|e| e.to_string())?;
    result
}

#[tauri::command]
pub async fn extract_chapter_graph(app: tauri::AppHandle, state: tauri::State<'_, AppState>, book_id: i64, chapter_idx: i64, provider_id: Option<i64>) -> Result<(), String> {
    {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        if chapter_idx < 1 || chapter_idx > repo::progress(conn, book_id)? + 1 {
            return Err("仅可补充已读章节与下一章图谱".into());
        }
    }
    ensure_selected(&app, &state, book_id, chapter_idx, provider_id).await.map(|_| ())
}

/// Enqueue prefetch before spawning so cancellation includes the next chapter
/// while its predecessor is waiting or running. Terminal failures gate future
/// automatic work until an explicit request succeeds.
pub fn prefetch_next(app: tauri::AppHandle, book_id: i64) {
    let queued = {
        let mut active = RUNNING_JOBS.lock();
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let Some(conn) = guard.as_ref() else { return; };
        let reference = match crate::store::quick_read::active_reference(conn, book_id) { Ok(Some(reference)) => reference, _ => return };
        let bootstrap_ready: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM jobs WHERE book_id=?1 AND kind='bootstrap_wiki' AND state='done' AND json_extract(payload_json,'$.source_digest')=?2)", params![book_id, reference.source_digest], |row| row.get(0)).unwrap_or(false);
        if !bootstrap_ready { return; }
        let (progress, total) = match (repo::progress(conn, book_id), crate::store::books::count_chapters(conn, book_id)) {
            (Ok(progress), Ok(total)) if progress > 0 => (progress, total),
            _ => return,
        };
        let mut queued = Vec::new();
        for idx in progress..=(progress + 1).min(total) {
            match enqueue_selected_prefetch(&mut active, &app, conn, book_id, idx, None) {
                Ok(Some(running)) => queued.push((idx, running)),
                Ok(None) | Err(_) => break,
            }
        }
        queued
    };
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let mut blocked: Option<(i64, String)> = None;
        for (idx, mut running) in queued {
            if let Some((predecessor, error)) = &blocked {
                {
                    let mut active = RUNNING_JOBS.lock();
                    let guard = state.db.lock();
                    if let Some(conn) = guard.as_ref() {
                        let _ = running.finish_blocked(&mut active, conn, *predecessor, error);
                    }
                }
                notify_job(&app, running.id);
                continue;
            }
            if let Err(error) = run_selected(&app, &state, book_id, idx, None, running).await {
                blocked = Some((idx, error));
            }
        }
    });
}

#[cfg(test)]
mod cancellation_tests {
    use super::*;
    use tokio_util::sync::CancellationToken;

    fn job_database() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn
    }

    fn sampled_book(conn: &Connection, total: i64, content: &str) -> i64 {
        let chapters: Vec<_> = (1..=total).map(|idx| crate::store::books::NewChapter { idx, title: format!("Chapter {idx}"), text: content.into(), token_est: 10 }).collect();
        crate::store::books::create_book_with_chapters(conn, "Budget fixture", None, "txt", "test", &chapters).unwrap()
    }

    fn test_config() -> RuntimeConfig {
        RuntimeConfig { provider_id: 1, model_extract: "test".into(), context_limit: 32768, output_reserve: 4096, prompt_version: "kg-lite/v1".into() }
    }

    #[test]
    fn unqualified_legacy_plan_does_not_select_generated_chapter() {
        let conn = job_database();
        let book = sampled_book(&conn, 2, "前章正文");
        let previous = repo::text_version(&conn, book, 1, "original", None).unwrap();
        let wiki = crate::core::story::NextChapterWiki { chapter_idx: 2, goals: vec!["继续调查".into()], state_directions: vec![], open_threads: vec![], avoid: vec![], ending_direction: "留下线索".into(), related_entry_ids: vec![] };
        crate::store::story::save_next_wiki(&conn, book, 2, previous.id, 0, "dependency", &wiki).unwrap();
        assert!(!story_generation_requested(&conn, book, 2).unwrap());
        assert!(!story_generation_requested(&conn, book, 1).unwrap());
    }

    #[test]
    fn production_plan_budgets_whole_long_chapters_and_preserves_cached_tail() {
        let conn = job_database();
        let book = sampled_book(&conn, 100, &"字".repeat(4097));
        let config = test_config();
        let tail = repo::text_version(&conn, book, 100, "original", None).unwrap();
        repo::commit_snapshot(&conn, &tail, &Default::default(), "test", 0, None, &config).unwrap();
        let payload = json!({"budget_started_ms":epoch_ms(),"budget_elapsed_base_ms":0});
        let selected = prepare_book_plan(&conn, book, &config, &payload).unwrap();
        assert!(selected.contains(&1));
        assert!(selected.contains(&100));
        let cost: usize = selected.iter().map(|idx| {
            let text = repo::text_version(&conn, book, *idx, "original", None).unwrap();
            if repo::extraction_matches(&conn, text.id, &config).unwrap() { 0 } else { crate::core::kg_extract::request_count(&text.content) }
        }).sum();
        assert_eq!(cost, 44);
        assert_eq!(selected.len(), 23);
        repo::record_extraction_attempt(&conn, book, 44, 470_000).unwrap();
        assert_eq!(prepare_book_plan(&conn, book, &config, &payload).unwrap(), selected);
        let coverage = repo::coverage(&conn, book).unwrap();
        assert_eq!(coverage.calls_used, 44);
        assert_eq!(coverage.elapsed_ms, 470_000);
        assert!(!coverage.target_complete);
    }

    #[test]
    fn failed_request_checkpoint_consumes_quota_without_marking_sample_completed() {
        let conn = job_database();
        let book = sampled_book(&conn, 2, "Original prose");
        repo::set_extraction_plan(&conn, book, &[1, 2], 0, 0).unwrap();
        let id = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, json!({"graph_format":"kg-lite/v1"})).unwrap();
        checkpoint_book(&conn, book, id, 1, 100).unwrap();
        jobs::mark_failed(&conn, id, "Provider request failed").unwrap();
        checkpoint_book(&conn, book, id, 0, 50).unwrap();
        let coverage = repo::coverage(&conn, book).unwrap();
        assert_eq!(coverage.calls_used, 1);
        assert_eq!(coverage.elapsed_ms, 100);
        assert!(coverage.completed_chapters.is_empty());
        assert!(!coverage.target_complete);
        let payload = job_payload(&conn, id).unwrap();
        assert_eq!(payload["calls_used"], 1);
        assert_eq!(payload["elapsed_ms"], 100);
        assert_eq!(payload["phase"], "failed");
    }

    #[test]
    fn checkpoint_transaction_rolls_back_counter_if_job_checkpoint_cannot_persist() {
        let conn = job_database();
        let book = sampled_book(&conn, 1, "Original prose");
        repo::set_extraction_plan(&conn, book, &[1], 0, 0).unwrap();
        let id = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, json!({"graph_format":"kg-lite/v1"})).unwrap();
        conn.execute_batch("CREATE TRIGGER reject_checkpoint BEFORE UPDATE ON jobs BEGIN SELECT RAISE(ABORT,'checkpoint failed'); END").unwrap();
        assert!(checkpoint_book(&conn, book, id, 1, 100).is_err());
        assert_eq!(repo::coverage(&conn, book).unwrap().calls_used, 0);
        assert!(job_payload(&conn, id).unwrap().get("calls_used").is_none());
    }

    #[test]
    fn foreground_refit_preserves_committed_and_failed_targets_not_fake_success() {
        let conn = job_database();
        let book = sampled_book(&conn, 5, "Original prose");
        let config = test_config();
        repo::set_extraction_plan(&conn, book, &[1, 2, 3, 4], 44, 100).unwrap();
        for idx in [1, 4, 5] {
            let text = repo::text_version(&conn, book, idx, "original", None).unwrap();
            repo::commit_snapshot(&conn, &text, &Default::default(), "test", 0, None, &config).unwrap();
        }
        let payload = json!({"graph_format":"kg-lite/v1","budget_started_ms":epoch_ms(),"budget_elapsed_base_ms":0,"attempted_chapters":[2]});
        let id = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, payload.clone()).unwrap();
        let selected = fit_remaining_book_plan(&conn, book, id, &config, &payload).unwrap();
        assert_eq!(selected, vec![1, 2, 4, 5]);
        let coverage = repo::coverage(&conn, book).unwrap();
        assert!(!coverage.target_complete);
        assert_eq!(coverage.mode, "partial");
        assert!(coverage.missing_chapters.contains(&2));
        assert_eq!(coverage.calls_used, 44);
    }

    #[test]
    fn explicit_recovery_keeps_attempted_targets_and_does_not_reset_book_counters() {
        let conn = job_database();
        let book = sampled_book(&conn, 3, "Original prose");
        repo::set_extraction_plan(&conn, book, &[1, 3], 44, 470_000).unwrap();
        let prior = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, json!({"graph_format":"kg-lite/v1","attempted_chapters":[1]})).unwrap();
        jobs::mark_failed(&conn, prior, "request failed").unwrap();
        let attempted = prior_book_attempts(&conn, book).unwrap();
        assert_eq!(attempted, vec![1]);
        let recovery = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, json!({"graph_format":"kg-lite/v1","attempted_chapters":attempted,"budget_started_ms":epoch_ms(),"budget_elapsed_base_ms":470_000})).unwrap();
        assert_ne!(recovery, prior);
        let payload = job_payload(&conn, recovery).unwrap();
        assert_eq!(payload["attempted_chapters"], json!([1]));
        let selected = fit_remaining_book_plan(&conn, book, recovery, &test_config(), &payload).unwrap();
        assert_eq!(selected, vec![1]);
        let coverage = repo::coverage(&conn, book).unwrap();
        assert_eq!(coverage.calls_used, 44);
        assert!(coverage.elapsed_ms >= 470_000);
        assert!(!coverage.target_complete);
    }

    #[test]
    fn foreground_preflight_failure_retains_target_without_charging_unsent_request() {
        let conn = job_database();
        let book = sampled_book(&conn, 3, "Original prose");
        repo::set_extraction_plan(&conn, book, &[1], 44, 100).unwrap();
        let payload = json!({"graph_format":"kg-lite/v1","budget_started_ms":epoch_ms(),"budget_elapsed_base_ms":100});
        let owner = jobs::enqueue(&conn, book, jobs::JobKind::GlobalExtract, payload).unwrap();
        fail_foreground_target(&conn, book, owner, 3, "whole chapter exceeds remaining call quota").unwrap();
        let coverage = repo::coverage(&conn, book).unwrap();
        assert_eq!(coverage.selected_chapters, vec![1, 3]);
        assert_eq!(coverage.calls_used, 44);
        assert!(!coverage.target_complete);
        assert_eq!(prior_book_attempts(&conn, book).unwrap(), vec![3]);
        assert_eq!(job_payload(&conn, owner).unwrap()["phase"], "failed");
        assert!(!jobs::is_active(&conn, owner).unwrap());
    }

    #[test]
    fn long_chapter_costs_reserve_first_and_last_before_uniform_samples() {
        let costs = vec![(1, 3), (2, 1), (3, 1), (4, 4), (5, 1), (6, 2)];
        assert_eq!(trim_plan(&costs, Some(3), 7), vec![1, 2, 3, 6]);
        assert_eq!(trim_plan(&costs, Some(3), 4), vec![1, 3]);
        assert_eq!(trim_plan(&[(1, 46), (2, 1), (3, 1)], None, 45), vec![2, 3]);
    }

    #[test]
    fn cached_chapters_cost_no_requests_and_remain_in_selected_plan() {
        assert_eq!(trim_plan(&[(1, 0), (2, 8), (3, 0)], None, 0), vec![1, 3]);
    }

    #[test]
    fn original_wait_keeps_request_timeout_and_generated_wait_has_no_480_second_cutoff() {
        let limit = crate::core::kg_extract::REQUEST_TIMEOUT;
        assert_eq!(selected_remaining(true, limit.as_millis() as u64).unwrap(), limit);
        let generated = selected_remaining(false, 480_000).unwrap();
        assert!(generated > std::time::Duration::from_secs(480));
    }

    #[test]
    fn restart_and_queue_wait_record_elapsed_without_rejecting_at_480_seconds() {
        let payload = json!({"budget_started_ms": 1000, "budget_elapsed_base_ms": 400_000});
        assert_eq!(book_elapsed(&payload, 400_500, 11_000), 410_000);
        assert_eq!(book_elapsed(&payload, 470_000, 11_000), 470_000);
        assert!(book_remaining(480_000).is_ok());
    }

    #[test]
    fn legacy_full_book_jobs_are_terminal_instead_of_resuming_paid_backfill() {
        let conn = job_database();
        let legacy = jobs::enqueue(&conn, 1, jobs::JobKind::GlobalExtract, json!({"committed_chapters":[1]})).unwrap();
        retire_legacy_book_jobs(&conn).unwrap();
        assert!(!jobs::is_active(&conn, legacy).unwrap());
        let payload: String = conn.query_row("SELECT payload_json FROM jobs WHERE id=?1", [legacy], |r| r.get(0)).unwrap();
        assert_eq!(serde_json::from_str::<Value>(&payload).unwrap()["committed_chapters"], json!([1]));
        let lite = jobs::enqueue(&conn, 1, jobs::JobKind::GlobalExtract, json!({"graph_format":"kg-lite/v1"})).unwrap();
        retire_legacy_book_jobs(&conn).unwrap();
        assert!(jobs::is_active(&conn, lite).unwrap());
    }

    #[tokio::test]
    async fn book_deadline_bounds_queued_permit_without_releasing_holder() {
        let semaphore = tokio::sync::Semaphore::new(1);
        let holder = semaphore.acquire().await.unwrap();
        let token = CancellationToken::new();
        let deadline = tokio::time::Instant::now();
        assert!(bounded(&token, deadline, semaphore.acquire()).await.is_err());
        assert_eq!(semaphore.available_permits(), 0);
        drop(holder);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[test]
    fn blocked_prefetch_releases_only_its_interest_in_shared_successor() {
        let conn = job_database();
        let id = jobs::enqueue(&conn, 1, jobs::JobKind::Generate, json!({"chapter_idx":3})).unwrap();
        let control = CallControl::default();
        let mut active = HashMap::from([(id, ActiveJob { book_id: 1, control: control.clone(), users: 2 })]);
        let mut blocked = RunningJob { id, control: control.clone(), finished: false };
        let mut foreground = RunningJob { id, control, finished: false };
        blocked.finish_blocked(&mut active, &conn, 2, "HTTP 504").unwrap();
        assert!(jobs::is_active(&conn, id).unwrap());
        assert_eq!(active.get(&id).unwrap().users, 1);
        assert!(foreground.control.check().is_ok());
        assert!(blocked.finished);
        assert_eq!(foreground.finish(&mut active), 0);
        jobs::mark_done(&conn, id).unwrap();
        let state: String = conn.query_row("SELECT state FROM jobs WHERE id=?1", [id], |row| row.get(0)).unwrap();
        assert_eq!(state, "done");
    }

    #[test]
    fn solo_blocked_successor_is_failed_with_checkpoint_intact() {
        let conn = job_database();
        let id = jobs::enqueue(&conn, 1, jobs::JobKind::Generate,
            json!({"chapter_idx":3,"committed_revisions":{"2":{"revision":4}}})).unwrap();
        let control = CallControl::default();
        let mut active = HashMap::from([(id, ActiveJob { book_id: 1, control: control.clone(), users: 1 })]);
        let mut blocked = RunningJob { id, control, finished: false };
        blocked.finish_blocked(&mut active, &conn, 2, "HTTP 504").unwrap();
        assert!(!jobs::is_active(&conn, id).unwrap());
        assert!(!active.contains_key(&id));
        assert!(blocked.finished);
        let (state, payload, error): (String, String, String) = conn.query_row(
            "SELECT state,payload_json,error FROM jobs WHERE id=?1", [id], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?))).unwrap();
        assert_eq!(state, "failed");
        assert!(error.contains("第2章"));
        assert!(error.contains("504"));
        let payload: Value = serde_json::from_str(&payload).unwrap();
        assert_eq!(payload["committed_revisions"]["2"]["revision"], 4);
    }

    #[test]
    fn overlapping_successful_callers_settle_the_job_exactly_once() {
        let id = -2;
        let control = CallControl::default();
        let mut active = HashMap::from([(id, ActiveJob { book_id: -2, control: control.clone(), users: 2 })]);
        let mut first = RunningJob { id, control: control.clone(), finished: false };
        let mut second = RunningJob { id, control, finished: false };
        assert_eq!(first.finish(&mut active), 1);
        assert_eq!(second.finish(&mut active), 0);
        assert!(!active.contains_key(&id));
        drop(first);
        drop(second);
    }

    #[test]
    fn foreground_callers_keep_shared_cancellation_registered_until_last_exit() {
        let id = -1;
        let control = CallControl::default();
        RUNNING_JOBS.lock().insert(id, ActiveJob { book_id: -1, control: control.clone(), users: 2 });
        let first = RunningJob { id, control: control.clone(), finished: false };
        let second = RunningJob { id, control: control.clone(), finished: false };
        drop(first);
        {
            let active = RUNNING_JOBS.lock();
            let registered = active.get(&id).expect("a queued caller must retain cancellation registration");
            assert_eq!(registered.users, 1);
            registered.control.cancel();
        }
        assert!(second.control.check().is_err());
        drop(second);
        assert!(!RUNNING_JOBS.lock().contains_key(&id));
    }

    #[tokio::test]
    async fn queued_book_lock_wait_is_cancellable_without_releasing_holder() {
        let gate = tokio::sync::Mutex::new(());
        let holder = gate.lock().await;
        let token = CancellationToken::new();
        let wait = cancellable(&token, gate.lock());
        tokio::pin!(wait);
        assert!(matches!(futures_util::poll!(&mut wait), std::task::Poll::Pending));
        token.cancel();
        assert!(wait.await.is_err());
        drop(holder);
        assert!(gate.try_lock().is_ok());
    }

    #[tokio::test]
    async fn queued_provider_permit_wait_is_cancellable() {
        let semaphore = tokio::sync::Semaphore::new(1);
        let holder = semaphore.acquire().await.unwrap();
        let token = CancellationToken::new();
        let wait = cancellable(&token, semaphore.acquire());
        tokio::pin!(wait);
        assert!(matches!(futures_util::poll!(&mut wait), std::task::Poll::Pending));
        token.cancel();
        assert!(wait.await.is_err());
        assert_eq!(semaphore.available_permits(), 0);
        drop(holder);
        assert_eq!(semaphore.available_permits(), 1);
    }

    #[tokio::test]
    async fn cancellation_wins_over_ready_stage_and_drops_pending_extraction() {
        let token = CancellationToken::new();
        token.cancel();
        assert!(cancellable(&token, std::future::ready(())).await.is_err());
        let token = CancellationToken::new();
        let wait = cancellable(&token, std::future::pending::<()>());
        tokio::pin!(wait);
        assert!(matches!(futures_util::poll!(&mut wait), std::task::Poll::Pending));
        token.cancel();
        assert!(wait.await.is_err());
    }
}
