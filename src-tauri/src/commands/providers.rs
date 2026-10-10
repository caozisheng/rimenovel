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
    pub id: Option<i64>,
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

/// Protocol adapters perform one attempt; shared policy owns retry and cancellation.
pub fn make_client(cfg: ProviderConfig) -> Result<Box<dyn crate::llm::LlmClient>, String> {
    make_client_with_control(cfg, crate::llm::policy::CallControl::default())
}

pub fn make_client_with_control(cfg: ProviderConfig, control: crate::llm::policy::CallControl) -> Result<Box<dyn crate::llm::LlmClient>, String> {
    let secret = cfg.api_key.clone();
    let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15))
        .retry(reqwest::retry::never()).read_timeout(std::time::Duration::from_secs(60))
        .timeout(std::time::Duration::from_secs(180)).build().map_err(|error| error.to_string())?;
    let adapter: Box<dyn crate::llm::LlmClient> = match cfg.protocol.as_str() {
        "openai" => Box::new(crate::llm::openai::OpenAiClient { cfg, http }),
        "anthropic" => Box::new(crate::llm::anthropic::AnthropicClient { cfg, http }),
        other => return Err(format!("未知协议: {other}")),
    };
    Ok(Box::new(crate::llm::policy::PolicyClient::new(adapter, control).with_secret(secret)))
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
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let key_ref = match input.id {
        Some(id) => tx.query_row("SELECT key_ref FROM llm_providers WHERE id=?1", [id], |r| r.get::<_, String>(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => "Provider 不存在，请重新选择".to_string(),
                other => other.to_string(),
            })?,
        None => format!("provider-key-{:032x}", rand::random::<u128>()),
    };
    let duplicate: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM llm_providers WHERE name=?1 AND (?2 IS NULL OR id<>?2))",
        params![input.name, input.id], |r| r.get(0),
    ).map_err(|e| e.to_string())?;
    if duplicate {
        return Err("名称已存在，请编辑已有 Provider 或使用其他名称".into());
    }
    let replace_key = !input.api_key.trim().is_empty();
    if !replace_key {
        if input.id.is_none() {
            return Err("新增 Provider 必须填写 API Key".into());
        }
        let existing = crate::creds::load(&key_ref)?;
        if existing.trim().is_empty() {
            return Err("已保存凭据为空，请输入 API Key".into());
        }
    }
    if let Some(id) = input.id {
        tx.execute(
            "UPDATE llm_providers SET name=?1, protocol=?2, base_url=?3, model_extract=?4,
             model_write=?5, model_merge=?6, params_json=?7 WHERE id=?8",
            params![input.name, input.protocol, input.base_url, input.model_extract,
                input.model_write, input.model_merge, settings, id],
        ).map_err(|e| e.to_string())?;
    } else {
        tx.execute(
            "INSERT INTO llm_providers (name, protocol, base_url, model_extract, model_write, model_merge, key_ref, params_json)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![input.name, input.protocol, input.base_url, input.model_extract,
                input.model_write, input.model_merge, key_ref, settings],
        ).map_err(|e| e.to_string())?;
    }
    let id = input.id.unwrap_or_else(|| tx.last_insert_rowid());
    let previous_key = if replace_key && input.id.is_some() { crate::creds::load(&key_ref).ok() } else { None };
    if replace_key {
        crate::creds::save(&key_ref, &input.api_key)?;
    }
    if let Err(error) = tx.commit() {
        if replace_key {
            let restored = match previous_key {
                Some(secret) => crate::creds::save(&key_ref, &secret),
                None => crate::creds::delete(&key_ref),
            };
            if let Err(restore_error) = restored {
                return Err(format!("保存失败: {error}; 凭据回滚失败: {restore_error}"));
            }
        }
        return Err(error.to_string());
    }
    Ok(id)
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
        max_tokens: 128,
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
            id: None,
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
    fn editing_saved_provider_updates_same_id_and_keeps_existing_key_when_blank() {
        let conn = db();
        let id = save_provider(&conn, &sample("keep-key-before")).unwrap();
        let mut edit = sample("keep-key-after");
        edit.id = Some(id);
        edit.api_key.clear();
        edit.base_url = "https://edited.example/v1".into();
        edit.model_write = Some("new-writer".into());

        assert_eq!(save_provider(&conn, &edit).unwrap(), id);
        let rows = list_providers(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].id, id);
        assert_eq!(rows[0].base_url, edit.base_url);
        assert_eq!(rows[0].model_write.as_deref(), Some("new-writer"));
        assert_eq!(test_config_loader(&conn, id).unwrap().api_key, "sk-test");
    }

    #[test]
    fn editing_provider_name_and_key_rotates_secret_without_returning_it() {
        let conn = db();
        let id = save_provider(&conn, &sample("rotate-key-before")).unwrap();
        let mut edit = sample("rotate-key-after");
        edit.id = Some(id);
        edit.api_key = "sk-rotated".into();

        assert_eq!(save_provider(&conn, &edit).unwrap(), id);
        assert_eq!(test_config_loader(&conn, id).unwrap().api_key, "sk-rotated");
        assert_eq!(list_providers(&conn).unwrap().iter().map(|row| row.id).collect::<Vec<_>>(), vec![id]);
        assert!(!serde_json::to_string(&list_providers(&conn).unwrap()).unwrap().contains("sk-rotated"));
    }
    #[test]
    fn editing_name_conflict_does_not_replace_metadata_or_credentials() {
        let conn = db();
        let id = save_provider(&conn, &sample("unchanged-provider")).unwrap();
        save_provider(&conn, &sample("other-provider")).unwrap();
        let mut edit = sample("other-provider");
        edit.id = Some(id);
        edit.api_key = "should-not-rotate".into();
        assert!(save_provider(&conn, &edit).is_err());
        let stored = test_config_loader(&conn, id).unwrap();
        assert_eq!(stored.name, "unchanged-provider");
        assert_eq!(stored.api_key, "sk-test");
    }

    #[test]
    fn invalid_edit_id_is_not_created_and_create_requires_a_secret() {
        let conn = db();
        let mut input = sample("missing-target");
        input.id = Some(999);
        assert!(save_provider(&conn, &input).is_err());
        input.id = None;
        input.api_key.clear();
        assert!(save_provider(&conn, &input).is_err());
        assert!(list_providers(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_renamed_provider_does_not_share_credentials_with_new_old_name() {
        let conn = db();
        let first = save_provider(&conn, &sample("reusable-name")).unwrap();
        let mut renamed = sample("renamed-provider");
        renamed.id = Some(first);
        renamed.api_key.clear();
        save_provider(&conn, &renamed).unwrap();
        let mut second = sample("reusable-name");
        second.api_key = "different-key".into();
        let second_id = save_provider(&conn, &second).unwrap();
        assert_ne!(first, second_id);
        assert_eq!(test_config_loader(&conn, first).unwrap().api_key, "sk-test");
        assert_eq!(test_config_loader(&conn, second_id).unwrap().api_key, "different-key");
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
        invalid.id = Some(id);
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
