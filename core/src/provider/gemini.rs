//! Gemini `generateContent` 适配器。
//!
//! 三个差异点：
//! 1. 角色只有 user / model，没有 assistant / tool；
//! 2. 工具调用是 `functionCall`，结果用 `functionResponse` 塞回 user turn；
//! 3. 函数参数不是增量 JSON，而是一次性完整对象，这里合成 Start + 一次 Args 事件。

use async_trait::async_trait;
use futures_util::StreamExt;
use serde_json::{json, Value};

use super::{parse_sse, EventStream, Provider, ProviderInfo, ProviderKind};
use crate::model::*;
use crate::{Error, Result};

pub struct GeminiProvider {
    id: String,
    label: String,
    base_url: String,
    api_key: String,
    default_model: String,
    models: Vec<ModelCard>,
    pricing: Pricing,
    client: reqwest::Client,
}

impl GeminiProvider {
    pub fn new(
        id: impl Into<String>,
        label: impl Into<String>,
        base_url: impl Into<String>,
        api_key: impl Into<String>,
        default_model: impl Into<String>,
        models: Vec<ModelCard>,
        pricing: Pricing,
    ) -> Self {
        GeminiProvider {
            id: id.into(),
            label: label.into(),
            base_url: base_url.into().trim_end_matches('/').to_string(),
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
        let contents = self.map_contents(req);
        let mut body = json!({ "contents": contents });
        let system = req.system.clone().or_else(|| {
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
            body["systemInstruction"] = json!({ "parts": [{ "text": sys }] });
        }
        if !req.tools.is_empty() {
            body["tools"] = json!([{
                "functionDeclarations": req.tools.iter().map(|t| json!({
                    "name": t.name,
                    "description": t.description,
                    "parameters": t.parameters,
                })).collect::<Vec<_>>()
            }]);
        }
        let mut gen = json!({});
        if let Some(t) = req.temperature {
            gen["temperature"] = json!(t);
        }
        if let Some(m) = req.max_tokens {
            gen["maxOutputTokens"] = json!(m);
        }
        if !req.stop.is_empty() {
            gen["stopSequences"] = json!(req.stop);
        }
        if gen.as_object().map(|o| !o.is_empty()).unwrap_or(false) {
            body["generationConfig"] = gen;
        }
        body
    }

    fn map_contents(&self, req: &ChatRequest) -> Value {
        let mut contents: Vec<Value> = Vec::new();
        for m in &req.messages {
            match m.role {
                Role::System => {}
                Role::User => {
                    let parts: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(json!({ "text": text })),
                            ContentPart::Image { mime, data_b64 } => Some(json!({
                                "inlineData": { "mimeType": mime, "data": data_b64 }
                            })),
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        contents.push(json!({ "role": "user", "parts": parts }));
                    }
                }
                Role::Assistant => {
                    let parts: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => {
                                if text.trim().is_empty() { None } else { Some(json!({ "text": text })) }
                            }
                            ContentPart::ToolUse { name, input, .. } => {
                                Some(json!({ "functionCall": { "name": name, "args": input } }))
                            }
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        contents.push(json!({ "role": "model", "parts": parts }));
                    }
                }
                Role::Tool => {
                    let parts: Vec<Value> = m
                        .parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::ToolResult { name, content, is_error, .. } => {
                                let mut resp = json!({ "content": content });
                                if *is_error {
                                    resp["error"] = json!(content);
                                }
                                Some(json!({ "functionResponse": { "name": name, "response": resp } }))
                            }
                            _ => None,
                        })
                        .collect();
                    if !parts.is_empty() {
                        contents.push(json!({ "role": "user", "parts": parts }));
                    }
                }
            }
        }
        Value::Array(contents)
    }

    fn parse_chunk(data: &str, counter: &mut u32) -> Vec<StreamEvent> {
        let v: Value = match serde_json::from_str(data) {
            Ok(v) => v,
            Err(_) => return vec![],
        };
        if let Some(err) = v.get("error") {
            return vec![StreamEvent::Error {
                message: err.get("message").and_then(|m| m.as_str()).unwrap_or("Gemini 上游错误").to_string(),
                retryable: err.get("code").and_then(|c| c.as_u64()) == Some(503),
            }];
        }
        let mut out = Vec::new();
        let mut finish: Option<FinishReason> = None;
        if let Some(cand) = v.get("candidates").and_then(|c| c.get(0)) {
            if let Some(parts) = cand.pointer("/content/parts").and_then(|p| p.as_array()) {
                for (idx, part) in parts.iter().enumerate() {
                    if let Some(t) = part.get("text").and_then(|t| t.as_str()) {
                        if !t.is_empty() {
                            out.push(StreamEvent::Text { delta: t.to_string() });
                        }
                    }
                    if let Some(fc) = part.get("functionCall") {
                        let name = fc.get("name").and_then(|n| n.as_str()).unwrap_or("unknown").to_string();
                        let args = fc.get("args").cloned().unwrap_or(json!({}));
                        let index = *counter;
                        *counter += 1;
                        out.push(StreamEvent::ToolCallStart {
                            index,
                            id: format!("gemini_{index}_{name}"),
                            name,
                        });
                        out.push(StreamEvent::ToolCallArgs { index, delta: args.to_string() });
                    }
                    let _ = idx;
                }
            }
            if let Some(reason) = cand.get("finishReason").and_then(|r| r.as_str()) {
                finish = Some(match reason {
                    "STOP" => FinishReason::Stop,
                    "MAX_TOKENS" => FinishReason::Length,
                    "SAFETY" | "RECITATION" => FinishReason::ContentFilter,
                    other => FinishReason::Other(other.to_string()),
                });
            }
        }
        // Gemini 的 usageMetadata 是累加值，只在最后一个 chunk 采信，避免重复计数
        if finish.is_some() {
            if let Some(meta) = v.get("usageMetadata") {
                out.push(StreamEvent::Usage {
                    usage: Usage {
                        input_tokens: meta.get("promptTokenCount").and_then(|x| x.as_u64()).unwrap_or(0),
                        output_tokens: meta.get("candidatesTokenCount").and_then(|x| x.as_u64()).unwrap_or(0),
                        cache_read_tokens: meta.get("cachedContentTokenCount").and_then(|x| x.as_u64()).unwrap_or(0),
                        reasoning_tokens: meta.get("thoughtsTokenCount").and_then(|x| x.as_u64()).unwrap_or(0),
                        ..Default::default()
                    },
                });
            }
        }
        if let Some(reason) = finish {
            out.push(StreamEvent::Finish { reason });
        }
        out
    }

    fn endpoint(&self, model: &str) -> String {
        format!("{}/v1beta/models/{}:streamGenerateContent?alt=sse", self.base_url, model)
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            id: self.id.clone(),
            kind: ProviderKind::Gemini,
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
            .post(self.endpoint(&req.model))
            .header("x-goog-api-key", &self.api_key)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            return Err(match status {
                401 | 403 => Error::Auth(format!("Gemini 鉴权失败({status}): {body}")),
                429 => Error::RateLimited { retry_after_ms: 5000 },
                s if s >= 500 => Error::Upstream { status: s, body, retryable: true },
                s => Error::Upstream { status: s, body, retryable: false },
            });
        }
        let provider_id = self.id.clone();
        let model = req.model.clone();
        let sse = parse_sse(resp.bytes_stream());
        let stream = async_stream::stream! {
            tokio::pin!(sse);
            let mut counter: u32 = 0;
            yield Ok(StreamEvent::Start { provider: provider_id, model });
            while let Some(ev) = sse.next().await {
                let ev = match ev { Ok(e) => e, Err(e) => { yield Err(e); return; } };
                if ev.is_done() { break; }
                for out in Self::parse_chunk(&ev.data, &mut counter) {
                    yield Ok(out);
                }
            }
        };
        Ok(Box::pin(stream))
    }

    async fn list_models(&self) -> Result<Vec<ModelCard>> {
        let resp = self
            .client
            .get(format!("{}/v1beta/models", self.base_url))
            .header("x-goog-api-key", &self.api_key)
            .send()
            .await?;
        if !resp.status().is_success() {
            return Ok(self.models.clone());
        }
        let v: Value = resp.json().await?;
        let mut cards: Vec<ModelCard> = v
            .get("models")
            .and_then(|m| m.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| {
                        let raw = m.get("name")?.as_str()?.to_string();
                        let id = raw.trim_start_matches("models/").to_string();
                        if !id.contains("gemini") {
                            return None;
                        }
                        Some(ModelCard {
                            display_name: m.get("displayName").and_then(|d| d.as_str()).unwrap_or(&id).to_string(),
                            id,
                            context_window: m.get("inputTokenLimit").and_then(|x| x.as_u64()).unwrap_or(0) as u32,
                            supports_tools: true,
                            supports_vision: true,
                            supports_reasoning: true,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        cards.sort_by(|a, b| a.id.cmp(&b.id));
        if cards.is_empty() { cards = self.models.clone(); }
        Ok(cards)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> GeminiProvider {
        GeminiProvider::new("gemini", "Gemini", "https://generativelanguage.googleapis.com", "k", "gemini-2.5-pro", vec![], Pricing::default())
    }

    #[test]
    fn maps_function_call_and_response() {
        let p = provider();
        let req = ChatRequest::new("gemini-2.5-pro", vec![
            Message::user("hi"),
            Message::new(Role::Assistant, vec![ContentPart::tool_use("g1", "fs_read", json!({"path":"a"}))]),
            Message::new(Role::Tool, vec![ContentPart::tool_result("g1", "fs_read", "ok", false)]),
        ]);
        let body = p.build_body(&req);
        let c = body["contents"].as_array().unwrap();
        assert_eq!(c[1]["role"], "model");
        assert!(c[1]["parts"][0]["functionCall"]["name"] == "fs_read");
        assert_eq!(c[2]["role"], "user");
        assert!(c[2]["parts"][0]["functionResponse"]["name"] == "fs_read");
    }

    #[test]
    fn emits_atomic_function_call() {
        let mut n = 0;
        let data = r#"{"candidates":[{"content":{"parts":[{"functionCall":{"name":"fs_read","args":{"path":"x"}}}]},"finishReason":"STOP"}]}"#;
        let evs = GeminiProvider::parse_chunk(data, &mut n);
        assert!(matches!(&evs[0], StreamEvent::ToolCallStart { name, .. } if name == "fs_read"));
        assert!(matches!(&evs[1], StreamEvent::ToolCallArgs { delta, .. } if delta.contains("\"path\"")));
        assert_eq!(n, 1);
    }
}
