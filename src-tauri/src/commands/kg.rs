//! Real KG IPC and book-scoped execution. SQLite locks never span provider awaits.
use super::{providers, AppState};
use crate::core::kg::{EditInput, EditRecord, EvidenceView, GraphQuery, GraphView, RuntimeConfig, TextVersion};
use crate::llm::{ProviderConfig, TaskKind};
use crate::{jobs, store::kg as repo};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use tauri::{Emitter, Manager};

static BOOK_GATES: LazyLock<Mutex<HashMap<i64, Arc<tokio::sync::Mutex<()>>>>> = LazyLock::new(|| Mutex::new(HashMap::new()));
static RUNNING_JOBS: LazyLock<Mutex<HashSet<i64>>> = LazyLock::new(|| Mutex::new(HashSet::new()));
// Bounded cross-book work. The lock order is book gate, then global permit.
static PROVIDER_LIMIT: LazyLock<tokio::sync::Semaphore> = LazyLock::new(|| tokio::sync::Semaphore::new(2));

fn book_gate(book_id: i64) -> Arc<tokio::sync::Mutex<()>> {
    BOOK_GATES.lock().entry(book_id).or_insert_with(|| Arc::new(tokio::sync::Mutex::new(()))).clone()
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
    Ok((cfg, RuntimeConfig { provider_id: id, model_extract, context_limit, output_reserve, prompt_version: "kg-v2-2026-10".into() }))
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

fn notify_job(app: &tauri::AppHandle, book_id: i64, id: i64, progress: f64, status: &str, error: Option<&str>) {
    let _ = app.emit("job:progress", json!({"book_id":book_id,"job_id":id,"progress":progress,"state":status,"error":error}));
}

/// Queue and start full original extraction. Repeated calls share the same active job.
pub fn schedule_book(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>) -> Result<i64, String> {
    let id = {
        let state = app.state::<AppState>();
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let exists: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM books WHERE id=?1)", [book_id], |r| r.get(0)).map_err(|e| e.to_string())?;
        if !exists { return Err("书籍不存在".into()); }
        jobs::enqueue(conn, book_id, jobs::JobKind::GlobalExtract, json!({"provider_id":provider_id,"committed_chapters":[]})).map_err(|e| e.to_string())?
    };
    spawn_book_job(app, book_id, id, provider_id);
    Ok(id)
}

#[tauri::command]
pub fn start_book_extraction(app: tauri::AppHandle, book_id: i64, provider_id: Option<i64>) -> Result<i64, String> {
    schedule_book(app, book_id, provider_id)
}

struct RunningJob(i64);
impl Drop for RunningJob { fn drop(&mut self) { RUNNING_JOBS.lock().remove(&self.0); } }
fn spawn_book_job(app: tauri::AppHandle, book_id: i64, job_id: i64, provider_id: Option<i64>) {
    if !RUNNING_JOBS.lock().insert(job_id) { return; }
    tauri::async_runtime::spawn(async move {
        let _running = RunningJob(job_id);
        let result = run_book_job(&app, book_id, job_id, provider_id).await;
        if let Err(error) = result {
            let state = app.state::<AppState>();
            let guard = state.db.lock();
            if let Some(conn) = guard.as_ref() {
                let progress = conn.query_row("SELECT progress FROM jobs WHERE id=?1", [job_id], |r| r.get::<_, f64>(0)).optional().ok().flatten();
                if let Some(progress) = progress {
                    let _ = jobs::mark_failed(conn, job_id, &error);
                    notify_job(&app, book_id, job_id, progress, "failed", Some(&error));
                }
            }
        }
    });
}

