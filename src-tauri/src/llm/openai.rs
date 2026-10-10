// OpenAI-compatible chat through async-openai. The SDK owns HTTP requests and SSE
// framing; this adapter performs one attempt and validates provider completion.

use crate::llm::{reported_usage, server_error, LlmClient, LlmError, ProviderConfig, StructuredRequest, StructuredResponse, TextRequest};
use async_openai::{config::OpenAIConfig, error::OpenAIError, middleware::{HttpRequestFactory, ReqwestService}, Client};
use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tower::ServiceExt;
use tokio_util::sync::CancellationToken;

mod bom;

pub struct OpenAiClient {
    pub cfg: ProviderConfig,
    pub http: reqwest::Client,
}

impl OpenAiClient {
    fn sdk(&self, cancellation: CancellationToken) -> Result<Client<OpenAIConfig>, LlmError> {
        // OpenAIConfig::headers unwraps bearer parsing. Preserve fallible adapter
        // behavior without echoing credential material in the configuration error.
        reqwest::header::HeaderValue::from_str(&self.cfg.api_key)
            .map_err(|_| LlmError::Config("API key contains invalid header characters".into()))?;
        let config = OpenAIConfig::new()
            .with_api_base(self.cfg.base_url.trim_end_matches('/'))
            .with_api_key(self.cfg.api_key.clone())
            .with_org_id("")
            .with_project_id("");
        let transport = ReqwestService::new(self.http.clone());
        // Installing a plain service replaces the SDK's default retry executor.
        // Intercept errors before SDK deserialization discards status and headers
        // on non-JSON responses (common for gateway HTML on 503/504).
        let service = tower::service_fn(move |factory: HttpRequestFactory| {
            let transport = transport.clone();
            let cancellation = cancellation.clone();
            async move {
                let response = transport.oneshot(factory).await?;
                if response.status().is_success() {
                    return bom::normalize(response, cancellation);
                }
                let status = response.status().as_u16();
                let headers = response.headers().clone();
                let body = response.text().await.map_err(OpenAIError::Reqwest)?;
                Err(OpenAIError::Boxed(Box::new(server_error(status, body, &headers))))
            }
        });
        Ok(Client::build(self.http.clone(), config).with_http_service(service))
    }

    fn redact_error(&self, mut error: LlmError) -> LlmError {
        if self.cfg.api_key.is_empty() {
            return error;
        }
        let redact = |text: &mut String| {
            if text.contains(&self.cfg.api_key) {
                *text = text.replace(&self.cfg.api_key, "[REDACTED]");
            }
        };
        match &mut error {
            LlmError::Network(text) | LlmError::Schema(text) | LlmError::Stream(text) | LlmError::Config(text) => redact(text),
            LlmError::Server { body, request_id, .. } => {
                redact(body);
                if let Some(request_id) = request_id {
                    redact(request_id);
                }
            }
            _ => {}
        }
        error
    }

