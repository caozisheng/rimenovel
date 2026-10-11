// Native Anthropic Messages adapter. Structured responses use forced tool use;
// streaming responses use the Messages SSE protocol.

use crate::llm::{reported_usage, ChatMessage, LlmClient, LlmError, ProviderConfig,
    StructuredRequest, StructuredResponse, TextRequest};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::sync::Arc;

mod sse;

pub struct AnthropicClient {
    pub cfg: ProviderConfig,
    pub http: reqwest::Client,
}

const API_VERSION: &str = "2023-06-01";

impl AnthropicClient {
    fn endpoint(&self, base: &str) -> String {
        format!("{}/v1/messages", base.trim_end_matches('/'))
    }

    fn auth(&self, rb: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        rb.header("x-api-key", &self.cfg.api_key)
            .header("anthropic-version", API_VERSION)
    }

    fn sanitize(&self, message: String) -> String {
        if self.cfg.api_key.is_empty() || !message.contains(&self.cfg.api_key) {
            message
        } else {
            message.replace(&self.cfg.api_key, "[REDACTED]")
        }
    }

    fn server_error(&self, status: u16, body: String, headers: &reqwest::header::HeaderMap) -> LlmError {
        let error = crate::llm::server_error(status, self.sanitize(body), headers);
        match error {
            LlmError::Server { status, body, retry_after, request_id } => LlmError::Server {
                status, body, retry_after, request_id: request_id.map(|id| self.sanitize(id)),
            },
            error => error,
        }
    }

    /// Anthropic keeps system prompts outside the messages array.
    fn split_messages(messages: &[ChatMessage]) -> (Option<String>, Vec<Value>) {
        let mut system = String::new();
        let mut first = true;
        for message in messages.iter().filter(|message| message.role == "system") {
            if !first { system.push('\n'); }
            system.push_str(&message.content);
            first = false;
        }
        let msgs = messages.iter().filter(|m| m.role != "system")
            .map(|m| json!({"role": m.role, "content": m.content})).collect();
        (if system.is_empty() { None } else { Some(system) }, msgs)
    }

    async fn post_once(&self, body: Value) -> Result<Value, LlmError> {
        let response = self.auth(self.http.post(self.endpoint(&self.cfg.base_url)).json(&body))
            .send().await.map_err(|e| LlmError::Network(self.sanitize(e.without_url().to_string())))?;
        let status = response.status();
        let headers = response.headers().clone();
        let body = response.bytes().await
            .map_err(|e| LlmError::Network(self.sanitize(e.without_url().to_string())))?;
        if !status.is_success() {
            let text = String::from_utf8_lossy(&body).into_owned();
            return Err(self.server_error(status.as_u16(), text, &headers));
        }
        serde_json::from_slice(&body)
            .map_err(|e| LlmError::Schema(format!("Anthropic response is not valid JSON: {e}")))
    }

