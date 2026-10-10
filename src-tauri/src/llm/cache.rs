// 响应缓存 + 用量记账（dev-plan Task 3.4）
// 仅 TaskKind∈{Extract,Merge} 且 temperature==0 缓存（幂等可重放）；
// key = sha256(provider_id + model + messages + schema_name + params)。

use crate::llm::{StructuredRequest, TaskKind};
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fmt::Write;

pub struct CacheKey(pub String);

const UNKNOWN_TOKEN_COUNT: i64 = -1;

fn update_part(h: &mut Sha256, bytes: &[u8]) {
    h.update((bytes.len() as u64).to_le_bytes());
    h.update(bytes);
}

pub fn cache_key(provider_id: i64, model: &str, req: &StructuredRequest<'_>) -> Option<CacheKey> {
    // 仅确定性调用可缓存
    if req.temperature != 0.0 || matches!(req.task, TaskKind::Write) {
        return None;
    }
    let mut h = Sha256::new();
    h.update(provider_id.to_le_bytes());
    update_part(&mut h, model.as_bytes());
    update_part(&mut h, req.task.as_str().as_bytes());
    update_part(&mut h, req.schema_name.as_bytes());
    update_part(&mut h, &req.temperature.to_le_bytes());
    // A schema name is not a schema identity: strict output cache reuse must
    // change when any nested shape, enum, or required field changes.
    update_part(&mut h, &serde_json::to_vec(req.schema).expect("schema is JSON"));
    for m in &req.messages {
        update_part(&mut h, m.role.as_bytes());
        update_part(&mut h, m.content.as_bytes());
    }
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest { write!(&mut hex, "{byte:02x}").expect("writing to String cannot fail"); }
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
    usage: Option<(i64, i64)>,
) -> rusqlite::Result<()> {
    let (tokens_in, tokens_out) = usage.unwrap_or((UNKNOWN_TOKEN_COUNT, UNKNOWN_TOKEN_COUNT));
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
    usage: Option<(i64, i64)>,
) -> rusqlite::Result<()> {
    let Some((tokens_in, tokens_out)) = usage else {
        return Ok(());
    };
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
    fn schema_contents_are_part_of_the_cache_key() {
        let base = req(0.0, TaskKind::Extract);
        let mut changed = base.clone();
        static OTHER_SCHEMA: std::sync::LazyLock<Value> =
            std::sync::LazyLock::new(|| json!({"type": "object", "required": ["answer"]}));
        changed.schema = &OTHER_SCHEMA;
        assert_ne!(
            cache_key(1, "m", &base).unwrap().0,
            cache_key(1, "m", &changed).unwrap().0,
            "schema name alone must not allow stale structured output reuse"
        );
    }

    #[test]
    fn cache_roundtrip_and_known_usage() {
        let conn = db();
        let key = cache_key(1, "m", &req(0.0, TaskKind::Extract)).unwrap();
        assert!(cache_get(&conn, &key).unwrap().is_none());
        cache_put(&conn, &key, &json!({"ok": true}), Some((100, 50))).unwrap();
        assert_eq!(cache_get(&conn, &key).unwrap().unwrap()["ok"], true);
        record_usage(&conn, 1, Some(7), TaskKind::Extract, Some((100, 50))).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM llm_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 1);
    }

    #[test]
    fn unavailable_usage_is_not_recorded_as_zero() {
        let conn = db();
        record_usage(&conn, 1, Some(7), TaskKind::Extract, None).unwrap();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM llm_usage", [], |r| r.get(0))
            .unwrap();
        assert_eq!(n, 0);
    }
}