    async fn structured_once(&self, req: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
        let model = self.cfg.model_for(req.task)?;
        let validator = jsonschema::validator_for(req.schema)
            .map_err(|error| LlmError::Schema(format!("无效 JSON Schema: {error}")))?;
        let messages: Vec<Value> = req.messages.iter()
            .map(|message| json!({"role": message.role, "content": message.content}))
            .collect();
        let body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": true,
            "stream_options": {"include_usage": true},
            "messages": messages,
            "response_format": {
                "type": "json_schema",
                "json_schema": {"name": req.schema_name, "schema": req.schema, "strict": true}
            }
        });
        // SDK streaming can own a detached reader. Bind its response body to
        // this operation so deadline/cancellation drops also close idle sockets.
        let cancellation = CancellationToken::new();
        let _lifetime = cancellation.clone().drop_guard();
        let sdk = self.sdk(cancellation)?;
        let mut stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Value, OpenAIError>> + Send>> =
            sdk.chat().create_stream_byot(body).await.map_err(sdk_error)?;
        let mut content = String::new();
        let mut finished = false;
        let mut received_content = false;
        let mut usage = None;
        while let Some(event) = stream.next().await {
            let event = event.map_err(sdk_structured_stream_error)?;
            if let Some(error) = event.get("error") {
                return Err(LlmError::Stream(format!("provider stream error: {error}")));
            }
            usage = reported_usage(&event, "prompt_tokens", "completion_tokens").or(usage);
            let Some(choice) = event.pointer("/choices/0") else {
                if event.get("choices").and_then(Value::as_array).is_some_and(Vec::is_empty) { continue; }
                return Err(LlmError::Stream("stream event missing choices/0".into()));
            };
            if choice.pointer("/delta/refusal").is_some_and(|refusal| !refusal.is_null()) {
                return Err(LlmError::Stream("provider refused structured output".into()));
            }
            if let Some(delta) = choice.pointer("/delta/content").and_then(Value::as_str) {
                content.push_str(delta);
                received_content = true;
            }
            if let Some(reason) = choice.get("finish_reason").filter(|reason| !reason.is_null()) {
                require_stop(reason.as_str())?;
                finished = true;
            }
        }
        if !finished || !received_content {
            return Err(LlmError::Stream("structured output ended before complete content and normal finish".into()));
        }
        let value: Value = serde_json::from_str(&content)
            .map_err(|error| LlmError::Schema(format!("非合法 JSON: {error}")))?;
        validator.validate(&value).map_err(|error| LlmError::Schema(error.to_string()))?;
        Ok(StructuredResponse {
            value,
            usage,
        })
    }

    async fn stream_once(
        &self,
        req: TextRequest,
        on_delta: std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
    ) -> Result<String, LlmError> {
        let model = self.cfg.model_for(req.task)?;
        let messages: Vec<Value> = req.messages.iter()
            .map(|message| json!({"role": message.role, "content": message.content}))
            .collect();
        let body = json!({
            "model": model,
            "temperature": req.temperature,
            "max_tokens": req.max_tokens,
            "stream": true,
            "messages": messages,
        });
        let cancellation = CancellationToken::new();
        let _lifetime = cancellation.clone().drop_guard();
        let sdk = self.sdk(cancellation)?;
        let mut stream: std::pin::Pin<Box<dyn futures_util::Stream<Item = Result<Value, OpenAIError>> + Send>> =
            sdk.chat().create_stream_byot(body).await.map_err(sdk_error)?;
        let mut full = String::new();
        let mut finished = false;
        while let Some(event) = stream.next().await {
            // Parsing/transport errors after stream setup are non-retryable:
            // callbacks may already have delivered part of the generated text.
            let event = event.map_err(sdk_stream_error)?;
            if let Some(error) = event.get("error") {
                return Err(LlmError::Stream(format!("provider stream error: {error}")));
            }
            let Some(choice) = event.pointer("/choices/0") else {
                // The optional final usage-only chunk has an empty choices array.
                if event.get("choices").and_then(Value::as_array).is_some_and(Vec::is_empty) {
                    continue;
                }
                return Err(LlmError::Stream("stream event missing choices/0".into()));
            };
            if choice.pointer("/delta/refusal").is_some_and(|refusal| !refusal.is_null()) {
                return Err(LlmError::Stream("provider refused text generation".into()));
            }
            if let Some(reason) = choice.get("finish_reason").filter(|reason| !reason.is_null()) {
                require_stop(reason.as_str())?;
                finished = true;
            }
            if let Some(delta) = choice.pointer("/delta/content").and_then(Value::as_str) {
                full.push_str(delta);
                on_delta(delta);
            }
        }
        // SDK hides [DONE] and returns EOF in either case. A normal finish_reason
        // is completion evidence; neither EOF nor [DONE] alone is sufficient.
        if finished {
            Ok(full)
        } else {
            Err(LlmError::Stream("generation ended before a completion marker".into()))
        }
    }
}

fn sdk_error(error: OpenAIError) -> LlmError {
    match error {
        OpenAIError::Reqwest(error) => LlmError::Network(error.without_url().to_string()),
        OpenAIError::Boxed(error) => match error.downcast::<LlmError>() {
            Ok(error) => *error,
            Err(error) => LlmError::Network(error.to_string()),
        },
        OpenAIError::JSONDeserialize(error, _) => LlmError::Schema(format!("非合法 JSON 响应: {error}")),
        OpenAIError::StreamError(error) => LlmError::Stream(error.to_string()),
        OpenAIError::InvalidArgument(error) => LlmError::Config(error),
        error => LlmError::Network(error.to_string()),
    }
}

fn sdk_structured_stream_error(error: OpenAIError) -> LlmError {
    // SDK erases the transport type into EventStream's Display text. Only its
    // transport variant is safe to retry: structured deltas remain private.
    if let OpenAIError::StreamError(inner) = &error {
        if matches!(inner.as_ref(), async_openai::error::StreamError::EventStream(text) if text.starts_with("Transport error: ")) {
            return LlmError::Network("structured response stream timed out or disconnected".into());
        }
    }
    sdk_stream_error(error)
}

fn sdk_stream_error(error: OpenAIError) -> LlmError {
    match error {
        // SDK Display includes the entire malformed event payload. Report only
        // parser coordinates so echoed prompts do not enter job/UI error text.
        OpenAIError::JSONDeserialize(error, _) => LlmError::Stream(format!(
            "invalid SSE JSON at line {} column {}", error.line(), error.column()
        )),
        error => LlmError::Stream(sdk_error(error).to_string()),
    }
}

fn require_stop(reason: Option<&str>) -> Result<(), LlmError> {
    match reason {
        Some("stop") => Ok(()),
        Some(reason) => Err(LlmError::Stream(format!("generation ended with {reason}"))),
        None => Err(LlmError::Stream("generation ended before a completion marker".into())),
    }
}

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn structured_with_usage(&self, req: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError> {
        self.structured_once(req).await.map_err(|error| self.redact_error(error))
    }

    async fn stream_text(
        &self,
        req: TextRequest,
        on_delta: std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
    ) -> Result<String, LlmError> {
        self.stream_once(req, on_delta).await.map_err(|error| self.redact_error(error))
    }
}

#[cfg(test)]
mod tests;
