//! 与厂商无关的统一模型层。
//!
//! 设计原则：内核只认这一套类型，适配器负责在边界处翻译。
//! 这样切换 OpenAI / Anthropic / Gemini 时，Agent 循环与前端事件流完全不变。

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text { text: String },
    /// 图片以 data URL 形式内联，避免本地文件路径泄漏到上游
    Image { mime: String, data_b64: String },
    ToolUse { id: String, name: String, input: Value },
    ToolResult { id: String, name: String, content: String, is_error: bool },
}

impl ContentPart {
    pub fn text(s: impl Into<String>) -> Self {
        ContentPart::Text { text: s.into() }
    }
    pub fn image(mime: impl Into<String>, data_b64: impl Into<String>) -> Self {
        ContentPart::Image { mime: mime.into(), data_b64: data_b64.into() }
    }
    pub fn tool_use(id: impl Into<String>, name: impl Into<String>, input: Value) -> Self {
        ContentPart::ToolUse { id: id.into(), name: name.into(), input }
    }
    pub fn tool_result(id: impl Into<String>, name: impl Into<String>, content: impl Into<String>, is_error: bool) -> Self {
        ContentPart::ToolResult { id: id.into(), name: name.into(), content: content.into(), is_error }
    }

    pub fn as_text(&self) -> Option<&str> {
        match self {
            ContentPart::Text { text } => Some(text),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MessageMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub provider: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<u64>,
    /// 被上下文压缩归并掉的消息数
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compacted_from: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    pub parts: Vec<ContentPart>,
    #[serde(default, skip_serializing_if = "MessageMeta::is_empty")]
    pub meta: MessageMeta,
}

impl MessageMeta {
    fn is_empty(&self) -> bool {
        self.model_ref.is_none()
            && self.provider.is_none()
            && self.usage.is_none()
            && self.cost_usd.is_none()
            && self.latency_ms.is_none()
            && self.compacted_from.is_none()
    }
}

impl Message {
    pub fn new(role: Role, parts: Vec<ContentPart>) -> Self {
        Message { role, parts, meta: MessageMeta::default() }
    }

    pub fn user(text: impl Into<String>) -> Self {
        Message::new(Role::User, vec![ContentPart::text(text)])
    }

    pub fn system(text: impl Into<String>) -> Self {
        Message::new(Role::System, vec![ContentPart::text(text)])
    }

    pub fn assistant(text: impl Into<String>) -> Self {
        Message::new(Role::Assistant, vec![ContentPart::text(text)])
    }

    pub fn tool_results(results: Vec<ContentPart>) -> Self {
        Message::new(Role::Tool, results)
    }

    /// 纯文本拼接（工具结果/思考过程不计入）
    pub fn plain_text(&self) -> String {
        self.parts.iter().filter_map(|p| p.as_text()).collect::<Vec<_>>().join("")
    }

    pub fn with_meta(mut self, meta: MessageMeta) -> Self {
        self.meta = meta;
        self
    }

    /// 粗略 token 估算：中文按 1 字 ≈ 1 token，英文按 4 字符 ≈ 1 token。
    /// 只用于上下文预算的启发式判断，不作为计费依据。
    pub fn estimate_tokens(&self) -> usize {
        self.parts
            .iter()
            .map(|p| match p {
                ContentPart::Text { text } => estimate_text(text),
                ContentPart::Image { .. } => 780,
                ContentPart::ToolUse { name, input, .. } => estimate_text(name) + estimate_json(input),
                ContentPart::ToolResult { content, .. } => estimate_text(content),
            })
            .sum()
    }
}

pub fn estimate_text(text: &str) -> usize {
    let mut cjk = 0usize;
    let mut other = 0usize;
    for ch in text.chars() {
        if ch.is_ascii() {
            other += 1;
        } else if !ch.is_whitespace() {
            cjk += 1;
        }
    }
    cjk + other.div_ceil(4)
}

pub fn estimate_json(v: &Value) -> usize {
    estimate_text(&v.to_string())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema
    #[serde(default = "empty_object_schema")]
    pub parameters: Value,
    /// 是否由 MCP 服务提供（UI 上标注来源与风险等级）
    #[serde(default)]
    pub source: ToolSource,
}

fn empty_object_schema() -> Value {
    json!({ "type": "object", "properties": {}, "additionalProperties": false })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    #[default]
    Builtin,
    Mcp,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        self.input_tokens + self.output_tokens
    }

    pub fn merge(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_read_tokens += other.cache_read_tokens;
        self.cache_write_tokens += other.cache_write_tokens;
        self.reasoning_tokens += other.reasoning_tokens;
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Pricing {
    /// 美元 / 百万输入 token
    #[serde(default)]
    pub input_per_mtok: f64,
    #[serde(default)]
    pub output_per_mtok: f64,
    #[serde(default)]
    pub cache_read_per_mtok: f64,
}

impl Pricing {
    pub fn cost_usd(&self, u: &Usage) -> f64 {
        let billable_input = u.input_tokens.saturating_sub(u.cache_read_tokens);
        (billable_input as f64 * self.input_per_mtok
            + u.cache_read_tokens as f64 * self.cache_read_per_mtok
            + u.output_tokens as f64 * self.output_per_mtok)
            / 1_000_000.0
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Cancelled,
    Other(String),
}

impl FinishReason {
    pub fn as_label(&self) -> String {
        match self {
            FinishReason::Stop => "stop".into(),
            FinishReason::Length => "length".into(),
            FinishReason::ToolCalls => "tool_calls".into(),
            FinishReason::ContentFilter => "content_filter".into(),
            FinishReason::Cancelled => "cancelled".into(),
            FinishReason::Other(s) => s.clone(),
        }
    }
}

/// 统一流式事件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    Start { provider: String, model: String },
    Text { delta: String },
    Reasoning { delta: String },
    ToolCallStart { index: u32, id: String, name: String },
    ToolCallArgs { index: u32, delta: String },
    Usage { usage: Usage },
    Finish { reason: FinishReason },
    Error { message: String, retryable: bool },
}

#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub stop: Vec<String>,
    /// Gemini/Anthropic 需要独立字段，OpenAI 兼容走 messages[0]
    pub system: Option<String>,
}

impl ChatRequest {
    pub fn new(model: impl Into<String>, messages: Vec<Message>) -> Self {
        ChatRequest {
            model: model.into(),
            messages,
            tools: vec![],
            temperature: None,
            max_tokens: None,
            stop: vec![],
            system: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelCard {
    pub id: String,
    pub display_name: String,
    #[serde(default)]
    pub context_window: u32,
    #[serde(default)]
    pub supports_tools: bool,
    #[serde(default)]
    pub supports_vision: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Capabilities {
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub vision: bool,
    #[serde(default)]
    pub reasoning: bool,
    /// 流式结束时是否稳定返回 usage（影响成本账本准确度）
    #[serde(default)]
    pub stream_usage: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_uses_cache_read_price() {
        let p = Pricing { input_per_mtok: 3.0, output_per_mtok: 15.0, cache_read_per_mtok: 0.3 };
        let u = Usage { input_tokens: 1_000_000, output_tokens: 100_000, cache_read_tokens: 500_000, ..Default::default() };
        let expected = (500_000.0 * 3.0 + 500_000.0 * 0.3 + 100_000.0 * 15.0) / 1_000_000.0;
        assert!((p.cost_usd(&u) - expected).abs() < 1e-9);
    }

    #[test]
    fn estimates_cjk_and_ascii() {
        assert_eq!(estimate_text("你好世界"), 4);
        assert_eq!(estimate_text("abcdefgh"), 2);
    }

    #[test]
    fn message_roundtrips_through_json() {
        let m = Message::new(Role::Assistant, vec![
            ContentPart::text("hi"),
            ContentPart::tool_use("c1", "fs_read", json!({"path":"a.txt"})),
        ]);
        let s = serde_json::to_string(&m).unwrap();
        let back: Message = serde_json::from_str(&s).unwrap();
        assert_eq!(m, back);
    }
}
