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
pub mod store;

use commands::AppState;
use parking_lot::Mutex;
use rusqlite::Connection;

fn init_db() -> Result<Connection, Box<dyn std::error::Error>> {
    let dir = app_data_dir()?;
    std::fs::create_dir_all(&dir)?;
    let conn = Connection::open(dir.join("rimenovel.db"))?;
    store::Db::apply_pragmas(&conn)?;
    store::Db::new(&conn).migrate()?;
    Ok(conn)
}

fn app_data_dir() -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    // Tauri 运行期用 app.path().app_data_dir()；此处为测试/CLI 兜底
    Ok(std::env::current_dir()?.join("data"))
}
pub fn run() {
    let db = init_db().expect("数据库初始化失败");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_dialog::init())
        .manage(AppState {
            db: Mutex::new(Some(db)),
        })
        .invoke_handler(tauri::generate_handler![
            commands::import_book,
            commands::list_books_cmd,
            commands::get_chapter_titles,
            commands::get_chapter_cmd,
            commands::providers::provider_save,
            commands::providers::provider_list,
            commands::providers::provider_delete,
            commands::providers::provider_test
        ])
        .run(tauri::generate_context!())
        .expect("error while running rimenovel application");
}