async fn run_book_job(app: &tauri::AppHandle, book_id: i64, job_id: i64, provider_id: Option<i64>) -> Result<(), String> {
    let state = app.state::<AppState>();
    let (cfg, config, total) = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        jobs::mark_running(conn, job_id).map_err(|e| e.to_string())?;
        let (cfg, config) = runtime_config(conn, provider_id)?;
        let total = crate::store::books::count_chapters(conn, book_id).map_err(|e| e.to_string())?;
        (cfg, config, total)
    };
    let client = providers::make_client(cfg)?;
    loop {
        // Release the gate at every committed chapter so the reading request wins
        // before another background chapter starts. Previous original dependencies
        // stay ordered, while already committed first-chapter work is reused.
        let gate = book_gate(book_id);
        let _book = gate.lock().await;
        let next = {
            let guard = state.db.lock();
            let conn = guard.as_ref().ok_or("数据库未初始化")?;
            let mut next = None;
            for idx in 1..=total {
                let text = repo::text_version(conn, book_id, idx, "original", None)?;
                if !repo::extraction_matches(conn, text.id, &config)? {
                    next = Some(idx);
                    break;
                }
            }
            next
        };
        let Some(idx) = next else { break; };
        let _permit = PROVIDER_LIMIT.acquire().await.map_err(|e| e.to_string())?;
        let text = {
            let guard = state.db.lock();
            repo::text_version(guard.as_ref().ok_or("数据库未初始化")?, book_id, idx, "original", None)?
        };
        crate::core::kg_extract::extract(&state, client.as_ref(), text, &config, Some(job_id)).await?;
        let remaining = {
            let guard = state.db.lock();
            repo::missing_original_chapters(guard.as_ref().ok_or("数据库未初始化")?, book_id)?.len()
        };
        let progress = if total == 0 { 1.0 } else { (total - remaining as i64) as f64 / total as f64 };
        notify_job(app, book_id, job_id, progress, "running", None);
        let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":idx}));
    }
    {
        let _permit = PROVIDER_LIMIT.acquire().await.map_err(|e| e.to_string())?;
        crate::core::kg_extract::reduce_book(&state, client.as_ref(), &config, book_id).await?;
    }
    {
        let guard = state.db.lock();
        jobs::mark_done(guard.as_ref().ok_or("数据库未初始化")?, job_id).map_err(|e| e.to_string())?;
    }
    notify_job(app, book_id, job_id, 1.0, "done", None);
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
        if kind == "global_extract" {
            spawn_book_job(app.clone(), book_id, id, provider_id);
        } else if let Some(idx) = value.get("chapter_idx").and_then(Value::as_i64) {
            let app = app.clone();
            tauri::async_runtime::spawn(async move {
                let state = app.state::<AppState>();
                let result = ensure_selected(&app, &state, book_id, idx, provider_id).await;
                let guard = state.db.lock();
                if let Some(conn) = guard.as_ref() {
                    let _ = match result { Ok(_) => jobs::mark_done(conn, id), Err(error) => jobs::mark_failed(conn, id, &error) };
                }
            });
        }
    }
    Ok(())
}

pub async fn ensure_selected(app: &tauri::AppHandle, state: &AppState, book_id: i64, idx: i64, provider_id: Option<i64>) -> Result<TextVersion, String> {
    let gate = book_gate(book_id);
    let _book = gate.lock().await;
    let job_id = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        let generated = repo::divergence(conn, book_id)?.is_some_and(|from| idx >= from);
        let kind = if generated { jobs::JobKind::Generate } else { jobs::JobKind::ChapterExtract };
        let id = jobs::enqueue(conn, book_id, kind, json!({"chapter_idx":idx,"provider_id":provider_id}))
            .map_err(|e| e.to_string())?;
        jobs::mark_running(conn, id).map_err(|e| e.to_string())?;
        id
    };
    let result: Result<TextVersion, String> = async {
        let (cfg, config) = {
            let guard = state.db.lock();
            runtime_config(guard.as_ref().ok_or("数据库未初始化")?, provider_id)?
        };
        let client = providers::make_client(cfg.clone())?;
        let _permit = PROVIDER_LIMIT.acquire().await.map_err(|e| e.to_string())?;
        let app_delta = app.clone();
        let delta: Arc<dyn Fn(&str) + Send + Sync> = Arc::new(move |text| {
            let _ = app_delta.emit("chapter:delta", json!({"book_id":book_id,"chapter_idx":idx,"delta":text}));
        });
        crate::core::kg_generate::ensure_chapter(state, client.as_ref(), &cfg, &config, book_id, idx, delta).await
    }.await;
    {
        let guard = state.db.lock();
        if let Some(conn) = guard.as_ref() {
            match &result {
                Ok(_) => jobs::mark_done(conn, job_id),
                Err(error) => jobs::mark_failed(conn, job_id, error),
            }.map_err(|e| e.to_string())?;
        }
    }
    notify_job(app, book_id, job_id, if result.is_ok() { 1.0 } else { 0.0 }, if result.is_ok() { "done" } else { "failed" }, result.as_ref().err().map(String::as_str));
    let _ = app.emit("graph:changed", json!({"book_id":book_id,"chapter_idx":idx}));
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

/// Nonblocking reading prefetch; persist the failure and notify instead of hiding it.
pub fn prefetch_next(app: tauri::AppHandle, book_id: i64) {
    tauri::async_runtime::spawn(async move {
        let state = app.state::<AppState>();
        let indices = {
            let guard = state.db.lock();
            let Some(conn) = guard.as_ref() else { return; };
            match (repo::progress(conn, book_id), crate::store::books::count_chapters(conn, book_id)) {
                (Ok(p),Ok(total)) if p > 0 => (p..=(p+1).min(total)).collect::<Vec<_>>(),
                _ => return,
            }
        };
        for idx in indices {
            if let Err(error) = ensure_selected(&app, &state, book_id, idx, None).await {
                let _ = app.emit("job:progress",json!({"book_id":book_id,"chapter_idx":idx,"state":"failed","progress":0.0,"error":error}));
                break;
            }
        }
    });
}
