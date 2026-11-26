//! OpenAI `/chat/completions` 适配器。
//!
//! 这一份代码同时服务 DeepSeek / Moonshot / 智谱 / 豆包 Ark / 通义兼容模式 /
//! OpenRouter / one-api 自建网关 —— 只要它兼容 OpenAI 的请求与 SSE 增量格式。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::{parse_sse, EventStream, Provider, ProviderInfo, ProviderKind};
use crate::model::*;
use crate::{Error, Result};

pub struct OpenAiProvider {
    id: String,
    label: String,
    base_url: String,
    api_key: String,
    kind: ProviderKind,
    default_model: String,
    models: Vec<ModelCard>,
    pricing: Pricing,
    client: reqwest::Client,
    extra_headers: Vec<(String, String)>,
}

impl OpenAiProvider {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        kind: ProviderKind,
        default_model: impl Into<String>,
        models: Vec<ModelCard>,
        pricing: Pricing,
    ) -> Self {
        OpenAiProvider {
            id: id.into(),
            label: label.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            kind,
            default_model: default_model.into(),
            models,
            pricing,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
            extra_headers: vec![],
        }
    }

    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.extra_headers = headers;
        self
    }

    fn endpoint(&self) -> String {
        match self.kind {
            ProviderKind::AzureOpenAi => format!("{}/chat/completions?api-version=2024-10-21", self.base_url),
            _ => format!("{}/chat/completions", self.base_url),
        }
    }

    fn auth_header(&self) -> Option<(String, String)> {
        if self.api_key.is_empty() {
            return None;
        }
        match self.kind {
            ProviderKind::AzureOpenAi => Some(("api-key".into(), self.api_key.clone())),
            _ => Some(("Authorization".into(), format!("Bearer {}", self.api_key))),
        }
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        let mut body = json!({
            "model": req.model,
            "stream": true,
            "stream_options": { "include_usage": true },
            "messages": self.map_messages(req),
        });

        if !req.tools.is_empty() {
            let tools: Vec<Value> = req
                .tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": t.name,
                            "description": t.description,
                            "parameters": t.parameters,
                        }
                    })
                })
                .collect();
            body["tools"] = Value::Array(tools);
            body["tool_choice"] = json!("auto");
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            body["max_tokens"] = json!(m);
        }
        if !req.stop.is_empty() {
            body["stop"] = json!(req.stop);
        }
        body
    }

    fn map_messages(&self, req: &ChatRequest) -> Value {
        let mut out: Vec<Value> = Vec::new();
        if let Some(sys) = req.system.as_ref().filter(|s| !s.trim().is_empty()) {
            out.push(json!({ "role": "system", "content": sys }));
        }
        for m in &req.messages {
            match m.role {
                Role::System => {
                    let text = m.plain_text();
                    if !text.is_empty() {
                        out.push(json!({ "role": "system", "content": text }));
                    }
                }
                Role::User => {
                    let mut blocks: Vec<Value> = Vec::new();
                    let mut has_image = false;
                    for p in &m.parts {
                        match p {
                            ContentPart::Text { text } => {
                                blocks.push(json!({ "type": "text", "text": text }));
                            }
                            ContentPart::Image { mime, data_b64 } => {
                                has_image = true;
                                blocks.push(json!({
                                    "type": "image_url",
                                    "image_url": { "url": format!("data:{mime};base64,{data_b64}") }
                                }));
                            }
                            _ => {}
                        }
                    }
                    if has_image {
                        out.push(json!({ "role": "user", "content": blocks }));
                    } else {
                        let text: String = m.parts.iter().filter_map(|p| p.as_text()).collect::<Vec<_>>().join("\n");
                        out.push(json!({ "role": "user", "content": text }));
                    }
                }
                Role::Assistant => {
                    let text: String = m.parts.iter().filter_map(|p| p.as_text()).collect::<Vec<_>>().join("\n");
                    let tool_calls: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::ToolUse { id, name, input } => Some(json!({
                                "id": id,
                                "type": "function",
                                "function": { "name": name, "arguments": input.to_string() }
                            })),
                            _ => None,
                        })
                        .collect();
                    let mut msg = json!({ "role": "assistant", "content": if text.is_empty() { Value::Null } else { json!(text) } });
                    if !tool_calls.is_empty() {
                        msg["tool_calls"] = Value::Array(tool_calls);
                    }
                    out.push(msg);
                }
                Role::Tool => {
                    for p in &m.parts {
                        if let ContentPart::ToolResult { id, content, .. } = p {
                            out.push(json!({ "role": "tool", "tool_call_id": id, "content": content }));
                        }
                    }
                }
            }
        }
        Value::Array(out)
    }

    /// 关联函数而非 `&self`：流内部要 'static，不能捕获 self 借用
    fn parse_chunk(data: &str) -> Vec<StreamEvent> {
        let mut events = Vec::new();
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return events,
        };
        if let Some(err) = v.get("error") {
            events.push(StreamEvent::Error {
                message: err.get("message").and_then(|m| m.as_str()).unwrap_or("上游返回未知错误").to_string(),
                retryable: false,
            });
            return events;
        }
        if let Some(choice) = v.get("choices").and_then(|c| c.get(0)) {
            let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
            if let Some(text) = delta.get("content").and_then(text_value) {
                if !text.is_empty() {
                    events.push(StreamEvent::Text { delta: text });
                }
            }
            if let Some(t) = delta.get("reasoning_content").and_then(|x| x.as_str()) {
                if !t.is_empty() {
                    events.push(StreamEvent::Reasoning { delta: t.to_string() });
                }
            }
            if let Some(calls) = delta.get("tool_calls").and_then(|c| c.as_array()) {
                for (fallback_idx, call) in calls.iter().enumerate() {
                    let index = call.get("index").and_then(|i| i.as_u64()).unwrap_or(fallback_idx as u64) as u32;
                    let id = call.get("id").and_then(|i| i.as_str()).unwrap_or("");
                    let name = call
                        .get("function")
                        .and_then(|f| f.get("name"))
                        .and_then(|n| n.as_str())
                        .unwrap_or("");
                    if !name.is_empty() {
                        events.push(StreamEvent::ToolCallStart { index, id: id.to_string(), name: name.to_string() });
                    }
                    if let Some(args) = call.get("function").and_then(|f| f.get("arguments")).and_then(|a| a.as_str()) {
                        if !args.is_empty() {
                            events.push(StreamEvent::ToolCallArgs { index, delta: args.to_string() });
                        }
                    }
                }
            }
            if let Some(reason) = choice.get("finish_reason").and_then(|r| r.as_str()) {
                events.push(StreamEvent::Finish { reason: map_finish(reason) });
            }
        }
        // usage 放在帧末尾，保证 UI 先看到 finish 再结算
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            events.push(StreamEvent::Usage {
                usage: Usage {
                    input_tokens: u.get("prompt_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                    output_tokens: u.get("completion_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                    cache_read_tokens: u
                        .get("prompt_tokens_details")
                        .and_then(|d| d.get("cached_tokens"))
                        .and_then(|x| x.as_u64())
                        .unwrap_or(0),
                    ..Default::default()
                },
            });
        }
        events
    }
}

