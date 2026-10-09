// 响应缓存 + 用量记账（dev-plan Task 3.4）
// 仅 TaskKind∈{Extract,Merge} 且 temperature==0 缓存（幂等可重放）；
// key = sha256(provider_id + model + messages + schema_name + params)。

use crate::llm::{StructuredRequest, TaskKind};
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};

pub struct CacheKey(pub String);

pub fn cache_key(provider_id: i64, model: &str, req: &StructuredRequest<'_>) -> Option<CacheKey> {
    // 仅确定性调用可缓存
    if req.temperature != 0.0 || matches!(req.task, TaskKind::Write) {
        return None;
    }
    let mut h = Sha256::new();
    h.update(provider_id.to_le_bytes());
    h.update(model.as_bytes());
    h.update(req.schema_name.as_bytes());
    h.update(req.temperature.to_le_bytes());
    for m in &req.messages {
        h.update(m.role.as_bytes());
        h.update([0u8]);
        h.update(m.content.as_bytes());
        h.update([0u8]);
    }
    let digest = h.finalize();
    let hex: String = digest.iter().map(|b| format!("{b:02x}")).collect();
    Some(CacheKey(hex))
}

pub fn cache_get(conn: &Connection, key: &CacheKey) -> rusqlite::Result<Option<Value>> {
    conn.query_row(
        "SELECT response_json FROM llm_cache WHERE hash = ?1",
        [&key.0],
        |r| r.get::<_, String>(0),
    )
    .map(|s| Some(serde_json::from_str(&s).expect("缓存内容必须合法 JSON")))
    .or_else(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        other => Err(other),
    })
}

pub fn cache_put(
    conn: &Connection,
    key: &CacheKey,
    response: &Value,
    tokens_in: i64,
    tokens_out: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT OR REPLACE INTO llm_cache (hash, response_json, tokens_in, tokens_out) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![key.0, serde_json::to_string(response).unwrap(), tokens_in, tokens_out],
    )?;
    Ok(())
}

pub fn record_usage(
    conn: &Connection,
    provider_id: i64,
    book_id: Option<i64>,
    task: TaskKind,
    tokens_in: i64,
    tokens_out: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO llm_usage (provider_id, book_id, task, tokens_in, tokens_out) VALUES (?1, ?2, ?3, ?4, ?5)",
        rusqlite::params![provider_id, book_id, task.as_str(), tokens_in, tokens_out],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::ChatMessage;
    use serde_json::json;

    fn db() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        crate::store::Db::apply_pragmas(&conn).unwrap();
        crate::store::Db::new(&conn).migrate().unwrap();
        conn
    }

    fn req(temp: f32, task: TaskKind) -> StructuredRequest<'static> {
        // 静态 schema 避免 lifetime 纠缠
        static SCHEMA: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| json!({"type": "object"}));
        StructuredRequest {
            task,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "相同输入".into(),
            }],
            schema: &SCHEMA,
            schema_name: "t",
            temperature: temp,
        }
    }

    #[test]
    fn deterministic_calls_are_cacheable() {
        assert!(cache_key(1, "m", &req(0.0, TaskKind::Extract)).is_some());
        assert!(cache_key(1, "m", &req(0.0, TaskKind::Merge)).is_some());
        // 非零温度 / write 不缓存
        assert!(cache_key(1, "m", &req(0.7, TaskKind::Extract)).is_none());
        assert!(cache_key(1, "m", &req(0.0, TaskKind::Write)).is_none());
    }

    #[test]
    fn same_input_same_key_different_input_differs() {
        let a = cache_key(1, "m", &req(0.0, TaskKind::Extract)).unwrap();
        let b = cache_key(1, "m", &req(0.0, TaskKind::Extract)).unwrap();
        assert_eq!(a.0, b.0);
        // 不同 provider/model → 不同 key
        let c = cache_key(2, "m", &req(0.0, TaskKind::Extract)).unwrap();
        assert_ne!(a.0, c.0);
    }

    #[test]
    fn cache_roundtrip_and_usage() {
        let conn = db();
        let key = cache_key(1, "m", &req(0.0, TaskKind::Extract)).unwrap();
        assert!(cache_get(&conn, &key).unwrap().is_none());
        cache_put(&conn, &key, &json!({"ok": true}), 100, 50).unwrap();
        assert_eq!(cache_get(&conn, &key).unwrap().unwrap()["ok"], true);
        record_usage(&conn, 1, Some(7), TaskKind::Extract, 100, 50).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM llm_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }
}
