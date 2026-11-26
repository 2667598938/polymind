//! Anthropic Messages API 适配器（Claude 系）。
//!
//! 与 OpenAI 的三个硬差异：
//! 1. `system` 是顶层字段，不是 messages 里的一条；
//! 2. `max_tokens` 必填；
//! 3. 工具结果以 user 消息里的 `tool_result` block 回传，且要求 user/assistant 交替。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::{parse_sse, EventStream, Provider, ProviderInfo, ProviderKind};
use crate::model::*;
use crate::{Error, Result};

pub struct AnthropicProvider {
    id: String,
    label: String,
    base_url: String,
    api_key: String,
    default_model: String,
    models: Vec<ModelCard>,
    pricing: Pricing,
    client: reqwest::Client,
}

impl AnthropicProvider {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        default_model: impl Into<String>,
        models: Vec<ModelCard>,
        pricing: Pricing,
    ) -> Self {
        let base = base_url.into().trim_end_matches('/').to_string();
        let base = if base.ends_with("/v1") { base } else { format!("{base}/v1") };
        AnthropicProvider {
            id: id.into(),
            label: label.into(),
            base_url: base,
            api_key: api_key.into(),
            default_model: default_model.into(),
            models,
            pricing,
            client: reqwest::Client::builder()
                .connect_timeout(std::time::Duration::from_secs(15))
                .timeout(std::time::Duration::from_secs(600))
                .build()
                .unwrap_or_default(),
        }
    }

    fn build_body(&self, req: &ChatRequest) -> Value {
        let messages = self.map_messages(req);
        let mut body = json!({
            "model": req.model,
            "max_tokens": req.max_tokens.unwrap_or(8192),
            "messages": messages,
            "stream": true,
        });
        let system = req
            .system
            .clone()
            .or_else(|| {
                let collected: String = req
                    .messages
                    .iter()
                    .filter(|m| m.role == Role::System)
                    .map(|m| m.plain_text())
                    .collect::<Vec<_>>()
                    .join("\n");
                if collected.trim().is_empty() { None } else { Some(collected) }
            });
        if let Some(sys) = system {
            body["system"] = json!(sys);
        }
        if !req.tools.is_empty() {
            body["tools"] = Value::Array(
                req.tools
                    .iter()
                    .map(|t| json!({
                        "name": t.name,
                        "description": t.description,
                        "input_schema": t.parameters,
                    }))
                    .collect(),
            );
        }
        if let Some(t) = req.temperature {
            body["temperature"] = json!(t);
        }
        if !req.stop.is_empty() {
            body["stop_sequences"] = json!(req.stop);
        }
        body
    }

    fn map_messages(&self, req: &ChatRequest) -> Value {
        // Anthropic 要求 user/assistant 交替出现，连续同角色要合并成一个 content 数组
        let mut out: Vec<Value> = Vec::new();
        let mut push = |role: &str, blocks: Vec<Value>| {
            if blocks.is_empty() {
                return;
            }
            if let Some(last) = out.last_mut() {
                if last["role"] == role {
                    if let Some(arr) = last["content"].as_array_mut() {
                        arr.extend(blocks);
                        return;
                    }
                }
            }
            out.push(json!({ "role": role, "content": blocks }));
        };

        for m in &req.messages {
            match m.role {
                Role::System => {}
                Role::User => {
                    let blocks: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(json!({ "type": "text", "text": text })),
                            ContentPart::Image { mime, data_b64 } => Some(json!({
                                "type": "image",
                                "source": { "type": "base64", "media_type": mime, "data": data_b64 }
                            })),
                            _ => None,
                        })
                        .collect();
                    push("user", blocks);
                }
                Role::Assistant => {
                    let blocks: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => {
                                if text.trim().is_empty() { None } else { Some(json!({ "type": "text", "text": text })) }
                            }
                            ContentPart::ToolUse { id, name, input } => Some(json!({
                                "type": "tool_use", "id": id, "name": name, "input": input
                            })),
                            _ => None,
                        })
                        .collect();
                    push("assistant", blocks);
                }
                Role::Tool => {
                    let blocks: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::ToolResult { id, content, is_error, .. } => Some(json!({
                                "type": "tool_result",
                                "tool_use_id": id,
                                "content": content,
                                "is_error": is_error,
                            })),
                            _ => None,
                        })
                        .collect();
                    push("user", blocks);
                }
            }
        }
        Value::Array(out)
    }

    fn parse_stream_event(event: Option<&str>, data: &str) -> Vec<StreamEvent> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        let kind = event.map(|s| s.to_string()).or_else(|| v.get("type").and_then(|t| t.as_str()).map(|s| s.to_string())).unwrap_or_default();
        let mut out = Vec::new();
        match kind.as_str() {
            "message_start" => {
                if let Some(u) = v.pointer("/message/usage") {
                    out.push(StreamEvent::Usage {
                        usage: Usage {
                            input_tokens: u.get("input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                            cache_read_tokens: u.get("cache_read_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                            cache_write_tokens: u.get("cache_creation_input_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                            ..Default::default()
                        },
                    });
                }
            }
            "content_block_start" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                if let Some(block) = v.get("content_block") {
                    if block.get("type").and_then(|t| t.as_str()) == Some("tool_use") {
                        out.push(StreamEvent::ToolCallStart {
                            index,
                            id: block.get("id").and_then(|i| i.as_str()).unwrap_or("").to_string(),
                            name: block.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string(),
                        });
                    }
                }
            }
            "content_block_delta" => {
                let index = v.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as u32;
                if let Some(delta) = v.get("delta") {
                    match delta.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                        "text_delta" => {
                            if let Some(t) = delta.get("text").and_then(|t| t.as_str()) {
                                out.push(StreamEvent::Text { delta: t.to_string() });
                            }
                        }
                        "thinking_delta" => {
                            if let Some(t) = delta.get("thinking").and_then(|t| t.as_str()) {
                                out.push(StreamEvent::Reasoning { delta: t.to_string() });
                            }
                        }
                        "input_json_delta" => {
                            if let Some(t) = delta.get("partial_json").and_then(|t| t.as_str()) {
                                out.push(StreamEvent::ToolCallArgs { index, delta: t.to_string() });
                            }
                        }
                        _ => {}
                    }
                }
            }
            "message_delta" => {
                if let Some(u) = v.get("usage") {
                    out.push(StreamEvent::Usage {
                        usage: Usage {
                            output_tokens: u.get("output_tokens").and_then(|x| x.as_u64()).unwrap_or(0),
                            ..Default::default()
                        },
                    });
                }
                if let Some(reason) = v.pointer("/delta/stop_reason").and_then(|r| r.as_str()) {
                    out.push(StreamEvent::Finish { reason: map_stop(reason) });
                }
            }
            "message_stop" => {
                out.push(StreamEvent::Finish { reason: FinishReason::Stop });
            }
            "error" => {
                out.push(StreamEvent::Error {
                    message: v.pointer("/error/message").and_then(|m| m.as_str()).unwrap_or("Claude 上游错误").to_string(),
                    retryable: v.pointer("/error/type").and_then(|t| t.as_str()) == Some("overloaded_error"),
                });
            }
            _ => {}
        }
        out
    }
}

