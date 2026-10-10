// Provider 管理 commands（dev-plan Task 3.5）
// API Key 落 creds vault（P8 换 OS keyring/Keystore）；DB 只存 key_ref。

use crate::commands::AppState;
use crate::llm::{ProviderConfig, StructuredRequest, TaskKind};
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Debug, Clone, Serialize)]
pub struct ProviderRow {
    pub id: i64,
    pub name: String,
    pub protocol: String,
    pub base_url: String,
    pub model_extract: Option<String>,
    pub model_write: Option<String>,
    pub model_merge: Option<String>,
    pub context_limit: Option<usize>,
    pub output_reserve: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ProviderInput {
    pub name: String,
    pub protocol: String, // openai|anthropic
    pub base_url: String,
    pub model_extract: Option<String>,
    pub model_write: Option<String>,
    pub model_merge: Option<String>,
    pub api_key: String,
    pub context_limit: Option<usize>,
    pub output_reserve: Option<usize>,
}

fn row_from(conn: &Connection, id: i64) -> rusqlite::Result<Option<ProviderRow>> {
    conn.query_row(
        "SELECT id, name, protocol, base_url, model_extract, model_write, model_merge,
         json_extract(params_json,'$.context_limit'),json_extract(params_json,'$.output_reserve')
         FROM llm_providers WHERE id = ?1",
        [id],
        |r| {
            Ok(ProviderRow {
                id: r.get(0)?,
                name: r.get(1)?,
                protocol: r.get(2)?,
                base_url: r.get(3)?,
                model_extract: r.get(4)?,
                model_write: r.get(5)?,
                model_merge: r.get(6)?,
                context_limit: r.get(7)?,
                output_reserve: r.get(8)?,
            })
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

pub(crate) fn load_config(conn: &Connection, id: i64) -> Result<ProviderConfig, String> {
    let row = row_from(conn, id).map_err(|e| e.to_string())?;
    let row = row.ok_or("provider 不存在")?;
    let key_ref: String = conn
        .query_row(
            "SELECT key_ref FROM llm_providers WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .map_err(|e| e.to_string())?;
    let api_key = crate::creds::load(&key_ref).map_err(|e| format!("读取凭据失败: {e}"))?;
    Ok(ProviderConfig {
        id: row.id,
        name: row.name,
        protocol: row.protocol,
        base_url: row.base_url,
        model_extract: row.model_extract,
        model_write: row.model_write,
        model_merge: row.model_merge,
        api_key,
    })
}

/// 按协议构造 client
pub fn make_client(cfg: ProviderConfig) -> Result<Box<dyn crate::llm::LlmClient>, String> {
    match cfg.protocol.as_str() {
        "openai" => Ok(Box::new(crate::llm::openai::OpenAiClient {
            cfg,
            http: reqwest::Client::new(),
        })),
        "anthropic" => Ok(Box::new(crate::llm::anthropic::AnthropicClient {
            cfg,
            http: reqwest::Client::new(),
        })),
        other => Err(format!("未知协议: {other}")),
    }
}

// ── 内部函数（业务，可单测）─────────────────────────────────────

fn save_provider(conn: &Connection, input: &ProviderInput) -> Result<i64, String> {
    if !matches!(input.protocol.as_str(), "openai" | "anthropic") {
        return Err("protocol 必须是 openai|anthropic".into());
    }
    if input.base_url.is_empty() {
        return Err("base_url 不能为空".into());
    }
    let limit = input.context_limit.unwrap_or(32768);
    let reserve = input.output_reserve.unwrap_or(4096);
    if limit == 0 || reserve == 0 || reserve >= limit {
        return Err("上下文预算必须为正数，且输出预留小于上下文预算".into());
    }
    let settings = json!({"context_limit":limit,"output_reserve":reserve}).to_string();
    let key_ref = format!("provider-key-{}", input.name);
    crate::creds::save(&key_ref, &input.api_key).map_err(|e| e.to_string())?;
    conn.execute(
        "INSERT OR REPLACE INTO llm_providers (id, name, protocol, base_url, model_extract, model_write, model_merge, key_ref, params_json)
         VALUES (
           (SELECT id FROM llm_providers WHERE name = ?1), ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            input.name,
            input.protocol,
            input.base_url,
            input.model_extract,
            input.model_write,
            input.model_merge,
            key_ref,
            settings
        ],
    )
    .map_err(|e| e.to_string())?;
    conn.query_row(
        "SELECT id FROM llm_providers WHERE name = ?1",
        [&input.name],
        |r| r.get(0),
    )
    .map_err(|e| e.to_string())
}

fn list_providers(conn: &Connection) -> Result<Vec<ProviderRow>, String> {
    let mut stmt = conn
        .prepare("SELECT id, name, protocol, base_url, model_extract, model_write, model_merge, json_extract(params_json,'$.context_limit'), json_extract(params_json,'$.output_reserve') FROM llm_providers ORDER BY id")
        .map_err(|e| e.to_string())?;
    let rows = stmt
        .query_map([], |r| {
            Ok(ProviderRow {
                id: r.get(0)?,
                name: r.get(1)?,
                protocol: r.get(2)?,
                base_url: r.get(3)?,
                model_extract: r.get(4)?,
                model_write: r.get(5)?,
                model_merge: r.get(6)?,
                context_limit: r.get(7)?,
                output_reserve: r.get(8)?,
            })
        })
        .map_err(|e| e.to_string())?;
    rows.collect::<Result<Vec<_>, _>>()
        .map_err(|e| e.to_string())
}

// ── Tauri commands（薄层）───────────────────────────────────────

#[tauri::command]
pub fn provider_save(state: tauri::State<AppState>, input: ProviderInput) -> Result<i64, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    save_provider(conn, &input)
}

#[tauri::command]
pub fn provider_list(state: tauri::State<AppState>) -> Result<Vec<ProviderRow>, String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    list_providers(conn)
}

#[tauri::command]
pub fn provider_delete(state: tauri::State<AppState>, id: i64) -> Result<(), String> {
    let guard = state.db.lock();
    let conn = guard.as_ref().ok_or("数据库未初始化")?;
    let key_ref: Option<String> = conn
        .query_row(
            "SELECT key_ref FROM llm_providers WHERE id = ?1",
            [id],
            |r| r.get(0),
        )
        .ok();
    conn.execute("DELETE FROM llm_providers WHERE id = ?1", [id])
        .map_err(|e| e.to_string())?;
    if let Some(kr) = key_ref {
        let _ = crate::creds::delete(&kr);
    }
    Ok(())
}

/// 测连: extract 模型发一次最小结构化请求(bool schema)
#[tauri::command]
pub async fn provider_test(state: tauri::State<'_, AppState>, id: i64) -> Result<String, String> {
    let cfg = {
        let guard = state.db.lock();
        let conn = guard.as_ref().ok_or("数据库未初始化")?;
        load_config(conn, id)?
    };
    let schema = json!({
        "type": "object",
        "additionalProperties": false,
        "required": ["ok"],
        "properties": {"ok": {"type": "boolean"}}
    });
    let req = StructuredRequest {
        task: TaskKind::Extract,
        messages: vec![crate::llm::ChatMessage {
            role: "user".into(),
            content: "连接测试: 返回 {\"ok\": true}".into(),
        }],
        schema: &schema,
        schema_name: "conn_test",
        temperature: 0.0,
    };
    let client = make_client(cfg)?;
    match client.structured(req).await {
        Ok(_) => Ok("连接成功".into()),
        Err(e) => Err(format!("连接失败: {e}")),
    }
}

#[cfg(test)]
pub(crate) fn test_config_loader(conn: &Connection, id: i64) -> Result<ProviderConfig, String> {
    load_config(conn, id)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::apply_pragmas(&conn).unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn
    }

    fn sample(name: &str) -> ProviderInput {
        ProviderInput {
            name: name.into(),
            protocol: "openai".into(),
            base_url: "https://api.example.com/v1".into(),
            model_extract: Some("m-extract".into()),
            model_write: Some("m-write".into()),
            model_merge: Some("m-merge".into()),
            api_key: "sk-test".into(),
            context_limit: Some(32768),
            output_reserve: Some(4096),
        }
    }

    #[test]
    fn save_list_delete_roundtrip() {
        let conn = db();
        save_provider(&conn, &sample("p1")).unwrap();
        // 同名再存 = 更新不新增
        save_provider(&conn, &sample("p1")).unwrap();
        assert_eq!(list_providers(&conn).unwrap().len(), 1);
        let id = list_providers(&conn).unwrap()[0].id;
        // load_config 能解析 key
        let cfg = test_config_loader(&conn, id).unwrap();
        assert_eq!(cfg.api_key, "sk-test");
    }

    #[test]
    fn rejects_bad_protocol() {
        let conn = db();
        let mut bad = sample("bad");
        bad.protocol = "grpc".into();
        assert!(save_provider(&conn, &bad).is_err());
    }

    #[test]
    fn invalid_budget_does_not_replace_live_provider() {
        let conn = db();
        let input = sample("budget-validation");
        let id = save_provider(&conn, &input).unwrap();
        let mut invalid = input;
        invalid.context_limit = Some(8192);
        invalid.output_reserve = Some(8192);
        invalid.base_url = "https://should-not-replace.invalid".into();
        assert!(save_provider(&conn, &invalid).is_err());
        let stored = row_from(&conn, id).unwrap().unwrap();
        assert_eq!(stored.base_url, "https://api.example.com/v1");
        assert_eq!(stored.context_limit, Some(32768));
        invalid.output_reserve = Some(0);
        assert!(save_provider(&conn, &invalid).is_err());
    }
}
