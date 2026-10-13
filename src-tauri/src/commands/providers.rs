// Provider 管理 commands（dev-plan Task 3.5）
// API Key 落 creds vault（P8 换 OS keyring/Keystore）；DB 只存 key_ref。

use crate::commands::AppState;
use crate::llm::{ProviderConfig, StructuredRequest, TaskKind};
use rusqlite::{params, Connection, OptionalExtension};
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
    pub tavily_enabled: bool,
    pub has_tavily_key: bool,
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
    pub tavily_enabled: Option<bool>,
    #[serde(default)]
    pub tavily_api_key: String,
}

fn row_from(conn: &Connection, id: i64) -> rusqlite::Result<Option<ProviderRow>> {
    conn.query_row(
        "SELECT id, name, protocol, base_url, model_extract, model_write, model_merge,
         json_extract(params_json,'$.context_limit'),json_extract(params_json,'$.output_reserve'),
         COALESCE(json_extract(params_json,'$.tavily_enabled'),0),tavily_key_ref IS NOT NULL
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
                tavily_enabled: r.get::<_,i64>(9)? != 0,
                has_tavily_key: r.get(10)?,
            })
        },
    )
    .map(Some)
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

pub(crate) fn tavily_key(conn:&Connection,id:i64)->Result<Option<String>,String>{
    let row=row_from(conn,id).map_err(|_|"搜索配置读取失败".to_owned())?.ok_or("Provider 不存在")?;
    if !row.tavily_enabled{return Ok(None);}
    let reference:Option<String>=conn.query_row("SELECT tavily_key_ref FROM llm_providers WHERE id=?1",[id],|row|row.get(0)).map_err(|_|"搜索配置读取失败".to_owned())?;
    reference.map(|reference|crate::creds::load(&reference).map_err(|_|"Tavily 凭据不可用".to_owned())).transpose()
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
    // Reasoning may be silent before the first byte. Bound the whole attempt,
    // rather than aborting a valid slow response after a shorter idle interval.
    let http = reqwest::Client::builder().connect_timeout(std::time::Duration::from_secs(15))
        .retry(reqwest::retry::never())
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
    let settings = json!({"context_limit":limit,"output_reserve":reserve,"tavily_enabled":input.tavily_enabled.unwrap_or(false)}).to_string();
    let tx = conn.unchecked_transaction().map_err(|e| e.to_string())?;
    let key_ref = match input.id {
        Some(id) => tx.query_row("SELECT key_ref FROM llm_providers WHERE id=?1", [id], |r| r.get::<_, String>(0))
            .map_err(|e| match e {
                rusqlite::Error::QueryReturnedNoRows => "Provider 不存在，请重新选择".to_string(),
                other => other.to_string(),
            })?,
        None => format!("provider-key-{:032x}", rand::random::<u128>()),
    };
    let existing_tavily_key_ref: Option<String> = input.id.map(|id|tx.query_row("SELECT tavily_key_ref FROM llm_providers WHERE id=?1",[id],|row|row.get(0)).optional()).transpose().map_err(|e|e.to_string())?.flatten().flatten();
    let replace_tavily_key=!input.tavily_api_key.trim().is_empty();
    let tavily_key_ref=if replace_tavily_key {Some(existing_tavily_key_ref.unwrap_or_else(||format!("tavily-key-{:032x}",rand::random::<u128>())))} else {existing_tavily_key_ref};
    if input.tavily_enabled.unwrap_or(false) && tavily_key_ref.is_none(){return Err("启用联网概要需要 Tavily API Key".into());}
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
             model_write=?5, model_merge=?6, params_json=?7, tavily_key_ref=?8 WHERE id=?9",
            params![input.name, input.protocol, input.base_url, input.model_extract,
                input.model_write, input.model_merge, settings, tavily_key_ref, id],
        ).map_err(|e| e.to_string())?;
    } else {
        tx.execute(
            "INSERT INTO llm_providers (name, protocol, base_url, model_extract, model_write, model_merge, key_ref, params_json, tavily_key_ref)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![input.name, input.protocol, input.base_url, input.model_extract,
                input.model_write, input.model_merge, key_ref, settings, tavily_key_ref],
        ).map_err(|e| e.to_string())?;
    }
    let id = input.id.unwrap_or_else(|| tx.last_insert_rowid());
    let previous_key = if replace_key && input.id.is_some() { crate::creds::load(&key_ref).ok() } else { None };
    let previous_tavily_key = if replace_tavily_key { tavily_key_ref.as_deref().and_then(|reference|crate::creds::load(reference).ok()) } else { None };
    if replace_key { crate::creds::save(&key_ref, &input.api_key)?; }
    if replace_tavily_key {
        if let Err(error)=crate::creds::save(tavily_key_ref.as_deref().unwrap(), &input.tavily_api_key) {
            if replace_key {match &previous_key {Some(secret)=>crate::creds::save(&key_ref,secret)?,None=>crate::creds::delete(&key_ref)?};}
            return Err(error);
        }
    }
    if let Err(error) = tx.commit() {
        if replace_key {
            let restored = match previous_key { Some(secret) => crate::creds::save(&key_ref,&secret), None => crate::creds::delete(&key_ref) };
            if let Err(restore_error)=restored{return Err(format!("保存失败: {error}; LLM 凭据回滚失败: {restore_error}"));}
        }
        if replace_tavily_key {
            let restored=match (tavily_key_ref.as_deref(),previous_tavily_key){(Some(reference),Some(secret))=>crate::creds::save(reference,&secret),(Some(reference),None)=>crate::creds::delete(reference),(None,_)=>Ok(())};
            if let Err(restore_error)=restored{return Err(format!("保存失败: {error}; Tavily 凭据回滚失败: {restore_error}"));}
        }
        return Err(error.to_string());
    }
    Ok(id)
}