fn map_stop(reason: &str) -> FinishReason {
    match reason {
        "end_turn" | "stop_sequence" => FinishReason::Stop,
        "max_tokens" => FinishReason::Length,
        "tool_use" => FinishReason::ToolCalls,
        "refusal" => FinishReason::ContentFilter,
        other => FinishReason::Other(other.to_string()),
    }
}

#[async_trait]
impl Provider for AnthropicProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            kind: ProviderKind::Anthropic,
            label: self.label.clone(),
            base_url: self.base_url.clone(),
            default_model: self.default_model.clone(),
            capabilities: Capabilities { tools: true, vision: true, reasoning: true, stream_usage: true },
            models: self.models.clone(),
        }
    }

    fn pricing(&self, _model: &str) -> Pricing {
        self.pricing.clone()
    }

    async fn stream_chat(&self, req: ChatRequest) -> Result<EventStream> {
        let body = self.build_body(&req);
        let resp = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                401 | 403 => Error::Auth(format!("Claude 鉴权失败({status}): {body}")),
                429 => Error::RateLimited { retry_after_ms: 5000 },
                529 => Error::Upstream { status, body, retryable: true },
                s if s >= 500 => Error::Upstream { status: s, body, retryable: true },
                s => Error::Upstream { status: s, body, retryable: false },
            });
        }

        let provider_id = self.id.clone();
        let model = req.model.clone();
        let sse = parse_sse(resp.bytes_stream());
        let stream = async_stream::stream! {
            tokio::pin!(sse);
            yield Ok(StreamEvent::Start { provider: provider_id, model });
            while let Some(ev) = sse.next().await {
                let ev = match ev { Ok(e) => e, Err(e) => { yield Err(e); return; } };
                if ev.is_done() { break; }
                for out in Self::parse_stream_event(ev.event.as_deref(), &ev.data) {
                    yield Ok(out);
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelCard>> {
        // Anthropic 没有公开的 /models 列表（新版本有，但兼容旧账号时不稳定）
        Ok(self.models.clone())
    }

    async fn health(&self) -> Result<()> {
        let resp = self
            .client
            .post(format!("{}/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&json!({
                "model": self.default_model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "ping" }]
            }))
            .send()
            .await?;
        match resp.status().as_u16() {
            200..=299 | 400 => Ok(()),
            401 | 403 => Err(Error::Auth("Anthropic API Key 无效".into())),
            s => Err(Error::Upstream { status: s, body: "健康检查失败".into(), retryable: false }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> AnthropicProvider {
        AnthropicProvider::new("claude", "Claude", "https://api.anthropic.com", "sk-ant", "claude-sonnet-4-5", vec![], Pricing::default())
    }

    #[test]
    fn merges_consecutive_tool_result_into_user() {
        let p = provider();
        let req = ChatRequest::new("claude-sonnet-4-5", vec![
            Message::user("hi"),
            Message::new(Role::Assistant, vec![
                ContentPart::tool_use("t1", "fs_read", json!({"path":"a"})),
            ]),
            Message::new(Role::Tool, vec![ContentPart::tool_result("t1", "fs_read", "ok", false)]),
        ]);
        let body = p.build_body(&req);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 3);
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(body["max_tokens"], 8192);
    }

    #[test]
    fn parses_tool_use_stream() {
        let evs = AnthropicProvider::parse_stream_event(Some("content_block_start"), r#"{"index":0,"content_block":{"type":"tool_use","id":"toolu_1","name":"fs_read"}}"#);
        assert!(matches!(&evs[0], StreamEvent::ToolCallStart { name, .. } if name == "fs_read"));

        let evs = AnthropicProvider::parse_stream_event(Some("message_delta"), r#"{"delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":12}}"#);
        assert!(matches!(&evs[0], StreamEvent::Usage { usage } if usage.output_tokens == 12));
        assert!(matches!(&evs[1], StreamEvent::Finish { reason: FinishReason::ToolCalls }));
    }
}