    fn tool_input(resp: &mut Value, expected_name: &str) -> Result<Value, LlmError> {
        let reason = resp.get("stop_reason").and_then(Value::as_str).unwrap_or("");
        if reason == "max_tokens" || reason == "refusal" {
            return Err(LlmError::Stream(format!("generation ended with {reason}")));
        }
        if reason != "tool_use" {
            return Err(LlmError::Stream(format!("structured response ended with {reason}")));
        }
        let blocks = resp.get_mut("content").and_then(Value::as_array_mut)
            .ok_or_else(|| LlmError::Schema("response missing content blocks".into()))?;
        let tool = blocks.iter_mut().find(|block| block.get("type").and_then(Value::as_str) == Some("tool_use"))
            .ok_or_else(|| LlmError::Schema("response missing tool_use block".into()))?;
        if tool.get("name").and_then(Value::as_str) != Some(expected_name) {
            return Err(LlmError::Schema("response used an unexpected tool".into()));
        }
        tool.get_mut("input").map(Value::take)
            .ok_or_else(|| LlmError::Schema("tool_use block missing input".into()))
    }
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn structured_with_usage(&self, req: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
        let validator = jsonschema::validator_for(req.schema)
            .map_err(|error| LlmError::Schema(self.sanitize(format!("invalid JSON Schema: {error}"))))?;
        if req.max_tokens == 0 { return Err(LlmError::Config("max_tokens must be greater than zero".into())); }
        let model = self.cfg.model_for(req.task)?;
        let (system, messages) = Self::split_messages(&req.messages);
        let mut body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "messages": messages,
            "tools": [{"name": req.schema_name, "description": "structured output", "input_schema": req.schema}],
            "tool_choice": {"type": "tool", "name": req.schema_name},
        });
        if let Some(system) = system { body["system"] = json!(system); }
        let mut response = self.post_once(body).await?;
        let usage = reported_usage(&response, "input_tokens", "output_tokens");
        let value = Self::tool_input(&mut response, req.schema_name).map_err(|error| match error {
            LlmError::Stream(message) => LlmError::Stream(self.sanitize(message)),
            error => error,
        })?;
        validator.validate(&value).map_err(|e| LlmError::Schema(self.sanitize(e.to_string())))?;
        Ok(StructuredResponse { value, usage })
    }

    async fn stream_text(&self, req: TextRequest, on_delta: Arc<dyn for<'a> Fn(&'a str) + Send + Sync>)
        -> Result<String, LlmError> {
        if req.max_tokens.unwrap_or(0) == 0 { return Err(LlmError::Config("max_tokens must be provided and greater than zero".into())); }
        let model = self.cfg.model_for(req.task)?;
        let (system, messages) = Self::split_messages(&req.messages);
        let mut body = json!({
            "model": model, "temperature": req.temperature,
            "max_tokens": req.max_tokens, "stream": true, "messages": messages,
        });
        if let Some(system) = system { body["system"] = json!(system); }
        let response = self.auth(self.http.post(self.endpoint(&self.cfg.base_url)).json(&body))
            .send().await.map_err(|e| LlmError::Network(self.sanitize(e.without_url().to_string())))?;
        let status = response.status();
        let headers = response.headers().clone();
        if !status.is_success() {
            let bytes = response.bytes().await.map_err(|e| LlmError::Network(self.sanitize(e.without_url().to_string())))?;
            let text = String::from_utf8_lossy(&bytes).into_owned();
            return Err(self.server_error(status.as_u16(), text, &headers));
        }

        use futures_util::StreamExt;
        let mut bytes = response.bytes_stream();
        let mut decoder = sse::Decoder::default();
        let mut full = String::new();
        while let Some(chunk) = bytes.next().await {
            let chunk = chunk.map_err(|e| LlmError::Stream(self.sanitize(e.without_url().to_string())))?;
            let done = decoder.feed(&chunk, |event, data| consume_event(event, data, &mut full, &on_delta))
                .map_err(|error| match error {
                    LlmError::Stream(message) => LlmError::Stream(self.sanitize(message)),
                    error => error,
                })?;
            if done { return Ok(full); }
        }
        decoder.finish()?;
        Err(LlmError::Stream("generation ended before message_stop".into()))
    }
}

fn consume_event(
    event: &str,
    data: &str,
    full: &mut String,
    on_delta: &Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
) -> Result<bool, LlmError> {
    let value: Value = serde_json::from_str(data)
        .map_err(|e| LlmError::Stream(format!("invalid Anthropic SSE JSON: {e}")))?;
    if event == "error" || value.get("type").and_then(Value::as_str) == Some("error") {
        let message = value.pointer("/error/message").and_then(Value::as_str)
            .or_else(|| value.get("message").and_then(Value::as_str)).unwrap_or("provider stream error");
        return Err(LlmError::Stream(message.to_owned()));
    }
    if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str)
        .or_else(|| value.get("stop_reason").and_then(Value::as_str)) {
        if !matches!(reason, "end_turn" | "stop_sequence") {
            return Err(LlmError::Stream(format!("generation ended with {reason}")));
        }
    }
    match value.get("type").and_then(Value::as_str).or(Some(event)) {
        Some("content_block_delta") => {
            let delta = value.get("delta")
                .ok_or_else(|| LlmError::Stream("content_block_delta missing delta".into()))?;
            if delta.get("type").and_then(Value::as_str) == Some("text_delta") || delta.get("text").is_some() {
                let text = delta.get("text").and_then(Value::as_str)
                    .ok_or_else(|| LlmError::Stream("text_delta missing valid text".into()))?;
                full.push_str(text);
                on_delta(text);
            }
        }
        Some("message_stop") => return Ok(true),
        _ => {}
    }
    Ok(false)
}

#[cfg(test)]
#[path = "anthropic/tests.rs"]
mod tests;