fn list_providers(conn: &Connection) -> Result<Vec<ProviderRow>, String> {
    let mut stmt = conn
        .prepare("SELECT id, name, protocol, base_url, model_extract, model_write, model_merge, json_extract(params_json,'$.context_limit'), json_extract(params_json,'$.output_reserve'), COALESCE(json_extract(params_json,'$.tavily_enabled'),0), tavily_key_ref IS NOT NULL FROM llm_providers ORDER BY id")
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
                tavily_enabled: r.get::<_,i64>(9)? != 0,
                has_tavily_key: r.get(10)?,
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
    delete_saved_provider(conn, id)
}

fn delete_saved_provider(conn: &Connection, id: i64) -> Result<(), String> {
    let tx=conn.unchecked_transaction().map_err(|error|error.to_string())?;
    let row:Option<(String,Option<String>)>=tx.query_row("SELECT key_ref,tavily_key_ref FROM llm_providers WHERE id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?))).optional().map_err(|error|error.to_string())?;
    let Some((llm_key,tavily_key))=row else {return Ok(());};
    let keys=[Some(llm_key),tavily_key];
    let previous=keys.each_ref().map(|reference|reference.as_deref().and_then(|reference|crate::creds::load(reference).ok()));
    let restore=|failure:String| {
        let mut message=failure;
        for (reference,secret) in keys.iter().zip(&previous) {
            if let (Some(reference),Some(secret))=(reference,secret) {
                if let Err(error)=crate::creds::save(reference,secret) {message.push_str(&format!("; 凭据回滚失败: {error}"));}
            }
        }
        message
    };
    tx.execute("DELETE FROM llm_providers WHERE id=?1",[id]).map_err(|error|error.to_string())?;
    for reference in keys.iter().flatten() {
        if let Err(error)=crate::creds::delete(reference) {return Err(restore(error));}
    }
    tx.commit().map_err(|error|restore(error.to_string()))
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
            tavily_enabled: Some(false),
            tavily_api_key: String::new(),
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

    #[tokio::test]
    async fn a_slow_first_response_can_complete_within_the_attempt_deadline() {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let (reply_tx, reply_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut reader = tokio::io::BufReader::new(stream);
            let mut content_length = 0;
            loop {
                let mut line = String::new();
                reader.read_line(&mut line).await.unwrap();
                if line == "\r\n" { break; }
                if let Some((name,value)) = line.split_once(':') {
                    if name.eq_ignore_ascii_case("content-length") { content_length = value.trim().parse::<usize>().unwrap(); }
                }
            }
            reader.read_exact(&mut vec![0;content_length]).await.unwrap();
            accepted_tx.send(()).unwrap();
            reply_rx.await.unwrap();
            let body = "data: {\"choices\":[{\"delta\":{\"content\":\"{\\\"summary\\\":\\\"抵达岳阳\\\"}\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n\n";
            let response = format!("HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len());
            let _ = reader.into_inner().write_all(response.as_bytes()).await;
        });
        let cfg = ProviderConfig {id:1,name:"slow-provider".into(),protocol:"openai".into(),base_url:format!("http://{address}"),model_extract:Some("extract".into()),model_write:None,model_merge:None,api_key:"test".into()};
        let client = make_client(cfg).unwrap();
        let request = tokio::spawn(async move {
            let schema = json!({"type":"object","additionalProperties":false,"required":["summary"],"properties":{"summary":{"type":"string"}}});
            client.structured(crate::llm::StructuredRequest {task:crate::llm::TaskKind::Extract,messages:vec![crate::llm::ChatMessage {role:"user".into(),content:"概括旅人到达岳阳的事件".into()}],schema:&schema,schema_name:"slow_extract",temperature:0.0,max_tokens:128}).await
        });
        accepted_rx.await.unwrap();
        tokio::time::pause();
        tokio::time::advance(std::time::Duration::from_secs(65)).await;
        reply_tx.send(()).unwrap();
        assert_eq!(request.await.unwrap().unwrap(),json!({"summary":"抵达岳阳"}));
        server.await.unwrap();
    }

    #[test]
    fn tavily_key_is_optional_secret_and_blank_edits_preserve_it() {
        let conn=db();let mut input=sample("web-provider");input.tavily_enabled=Some(false);input.tavily_api_key="tvly-private".into();
        let id=save_provider(&conn,&input).unwrap();
        assert!(tavily_key(&conn,id).unwrap().is_none());
        let mut enable=sample("web-provider");enable.id=Some(id);enable.api_key.clear();enable.tavily_api_key.clear();enable.tavily_enabled=Some(true);
        assert!(save_provider(&conn,&enable).is_ok());
        let rows=list_providers(&conn).unwrap();
        assert!(rows[0].has_tavily_key);assert!(rows[0].tavily_enabled);
        assert!(!serde_json::to_string(&rows).unwrap().contains("tvly-private"));
        assert_eq!(tavily_key(&conn,id).unwrap().as_deref(),Some("tvly-private"));
        assert_eq!(load_config(&conn,id).unwrap().api_key,"sk-test");
        enable.tavily_api_key="tvly-rotated".into();save_provider(&conn,&enable).unwrap();
        assert_eq!(tavily_key(&conn,id).unwrap().as_deref(),Some("tvly-rotated"));
        enable.tavily_enabled=Some(false);enable.tavily_api_key.clear();save_provider(&conn,&enable).unwrap();
        assert!(tavily_key(&conn,id).unwrap().is_none());
        assert!(list_providers(&conn).unwrap()[0].has_tavily_key);
        assert_eq!(load_config(&conn,id).unwrap().api_key,"sk-test");
    }

    #[test]
    fn enabling_tavily_without_key_does_not_modify_llm_provider(){
        let conn=db();let id=save_provider(&conn,&sample("no-search-key")).unwrap();
        let mut change=sample("no-search-key");change.id=Some(id);change.api_key.clear();change.tavily_enabled=Some(true);
        assert!(save_provider(&conn,&change).is_err());
        assert!(!list_providers(&conn).unwrap()[0].tavily_enabled);
        assert_eq!(load_config(&conn,id).unwrap().api_key,"sk-test");
    }

    #[test]
    fn credential_delete_failure_keeps_provider_retryable() {
        const CHILD: &str = "RIMENOVEL_PROVIDER_DELETE_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let unique=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
            let dir=std::env::temp_dir().join(format!("rimenovel-delete-{}-{unique}",std::process::id()));
            std::fs::create_dir_all(&dir).unwrap();
            let result=std::process::Command::new(std::env::current_exe().unwrap()).args(["--exact","commands::providers::tests::credential_delete_failure_keeps_provider_retryable","--nocapture"]).env(CHILD,"1").env("RIMENOVEL_DATA_DIR",&dir).output().unwrap();
            std::fs::remove_dir_all(&dir).unwrap();
            assert!(result.status.success(),"{}{}",String::from_utf8_lossy(&result.stdout),String::from_utf8_lossy(&result.stderr));
            return;
        }
        let conn=db();let mut input=sample("delete-retry");input.tavily_enabled=Some(true);input.tavily_api_key="tvly-delete".into();
        let id=save_provider(&conn,&input).unwrap();
        let refs:(String,String)=conn.query_row("SELECT key_ref,tavily_key_ref FROM llm_providers WHERE id=?1",[id],|row|Ok((row.get(0)?,row.get(1)?))).unwrap();
        let path=std::path::PathBuf::from(std::env::var_os("RIMENOVEL_DATA_DIR").unwrap()).join("creds.json");let backup=path.with_extension("backup");
        std::fs::rename(&path,&backup).unwrap();std::fs::create_dir(&path).unwrap();
        let failed=delete_saved_provider(&conn,id);
        std::fs::remove_dir(&path).unwrap();std::fs::rename(backup,path).unwrap();
        assert!(failed.is_err());
        assert_eq!(list_providers(&conn).unwrap().iter().map(|row|row.id).collect::<Vec<_>>(),vec![id]);
        assert_eq!(load_config(&conn,id).unwrap().api_key,"sk-test");
        assert_eq!(tavily_key(&conn,id).unwrap().as_deref(),Some("tvly-delete"));
        delete_saved_provider(&conn,id).unwrap();
        assert!(list_providers(&conn).unwrap().is_empty());
        assert!(crate::creds::load(&refs.0).is_err());assert!(crate::creds::load(&refs.1).is_err());
    }
}
