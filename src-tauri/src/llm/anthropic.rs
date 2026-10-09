// Anthropic 协议实现（dev-plan Task 3.3）
// /v1/messages: 结构化=forced tool-use(input_json_schema); SSE 流式;
// 头: x-api-key + anthropic-version; 429/5xx 退避 1 次; schema 失败 → 内嵌降级 1 次。

use crate::llm::{
    ChatMessage, LlmClient, LlmError, ProviderConfig, StructuredRequest, TextRequest,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct AnthropicClient {
    pub cfg: ProviderConfig,
    pub http: reqwest::Client,
}

const RETRY_BACKOFF: Duration = Duration::from_millis(500);
const API_VERSION: &str = "2023-06-01";

impl AnthropicClient {
    fn endpoint(&self, base: &str) -> String {
        format!("{}/v1/messages", base.trim_end_matches('/'))
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("x-api-key", &self.cfg.api_key)
            .header("anthropic-version", API_VERSION)
    }

    /// Anthropic 消息格式：system 提出消息数组外，其余 role user/assistant 交替
    fn split_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let system = messages
            .iter()
            .filter(|m| m.role == "system")
            .map(|m| m.content.clone())
            .collect::<Vec<_>>()
            .join("\n");
        let msgs: Vec<Value> = messages
            .iter()
            .filter(|m| m.role != "system")
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();
        (
            if system.is_empty() {
                None
            } else {
                Some(system)
            },
            msgs,
        )
    }

    async fn post_with_retry(&self, body: Value) -> Result<Value, LlmError> {
        let url = self.endpoint(&self.cfg.base_url);
        let mut attempt = 0;
        loop {
            let resp = self
                .auth(self.http.post(&url).json(&body))
                .send()
                .await
                .map_err(|e| LlmError::Network(e.to_string()))?;
            let status = resp.status();
            if status.is_success() {
                return resp
                    .json()
                    .await
                    .map_err(|e| LlmError::Network(e.to_string()));
            }
            let body_txt = resp.text().await.unwrap_or_default();
            if (status.as_u16() == 429 || status.is_server_error()) && attempt == 0 {
                attempt += 1;
                tokio::time::sleep(RETRY_BACKOFF).await;
                continue;
            }
            return Err(LlmError::Server {
                status: status.as_u16(),
                body: body_txt,
            });
        }
    }

    /// 从 tool_use 块取 input
    fn tool_input(resp: &Value) -> Option<Value> {
        resp.pointer("/content")
            .and_then(Value::as_array)?
            .iter()
            .find(|b| b.get("type").and_then(Value::as_str) == Some("tool_use"))
            .and_then(|b| b.get("input"))
            .cloned()
    }

    /// 从文本块拼接全文
    fn text_content(resp: &Value) -> Option<String> {
        let arr = resp.pointer("/content").and_then(Value::as_array)?;
        let s: Vec<&str> = arr
            .iter()
            .filter(|b| b.get("type").and_then(Value::as_str) == Some("text"))
            .filter_map(|b| b.get("text").and_then(Value::as_str))
            .collect();
        if s.is_empty() {
            None
        } else {
            Some(s.join(""))
        }
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn structured(&self, req: StructuredRequest<'_>) -> Result<Value, LlmError> {
        let model = self.cfg.model_for(req.task)?.to_string();
        let (system, msgs) = Self::split_messages(&req.messages);

        let mut body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": 8192,
            "messages": msgs,
            "tools": [{
                "name": req.schema_name,
                "description": "structured output",
                "input_schema": req.schema,
            }],
            "tool_choice": {"type": "tool", "name": req.schema_name},
        });
        if let Some(sys) = &system {
            body["system"] = json!(sys);
        }
        let resp = self.post_with_retry(body).await?;
        let parsed =
            Self::tool_input(&resp).ok_or_else(|| LlmError::Stream("响应缺 tool_use 块".into()))?;
        let validator = crate::llm::compile_schema(req.schema);
        if validator.validate(&parsed).is_ok() {
            return Ok(parsed);
        }

        // 降级：去掉 tool_choice 强制，system 内嵌 schema 指示直接输出 JSON 文本
        let mut body2 = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": 8192,
            "messages": req.messages.iter().map(|m| json!({"role": if m.role=="system" {"user"} else {&m.role}, "content": m.content})).collect::<Vec<_>>(),
        });
        let schema_txt = serde_json::to_string(req.schema).unwrap_or_default();
        body2["system"] = json!(format!(
            "你的输出必须是符合以下 JSON Schema 的单个 JSON 对象（不要多余文本）:\n{schema_txt}"
        ));
        let resp2 = self.post_with_retry(body2).await?;
        let text = Self::text_content(&resp2)
            .ok_or_else(|| LlmError::Stream("降级响应缺 text 块".into()))?;
        let parsed2: Value = serde_json::from_str(text.trim())
            .map_err(|e| LlmError::Schema(format!("降级仍非合法 JSON: {e}")))?;
        validator
            .validate(&parsed2)
            .map_err(|e| LlmError::Schema(e.to_string()))?;
        Ok(parsed2)
    }

    async fn stream_text(
        &self,
        req: TextRequest,
        on_delta: std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
    ) -> Result<String, LlmError> {
        let model = self.cfg.model_for(req.task)?.to_string();
        let (system, msgs) = Self::split_messages(&req.messages);
        let mut body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens.unwrap_or(8192),
            "stream": true,
            "messages": msgs,
        });
        if let Some(sys) = &system {
            body["system"] = json!(sys);
        }
        let url = self.endpoint(&self.cfg.base_url);
        let resp = self
            .auth(self.http.post(&url).json(&body))
            .send()
            .await
            .map_err(|e| LlmError::Network(e.to_string()))?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let b = resp.text().await.unwrap_or_default();
            return Err(LlmError::Server { status, body: b });
        }
        // SSE 事件: content_block_delta 的 delta.text 为增量; message_stop 结束
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut full = String::new();
        while let Some(chunk) = stream.next().await {
            let bytes = chunk.map_err(|e| LlmError::Stream(e.to_string()))?;
            buf.push_str(&String::from_utf8_lossy(&bytes));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                if let Some(data) = line.strip_prefix("data:") {
                    let data = data.trim();
                    if let Ok(ev) = serde_json::from_str::<Value>(data) {
                        let is_stop = ev["type"].as_str() == Some("message_stop");
                        if is_stop {
                            return Ok(full);
                        }
                        if ev["type"].as_str() == Some("content_block_delta") {
                            apply_delta(&ev, &mut full, &on_delta);
                        }
                    }
                }
            }
        }
        Ok(full)
    }
}

