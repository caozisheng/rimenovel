// OpenAI 兼容协议实现（dev-plan Task 3.2）
// /chat/completions: 结构化(json_schema strict) + SSE 流式；Bearer 认证；
// 429/5xx 指数退避 1 次；schema 校验失败 → prompt 内嵌 schema 降级重试 1 次。

use crate::llm::{LlmClient, LlmError, ProviderConfig, StructuredRequest, TextRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::time::Duration;

pub struct OpenAiClient {
    pub cfg: ProviderConfig,
    pub http: reqwest::Client,
}

const RETRY_BACKOFF: Duration = Duration::from_millis(500);

impl OpenAiClient {
    fn endpoint(&self, base: &str) -> String {
        format!("{}/chat/completions", base.trim_end_matches('/'))
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.bearer_auth(&self.cfg.api_key)
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
            // 429/5xx 退避重试一次
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
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn structured(&self, req: StructuredRequest<'_>) -> Result<Value, LlmError> {
        let model = self.cfg.model_for(req.task)?.to_string();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();

        // 第一次：response_format json_schema(strict)
        let body = json!({
            "model": model,
            "temperature": req.temperature,
            "messages": messages,
            "response_format": {
                "type": "json_schema",
                "json_schema": {
                    "name": req.schema_name,
                    "schema": req.schema,
                    "strict": true,
                },
            },
        });
        let resp = self.post_with_retry(body).await?;
        let content = resp
            .pointer("/choices/0/message/content")
            .and_then(Value::as_str)
            .ok_or_else(|| LlmError::Stream("响应缺 choices/0/message/content".into()))?;
        let parsed: Value = serde_json::from_str(content)
            .map_err(|e| LlmError::Schema(format!("非合法 JSON: {e}")))?;
        let validator = crate::llm::compile_schema(req.schema);
        if let Err(_e) = validator.validate(&parsed) {
            // 降级：prompt 内嵌 schema 再试一次
            let mut msgs2: Vec<Value> = messages.clone();
            msgs2.push(json!({
                "role": "system",
                "content": format!(
                    "你的输出必须是符合以下 JSON Schema 的单个 JSON 对象（不要多余文本）:\n{}",
                    serde_json::to_string(req.schema).unwrap_or_default()
                ),
            }));
            let body2 = json!({
                "model": model,
                "temperature": req.temperature,
                "messages": msgs2,
                "response_format": {"type": "json_object"},
            });
            let resp2 = self.post_with_retry(body2).await?;
            let content2 = resp2
                .pointer("/choices/0/message/content")
                .and_then(Value::as_str)
                .ok_or_else(|| LlmError::Stream("降级响应缺 content".into()))?;
            let parsed2: Value = serde_json::from_str(content2)
                .map_err(|e| LlmError::Schema(format!("降级仍非合法 JSON: {e}")))?;
            validator
                .validate(&parsed2)
                .map_err(|e| LlmError::Schema(e.to_string()))?;
            return Ok(parsed2);
        }
        Ok(parsed)
    }

    async fn stream_text(
        &self,
        req: TextRequest,
        on_delta: std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
    ) -> Result<String, LlmError> {
        let model = self.cfg.model_for(req.task)?.to_string();
        let messages: Vec<Value> = req
            .messages
            .iter()
            .map(|m| json!({"role": m.role, "content": m.content}))
            .collect();
        let body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": true,
            "messages": messages,
        });
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
        // SSE: data: {...}\n\n, [DONE] 结束
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
                    if data == "[DONE]" {
                        return Ok(full);
                    }
                    if let Ok(ev) = serde_json::from_str::<Value>(data) {
                        if let Some(d) = ev
                            .pointer("/choices/0/delta/content")
                            .and_then(Value::as_str)
                        {
                            full.push_str(d);
                            on_delta(d);
                        }
                    }
                }
            }
        }
        Ok(full)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::{ChatMessage, TaskKind};
    use wiremock::matchers::{body_partial_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn cfg(server_uri: String) -> ProviderConfig {
        ProviderConfig {
            id: 1,
            name: "test-openai".into(),
            protocol: "openai".into(),
            base_url: server_uri,
            model_extract: Some("gpt-test".into()),
            model_write: Some("gpt-test".into()),
            model_merge: Some("gpt-test".into()),
            api_key: "sk-test".into(),
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

    fn ok_body() -> Value {
        json!({"choices": [{"message": {"content": "{\"ok\": true}"}}]})
    }

    #[tokio::test]
    async fn structured_parses_and_validates() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(
                json!({"response_format": {"type": "json_schema"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        let client = OpenAiClient {
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
    async fn schema_failure_falls_back_to_embedded_schema() {
        let server = MockServer::start().await;
        // 第一次: 返回不合 schema 的 JSON
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(
                json!({"response_format": {"type": "json_schema"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "choices": [{"message": {"content": "{\"wrong\": 1}"} }]
            })))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        // 第二次(降级 json_object): 合法
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(body_partial_json(
                json!({"response_format": {"type": "json_object"}}),
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        let client = OpenAiClient {
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
            schema_name: "test",
            temperature: 0.0,
        };
        let v = client.structured(req).await.unwrap();
        assert_eq!(v["ok"], true);
    }

    #[tokio::test]
    async fn rate_limit_retries_once_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(429).set_body_string("rate"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        let client = OpenAiClient {
            cfg: cfg(server.uri()),
            http: reqwest::Client::new(),
        };
        let body = json!({"model": "gpt-test", "messages": []});
        let v = client.post_with_retry(body).await.unwrap();
        assert!(v["choices"].is_array());
    }

    #[tokio::test]
    async fn bearer_auth_header_sent() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .and(wiremock::matchers::header(
                "authorization",
                "Bearer sk-test",
            ))
            .respond_with(ResponseTemplate::new(200).set_body_json(ok_body()))
            .mount(&server)
            .await;
        let client = OpenAiClient {
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
        client.structured(req).await.unwrap();
    }

    #[tokio::test]
    async fn sse_stream_aggregates_deltas() {
        let server = MockServer::start().await;
        let sse = "data: {\"choices\":[{\"delta\":{\"content\":\"你好\"}}]}\n\n\
                   data: {\"choices\":[{\"delta\":{\"content\":\"，世界\"}}]}\n\n\
                   data: [DONE]\n\n";
        Mock::given(method("POST"))
            .and(path("/chat/completions"))
            .respond_with(
                ResponseTemplate::new(200)
                    .insert_header("content-type", "text/event-stream")
                    .set_body_string(sse),
            )
            .mount(&server)
            .await;
        let client = OpenAiClient {
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
        assert_eq!(full, "你好，世界");
        assert_eq!(*got.lock(), "你好，世界");
    }
}
