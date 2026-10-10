//! Real KG IPC and book-scoped execution. SQLite locks never span provider awaits.
use super::{providers, AppState};
use crate::core::kg::{EditInput, EditRecord, EvidenceView, GraphQuery, GraphView, RuntimeConfig, TextVersion};
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
static PROVIDER_LIMIT: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(2));

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

fn require_active(conn: &Connection, running: &RunningJob) -> Result<(), String> {
    running.control.check().map_err(|e| e.to_string())?;
    if !jobs::is_active(conn, running.id).map_err(|e| e.to_string())? {
        return Err("任务已结束或取消".into());
    }
    Ok(())
}

#[tauri::command]
pub fn cancel_book_jobs(app: tauri::AppHandle, state: tauri::State<AppState>, book_id: i64) -> Result<(), String> {
    let active = RUNNING_JOBS.lock();
    for job in active.values().filter(|job| job.book_id == book_id) { job.control.cancel(); }
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
    Ok((cfg, RuntimeConfig { provider_id: id, model_extract, context_limit, output_reserve, prompt_version: "kg-v2-sdk-policy-v1".into() }))
}

#[tauri::command]
pub fn get_graph(state: tauri::State<AppState>, query: GraphQuery) -> Result<GraphView, String> {
    let guard = state.db.lock();
    repo::get_view(guard.as_ref().ok_or("数据库未初始化")?, &query)
}

#[tauri::command]
pub fn get_graph_evidence(state: tauri::State<AppState>, query: GraphQuery, kind: String, key: String) -> Result<Vec<EvidenceView>, String> {
    let guard = state.db.lock();
    repo::evidence(guard.as_ref().ok_or("数据库未初始化")?, &query, &kind, &key)
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

/// Queue and start full original extraction. Repeated calls share the same active job.
pub fn schedule_book(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>) -> Result<i64, String> {
    let id = {
        let _active = RUNNING_JOBS.lock();
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE id=?1)", [book_id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !exists { return Err("书籍不存在".into()); }
        let id = jobs::enqueue(conn, book_id, jobs::JobKind::GlobalExtract, json!({"provider_id":provider_id,"committed_chapters":[],"phase":"queued","origin":"explicit"})).map_err(|e| e.to_string())?;
        jobs::mark_explicit(conn, id).map_err(|e| e.to_string())?;
        id
    };
    spawn_book_job(app, book_id, id, provider_id);
    Ok(id)
}

#[tauri::command]
pub fn start_book_extraction(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>) -> Result<i64, String> {
    schedule_book(app, book_id, provider_id)
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
        let result = run_book_job(&app, book_id, &running, provider_id).await;
        let state = app.state::<AppState>();
        let _ = finish_job(&app, &state, &mut running, &result);
    });
}

async fn run_book_job(app: &tauri::AppHandle, book_id: i64, running: &RunningJob, provider_id: Option<i64>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let token = &running.control.cancellation;
    let (cfg, config, total) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        require_active(conn, running)?;
        jobs::mark_running(conn, running.id).map_err(|e| e.to_string())?;
        let (cfg, config) = runtime_config(conn, provider_id)?;
        let total = crate::store::books::count_chapters(conn, book_id).map_err(|e| e.to_string())?;
        (cfg, config, total)
    };
    let client = providers::make_client_with_control(cfg, running.control.clone())?;
    loop {
        // Release both permits after each committed chapter, preserving ordered
        // originals and allowing foreground readers into the same-book FIFO gate.
        job_phase(app, running.id, "book_wait", None, None, None);
        let gate = book_gate(book_id);
        let _book = cancellable(token, gate.lock()).await?;
        let (next, mut committed) = {
            let _commit = client.commit_guard().map_err(|e| e.to_string())?;
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, running)?;
            jobs::ensure_prefetch_allowed(conn, running.id)?;
            let mut next = None;
            let mut committed = Vec::new();
            for idx in 1..=total {
                running.control.check().map_err(|e| e.to_string())?;
                let text = repo::text_version(conn, book_id, idx, "original", None)?;
                if repo::extraction_matches(conn, text.id, &config)? { committed.push(idx); }
                else if next.is_none() { next = Some(idx); }
            }
            (next, committed)
        };
        let Some(idx) = next else { break; };
        job_phase(app, running.id, "provider_wait", None, None, Some(idx));
        let _permit = cancellable(token, PROVIDER_LIMIT.acquire()).await?.map_err(|e| e.to_string())?;
        running.control.check().map_err(|e| e.to_string())?;
        let text = {
            let _commit = client.commit_guard().map_err(|e| e.to_string())?;
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, running)?;
            repo::text_version(conn, book_id, idx, "original", None)?
        };
        job_phase(app, running.id, "chapter", None, None, Some(idx));
        cancellable(token, crate::core::kg_extract::extract(&state, client.as_ref(), text, &config, Some(running.id))).await??;
        {
            let _commit = client.commit_guard().map_err(|e| e.to_string())?;
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, running)?;
            committed.push(idx);
            committed.sort_unstable();
            let progress = if total == 0 { 1.0 } else { committed.len() as f64 / total as f64 };
            jobs::set_progress(conn, running.id, progress, &json!({"committed_chapters":committed})).map_err(|e| e.to_string())?;
        }
        notify_job(app, running.id);
        let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":idx}));
    }
    job_phase(app, running.id, "book_wait", None, None, None);
    let gate = book_gate(book_id);
    let _book = cancellable(token, gate.lock()).await?;
    job_phase(app, running.id, "provider_wait", None, None, None);
    let _permit = cancellable(token, PROVIDER_LIMIT.acquire()).await?.map_err(|e| e.to_string())?;
    job_phase(app, running.id, "reduce", None, None, None);
    cancellable(token, crate::core::kg_extract::reduce_book(&state, client.as_ref(), &config, book_id)).await??;
    running.control.check().map_err(|e| e.to_string())?;
    Ok(())
}

