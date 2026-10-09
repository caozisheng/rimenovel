// LLM Provider 层（dev-plan Task 3.1）
// 任务路由三类: extract(窗口/章节抽取/回抽/消歧) merge(合并裁决/前向调和) write(正文生成)
pub mod anthropic;
pub mod cache;
pub mod openai;

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
    /// JSON Schema（如 docs/schemas/chapter-graph.schema.json 编译期内嵌）
    pub schema: &'a Value,
    pub schema_name: &'a str,
    pub temperature: f32, // 抽取/调和恒 0.0
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
    Server { status: u16, body: String },
    #[error("schema 校验失败: {0}")]
    Schema(String),
    #[error("流中断: {0}")]
    Stream(String),
    #[error("配置错误: {0}")]
    Config(String),
}

/// 协议客户端抽象：openai.rs / anthropic.rs 各自实现
#[async_trait::async_trait]
pub trait LlmClient: Send + Sync {
    /// 结构化输出：协议实现负责请求组装、响应解析、jsonschema 校验、
    /// 失败重试 1 次、降级 prompt 内嵌 schema 再 1 次（实现侧约定）。
    async fn structured(&self, req: StructuredRequest<'_>) -> Result<Value, LlmError>;

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