/// OpenAI 的 content 既可能是字符串，也可能是 `[{type:text,text}]` 数组
fn text_value(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.to_string());
    }
    if let Some(arr) = v.as_array() {
        let joined = arr
            .iter()
            .filter_map(|p| p.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

fn map_finish(reason: &str) -> FinishReason {
    match reason {
        "stop" => FinishReason::Stop,
        "length" => FinishReason::Length,
        "tool_calls" | "function_call" => FinishReason::ToolCalls,
        "content_filter" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_string()),
    }
}

#[async_trait]
impl Provider for OpenAiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            kind: self.kind,
            label: self.label.clone(),
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            capabilities: Capabilities { tools: true, vision: true, reasoning: true, stream_usage: true },
            models: self.models.clone(),
        }
    }

    fn pricing(&self, model: &str) -> Pricing {
        let _ = model;
        self.pricing.clone()
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<EventStream> {
        let body = self.build_body(&req);
        let mut builder = self.client.post(self.endpoint()).json(&body).header("Content-Type", "application/json");
        if let Some((k, v)) = self.auth_header() {
            builder = builder.header(k, v);
        }
        for (k, v) in &self.extra_headers {
            builder = builder.header(k, v);
        }
        let resp = builder.send().await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                401 | 403 => Error::Auth(format!("鉴权失败({status}): {body}")),
                429 => Error::RateLimited { retry_after_ms: 3000 },
                s if s >= 500 => Error::Upstream { status: s, body, retryable: true },
                s => Error::Upstream { status: s, body, retryable: false },
            });
        }

        let model = req.model.clone();
        let provider = self.id.clone();
        let sse = parse_sse(resp.bytes_stream());
        let stream = async_stream::stream! {
            tokio::pin!(sse);
            yield Ok(StreamEvent::Start { provider, model });
            while let Some(ev) = sse.next().await {
                let ev = match ev { Ok(e) => e, Err(e) => { yield Err(e); return; } };
                if ev.is_done() { break; }
                for out in Self::parse_chunk(&ev.data) {
                    yield Ok(out);
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelCard>> {
        let url = match self.kind {
            ProviderKind::AzureOpenAi => return Ok(self.models.clone()),
            _ => format!("{}/models", self.base_url),
        };
        let mut builder = self.client.get(url);
        if let Some((k, v)) = self.auth_header() {
            builder = builder.header(k, v);
        }
        let resp = builder.send().await?;
        if !resp.status().is_success() {
            // 很多兼容网关没有 /models，回退静态清单而不是报错
            return Ok(self.models.clone());
        }
        let v: Value = resp.json().await?;
        let list = v.get("data").and_then(|d| d.as_array()).cloned().unwrap_or_default();
        let mut cards: Vec<ModelCard> = list
            .iter()
            .filter_map(|m| {
                let id = m.get("id")?.as_str()?.to_string();
                Some(ModelCard {
                    display_name: id.clone(),
                    id,
                    context_window: 0,
                    supports_tools: true,
                    supports_vision: true,
                    supports_reasoning: false,
                })
            })
            .collect();
        cards.sort_by(|a, b| a.id.cmp(&b.id));
        if cards.is_empty() {
            cards = self.models.clone();
        }
        Ok(cards)
    }

    async fn health(&self) -> Result<()> {
        let url = match self.kind {
            ProviderKind::AzureOpenAi => self.base_url.clone(),
            _ => format!("{}/models", self.base_url),
        };
        let mut builder = self.client.get(url);
        if let Some((k, v)) = self.auth_header() {
            builder = builder.header(k, v);
        }
        let resp = builder.send().await?;
        match resp.status().as_u16() {
            200..=299 => Ok(()),
            401 | 403 => Err(Error::Auth("API Key 无效或无权限".into())),
            s => Err(Error::Upstream { status: s, body: "健康检查失败".into(), retryable: false }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ChatRequest, ContentPart, Message, Role};
    use serde_json::json;

    fn provider() -> OpenAiProvider {
        OpenAiProvider::new("openai", "OpenAI", "https://api.openai.com/v1", "sk-x", ProviderKind::OpenAi, "gpt-4o", vec![], Pricing::default())
    }

    #[test]
    fn maps_tool_call_roundtrip() {
        let p = provider();
        let mut req = ChatRequest::new("gpt-4o", vec![
            Message::user("读一下 README"),
            Message::new(Role::Assistant, vec![
                ContentPart::text("我来读取"),
                ContentPart::tool_use("call_1", "fs_read", json!({"path":"README.md"})),
            ]),
            Message::new(Role::Tool, vec![ContentPart::tool_result("call_1", "fs_read", "内容", false)]),
        ]);
        req.system = Some("你是助手".into());
        let body = p.build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "fs_read");
        assert_eq!(msgs[3]["role"], "tool");
        assert_eq!(msgs[3]["tool_call_id"], "call_1");
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn parses_text_and_tool_call_deltas() {
        let evs = OpenAiProvider::parse_chunk(r#"{"choices":[{"delta":{"content":"你好"}}]}"#);
        assert!(matches!(&evs[0], StreamEvent::Text { delta } if delta == "你好"));

        let evs = OpenAiProvider::parse_chunk(r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"fs_read","arguments":"{\"path\":"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":10,"completion_tokens":2}}"#);
        assert!(matches!(&evs[0], StreamEvent::ToolCallStart { name, .. } if name == "fs_read"));
        assert!(matches!(&evs[1], StreamEvent::ToolCallArgs { .. }));
        assert!(matches!(&evs[2], StreamEvent::Finish { reason: FinishReason::ToolCalls }));
    }
}