pub fn resume_jobs(app: &tauri::AppHandle) -> Result<(), String> {
    let queued = {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
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

fn enqueue_selected(active: &mut HashMap<i64, ActiveJob>, app: &tauri::AppHandle, conn: &Connection, book_id: i64, idx: i64, provider_id: Option<i64>) -> Result<RunningJob, String> {
    let generated = repo::divergence(conn, book_id)?.is_some_and(|from| idx >= from);
    let kind = if generated { jobs::JobKind::Generate } else { jobs::JobKind::ChapterExtract };
    let id = jobs::enqueue(conn, book_id, kind, json!({"chapter_idx":idx,"provider_id":provider_id,"phase":"queued","origin":"explicit"})).map_err(|e| e.to_string())?;
    jobs::mark_explicit(conn, id).map_err(|e| e.to_string())?;
    Ok(register_job(active, app, book_id, id))
}

fn enqueue_selected_prefetch(active: &mut HashMap<i64, ActiveJob>, app: &tauri::AppHandle, conn: &Connection, book_id: i64, idx: i64, provider_id: Option<i64>) -> Result<Option<RunningJob>, String> {
    let generated = repo::divergence(conn, book_id)?.is_some_and(|from| idx >= from);
    let kind = if generated { jobs::JobKind::Generate } else { jobs::JobKind::ChapterExtract };
    let id = jobs::enqueue_prefetch(conn, book_id, kind, json!({"chapter_idx":idx,"provider_id":provider_id,"phase":"queued"})).map_err(|e| e.to_string())?;
    Ok(id.map(|id| register_job(active, app, book_id, id)))
}

async fn run_selected(app: &tauri::AppHandle, state: &AppState, book_id: i64, idx: i64, provider_id: Option<i64>, mut running: RunningJob) -> Result<TextVersion, String> {
    let result: Result<TextVersion, String> = async {
        let token = &running.control.cancellation;
        job_phase(app, running.id, "book_wait", None, None, Some(idx));
        let gate = book_gate(book_id);
        let _book = cancellable(token, gate.lock()).await?;
        let (cfg, config) = {
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            require_active(conn, &running)?;
            jobs::ensure_prefetch_allowed(conn, running.id)?;
            jobs::mark_running(conn, running.id).map_err(|e| e.to_string())?;
            runtime_config(conn, provider_id)?
        };
        let client = providers::make_client_with_control(cfg.clone(), running.control.clone())?;
        job_phase(app, running.id, "provider_wait", None, None, Some(idx));
        let _permit = cancellable(token, PROVIDER_LIMIT.acquire()).await?.map_err(|e| e.to_string())?;
        running.control.check().map_err(|e| e.to_string())?;
        let app_delta = app.clone();
        let delta_token = token.clone();
        let delta: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |text| {
            if !delta_token.is_cancelled() {
                let _ = app_delta.emit("chapter:delta", json!({"book_id":book_id,"chapter_idx":idx,"delta":text}));
            }
        });
        job_phase(app, running.id, "chapter", None, None, Some(idx));
        let text = cancellable(token, crate::core::kg_generate::ensure_chapter(state, client.as_ref(), &cfg, &config, book_id, idx, delta)).await??;
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
    async fn cancellation_wins_over_ready_stage_and_drops_pending_reduce() {
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
