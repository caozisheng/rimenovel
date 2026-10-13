// RimeNovel — Rust core 入口
// 模块地图（见 docs/rimenovel-overall-design.md §12）：
//   commands/  IPC handlers（薄层）
//   core/      领域纯逻辑：分章、图谱、overlay、阅读线、上下文组装、调和
//   llm/       provider、openai/anthropic 协议、schema、SSE
//   jobs/      任务队列与状态机
//   store/     rusqlite + 迁移
//   creds/     OS 安全存储

pub mod commands;
pub mod core;
pub mod creds;
pub mod jobs;
pub mod llm;
pub mod web_search;
pub mod store;

use commands::AppState;
use parking_lot::Mutex;
use rusqlite::Connection;
use tauri::Manager;

fn init_db(dir: &std::path::Path) -> Result<Connection, Box<dyn std::error::Error>> {
    std::fs::create_dir_all(&dir)?;
    let conn = Connection::open(dir.join("rimenovel.db"))?;
    store::Db::apply_pragmas(&conn)?;
    store::Db::new(&conn).migrate()?;
    Ok(conn)
}

pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            db: Mutex::new(None),
        })
        .setup(|app| {
            let dir = match std::env::var_os("RIMENOVEL_DATA_DIR") {
                Some(path) => std::path::PathBuf::from(path),
                None => app.path().app_data_dir()?,
            };
            std::fs::create_dir_all(&dir)?;
            // Preserve pre-cutover local data once, without overwriting an
            // installed database or discarding SQLite's committed WAL pages.
            let legacy = std::env::current_dir()?.join("data");
            let target = dir.join("rimenovel.db");
            if !target.exists() && legacy.join("rimenovel.db").exists() && legacy != dir {
                let source = Connection::open(legacy.join("rimenovel.db"))?;
                source.execute("VACUUM INTO ?1", [target.to_string_lossy().as_ref()])?;
                let old_creds = legacy.join("creds.json");
                if old_creds.exists() && !dir.join("creds.json").exists() {
                    std::fs::copy(old_creds, dir.join("creds.json"))?;
                }
            }
            creds::configure_directory(dir.clone()).map_err(std::io::Error::other)?;
            *app.state::<AppState>().db.lock() = Some(init_db(&dir).map_err(|e| std::io::Error::other(e.to_string()))?);
            commands::kg::resume_jobs(app.handle()).map_err(std::io::Error::other)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::import_book,
            commands::delete_book_cmd,
            commands::list_books_cmd,
            commands::get_chapter_titles,
            commands::get_chapter_cmd,
            commands::providers::provider_save,
            commands::providers::provider_list,
            commands::providers::provider_delete,
            commands::providers::provider_test,
            commands::status::get_extraction_statuses,
            commands::quick_read::start_quick_read,
            commands::kg::cancel_book_jobs,
            commands::story::get_story_map,
            commands::story::save_intervention,
            commands::story::end_questionnaire,
            commands::story::get_active_interventions,
            commands::story::get_question_batch,
            commands::story::get_next_wiki
        ])
        .run(tauri::generate_context!())
        .expect("error while running rimenovel application");
}
