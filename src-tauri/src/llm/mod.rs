// LLM Provider 层（dev-plan Task 3.1）
// 任务档位：extract（轻量章节图谱/回抽）、merge（下一章工作图谱）、write（流式正文）。
pub mod anthropic;
pub mod cache;
pub mod openai;
pub mod policy;

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// 编译 JSON Schema（jsonschema crate 校验器复用入口）
pub fn compile_schema(schema: &Value) -> jsonschema::Validator {
    jsonschema::validator_for(schema).expect("schema 必须预校验合法")
}
/// 任务档位 → provider 配置的模型字段映射
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TaskKind {
    Extract,
    Merge,
    Write,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            TaskKind::Extract => "extract",
            TaskKind::Merge => "merge",
            TaskKind::Write => "write",
        }
    }
}

#[derive(Debug, Clone)]
pub struct ChatMessage {
    pub role: String, // system|user|assistant
    pub content: String,
}

/// 结构化输出请求（抽取/调和）：附 JSON Schema 强约束
#[derive(Debug, Clone)]
pub struct StructuredRequest<'a> {
    pub task: TaskKind,
    pub messages: Vec<ChatMessage>,
    /// Exact versioned JSON Schema; not a generated SDK schema.
    pub schema: &'a Value,
    pub schema_name: &'a str,
    pub temperature: f32, // 抽取/调和恒 0.0
    pub max_tokens: u32,
}

/// 正文生成请求（流式）
#[derive(Debug, Clone)]
pub struct TextRequest {
    pub task: TaskKind, // 恒 Write
    pub messages: Vec<ChatMessage>,
    pub temperature: f32,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("网络错误: {0}")]
    Network(String),
    #[error("限流/服务端错误 {status}: {body}")]
    Server { status: u16, body: String, retry_after: Option<std::time::Duration>, request_id: Option<String> },
    #[error("schema 校验失败: {0}")]
    Schema(String),
    #[error("流中断: {0}")]
    Stream(String),
    #[error("配置错误: {0}")]
    Config(String),
    #[error("请求已取消")]
    Cancelled,
    #[error("模型请求超时")]
    Timeout,
}

#[derive(Clone, Debug)]
pub struct StructuredResponse {
    pub value: Value,
    pub usage: Option<(i64, i64)>,
}

pub(crate) fn reported_usage(response:&Value, input_key:&str, output_key:&str) -> Option<(i64,i64)> {
    let usage = response.get("usage")?;
    let input = usage.get(input_key)?.as_i64()?;
    let output = usage.get(output_key)?.as_i64()?;
    (input >= 0 && output >= 0).then_some((input,output))
}


/// 协议客户端抽象：openai.rs / anthropic.rs 各自实现
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// Each adapter performs one attempt. The policy wrapper owns retries.
    async fn structured(&self, req: StructuredRequest<'_>) -> Result<Value, LlmError> {
        Ok(self.structured_with_usage(req).await?.value)
    }
    async fn structured_with_usage(&self, req: StructuredRequest<'_>) -> Result<StructuredResponse, LlmError>;

    /// Cancellation is checked both around awaits and before durable writes.
    fn check_cancelled(&self) -> Result<(), LlmError> { Ok(()) }

    /// Serializes cancellation against a host durable operation. No network awaits inside.
    fn commit_guard(&self) -> Result<Option<policy::CommitGuard<'_>>, LlmError> { self.check_cancelled()?; Ok(None) }

    /// 流式正文生成：on_delta 逐块回调（token 流透传到前端 event）。
    /// Arc<dyn Fn> 避免 async_trait 脱糖下 &dyn Fn(&str) 的 HRTB 生命周期冲突。
    async fn stream_text(
        &self,
        req: TextRequest,
        on_delta: std::sync::Arc<dyn for<'a> Fn(&'a str) + Send + Sync>,
    ) -> Result<String, LlmError>;
}

/// Provider 连接配置（DB llm_providers 行 + 凭据引用解析后的 key）
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderConfig {
    pub id: i64,
    pub name: String,
    pub protocol: String, // openai|anthropic
    pub base_url: String,
    pub model_extract: Option<String>,
    pub model_write: Option<String>,
    pub model_merge: Option<String>,
    #[serde(skip)]
    pub api_key: String, // 运行期从 creds 解析，不序列化
}

impl ProviderConfig {
    pub fn model_for(&self, task: TaskKind) -> Result<&str, LlmError> {
        let m = match task {
            TaskKind::Extract => self.model_extract.as_deref(),
            TaskKind::Merge => self.model_merge.as_deref(),
            TaskKind::Write => self.model_write.as_deref(),
        };
        m.ok_or_else(|| {
            LlmError::Config(format!(
                "provider {} 未配置 {} 模型",
                self.name,
                task.as_str()
            ))
        })
    }
}

pub fn server_error(status: u16, body: String, headers: &reqwest::header::HeaderMap) -> LlmError {
    let retry_after = headers.get("retry-after").and_then(|value| value.to_str().ok()).and_then(|value| {
        value.parse::<u64>().ok().map(std::time::Duration::from_secs)
            .or_else(|| httpdate::parse_http_date(value).ok().map(|date| date.duration_since(std::time::SystemTime::now()).unwrap_or_default()))
    });
    let request_id = ["x-request-id", "request-id"].iter().find_map(|name| headers.get(*name).and_then(|value| value.to_str().ok()).map(str::to_owned));
    LlmError::Server { status, body, retry_after, request_id }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_for_routes_by_task() {
        let p = ProviderConfig {
            id: 1,
            name: "openrouter".into(),
            protocol: "openai".into(),
            base_url: "https://example/v1".into(),
            model_extract: Some("cheap".into()),
            model_write: Some("quality".into()),
            model_merge: Some("mid".into()),
            api_key: "sk-x".into(),
        };
        assert_eq!(p.model_for(TaskKind::Extract).unwrap(), "cheap");
        assert_eq!(p.model_for(TaskKind::Write).unwrap(), "quality");
        assert_eq!(p.model_for(TaskKind::Merge).unwrap(), "mid");
    }

    #[test]
    fn model_for_missing_returns_config_error() {
        let p = ProviderConfig {
            id: 2,
            name: "partial".into(),
            protocol: "openai".into(),
            base_url: "https://x".into(),
            model_extract: None,
            model_write: None,
            model_merge: None,
            api_key: String::new(),
        };
        assert!(matches!(
            p.model_for(TaskKind::Write),
            Err(LlmError::Config(_))
        ));
    }

    #[test]
    fn provider_config_never_serializes_api_key() {
        let p = ProviderConfig {
            id: 1,
            name: "n".into(),
            protocol: "openai".into(),
            base_url: "u".into(),
            model_extract: None,
            model_write: None,
            model_merge: None,
            api_key: "SECRET".into(),
        };
        let json = serde_json::to_string(&p).unwrap();
        assert!(!json.contains("SECRET"));
    }
}