/// 把 content_block_delta 的文本增量追加到 full 并转发回调。
/// 独立函数使 `ev` 的借用局部化（async_trait 脱糖下 Drop 顺序分析的限制）。
fn apply_delta(
    ev: &Value,
    full: &mut String,
    on_delta: &std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
) {
    if let Some(t) = ev.pointer("/delta/text").and_then(Value::as_str) {
        full.push_str(t);
        on_delta(t);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::TaskKind;
    use wiremock::matchers::{body_partial_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(server_uri: String) -> ProviderConfig {
        ProviderConfig {
            id: 1,
            name: "test-anthropic".into(),
            protocol: "anthropic".into(),
            base_url: server_uri,
            model_extract: Some("claude-test".into()),
            model_write: Some("claude-test".into()),
            model_merge: Some("claude-test".into()),
            api_key: "ak-test".into(),
        }
    }

    fn schema() -> Value {
        json!({
            "type": "object",
            "additionalProperties": false,
            "required": ["ok"],
            "properties": {"ok": {"type": "boolean"}}
        })
    }

    fn tool_use_body() -> Value {
        json!({"content": [{"type": "tool_use", "input": {"ok": true}}]})
    }

    #[tokio::test]
    async fn structured_via_forced_tool_use() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_partial_json(json!({"tool_choice": {"type": "tool"}})))
            .and(header("x-api-key", "ak-test"))
            .and(header("anthropic-version", "2023-06-01"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tool_use_body()))
            .mount(&server)
            .await;
        let client = AnthropicClient {
            cfg: cfg(server.uri()),
            http: reqwest::Client::new(),
        };
        let req = StructuredRequest {
            task: TaskKind::Extract,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "抽取".into(),
            }],
            schema: &schema(),
            schema_name: "test",
            temperature: 0.0,
        };
        let v = client.structured(req).await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn schema_failure_falls_back_to_text() {
        let server = MockServer::start().await;
        // 第一次: tool_use 输出不合 schema
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .and(body_partial_json(json!({"tool_choice": {"type": "tool"}})))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "tool_use", "input": {"wrong": 1}}]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // 降级: text 块返回合法 JSON 文本
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "content": [{"type": "text", "text": "{\"ok\": true}"}]
            })))
            .mount(&server)
            .await;
        let client = AnthropicClient {
            cfg: cfg(server.uri()),
            http: reqwest::Client::new(),
        };
        let req = StructuredRequest {
            task: TaskKind::Extract,
            messages: vec![ChatMessage {
                role: "user".into(),
                content: "x".into(),
            }],
            schema: &schema(),
            schema_name: "t",
            temperature: 0.0,
        };
        let v = client.structured(req).await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn rate_limit_retries_once() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(500).set_body_string("boom"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(ResponseTemplate::new(200).set_body_json(tool_use_body()))
            .mount(&server)
            .await;
        let client = AnthropicClient {
            cfg: cfg(server.uri()),
            http: reqwest::Client::new(),
        };
        let v = client
            .post_with_retry(json!({"model": "claude-test"}))
            .await
            .unwrap();
        assert!(v["content"].is_array());
    }

    #[tokio::test]
    async fn sse_stream_deltas_and_stop() {
        let server = MockServer::start().await;
        let sse = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"山雨\"}}\n\n\
                   event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"delta\":{\"type\":\"text_delta\",\"text\":\"欲来\"}}\n\n\
                   event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        Mock::given(method("POST"))
            .and(path("/v1/messages"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;
        let client = AnthropicClient {
            cfg: cfg(server.uri()),
            http: reqwest::Client::new(),
        };
        let got = std::sync::Arc::new(parking_lot::Mutex::new(String::new()));
        let sink = got.clone();
        let full = client
            .stream_text(
                TextRequest {
                    task: TaskKind::Write,
                    messages: vec![ChatMessage {
                        role: "user".into(),
                        content: "写".into(),
                    }],
                    temperature: 0.8,
                    max_tokens: None,
                },
                std::sync::Arc::new(move |d| sink.lock().push_str(d)),
            )
            .await
            .unwrap();
        assert_eq!(full, "山雨欲来");
        assert_eq!(*got.lock(), "山雨欲来");
    }
}
