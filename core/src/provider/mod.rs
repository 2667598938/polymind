//! 供应商适配层。
//!
//! 每个适配器只做三件事：翻译请求、解析流、暴露能力矩阵。
//! Agent 循环永远拿到统一的 `StreamEvent`，因此"换模型"不改变上层逻辑。

use async_trait::async_trait;
use futures_util::{Stream, StreamExt};
use serde::{Deserialize, Serialize};
use std::pin::Pin;

use crate::model::*;
use crate::{Error, Result};

pub mod anthropic;
pub mod gemini;
pub mod openai;
pub mod registry;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use openai::OpenAiProvider;
pub use registry::Registry;

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent>> + Send>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderKind {
    /// OpenAI 及所有 `/chat/completions` 兼容网关：DeepSeek、Moonshot、智谱、
    /// 豆包 Ark、通义 DashScope 兼容模式、OpenRouter、one-api/new-api 自建网关
    #[serde(rename = "openai")]
    OpenAi,
    #[serde(rename = "anthropic")]
    Anthropic,
    #[serde(rename = "gemini")]
    Gemini,
    #[serde(rename = "azure_openai")]
    AzureOpenAi,
    #[serde(rename = "ollama")]
    Ollama,
}

impl ProviderKind {
    pub fn default_base_url(&self) -> &'static str {
        match self {
            ProviderKind::OpenAi => "https://api.openai.com/v1",
            ProviderKind::Anthropic => "https://api.anthropic.com",
            ProviderKind::Gemini => "https://generativelanguage.googleapis.com",
            ProviderKind::AzureOpenAi => "https://{resource}.openai.azure.com/openai/deployments/{deployment}",
            ProviderKind::Ollama => "http://127.0.0.1:11434/v1",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            ProviderKind::OpenAi => "OpenAI 兼容",
            ProviderKind::Anthropic => "Anthropic",
            ProviderKind::Gemini => "Google Gemini",
            ProviderKind::AzureOpenAi => "Azure OpenAI",
            ProviderKind::Ollama => "本地 Ollama",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub id: String,
    pub kind: ProviderKind,
    pub label: String,
    pub base_url: String,
    pub default_model: String,
    pub capabilities: Capabilities,
    pub models: Vec<ModelCard>,
}

#[async_trait]
pub trait Provider: Send + Sync {
    fn info(&self) -> ProviderInfo;

    /// 单价表（美元/百万 token），未知模型返回 0，UI 会显示"未配置价格"
    fn pricing(&self, model: &str) -> Pricing;

    async fn stream_chat(&self, req: ChatRequest) -> Result<EventStream>;

    /// 真实拉取模型列表；上游不支持时回退到配置内的静态清单
    async fn list_models(&self) -> Result<Vec<ModelCard>> {
        Ok(self.info().models)
    }

    /// 连通性检查（不消耗生成额度）
    async fn health(&self) -> Result<()> {
        Ok(())
    }
}

#[derive(Debug, Clone, Default)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseEvent {
    pub fn is_done(&self) -> bool {
        self.data.trim() == "[DONE]"
    }
}

/// 极简 SSE 解析器：按空行切帧，拼接多行 data。
/// 自己实现而不引第三方，是为了能独立单测，也便于后续换成 msgpack 网关。
pub fn parse_sse<S>(byte_stream: S) -> impl Stream<Item = Result<SseEvent>> + Send
where
    S: Stream<Item = std::result::Result<bytes::Bytes, reqwest::Error>> + Send + 'static,
{
    async_stream::stream! {
        let mut buf: Vec<u8> = Vec::with_capacity(8192);
        let mut byte_stream = Box::pin(byte_stream);
        while let Some(chunk) = byte_stream.next().await {
            let chunk = match chunk {
                Ok(c) => c,
                Err(e) => { yield Err(Error::Http(e.to_string())); return; }
            };
            buf.extend_from_slice(&chunk);
            while let Some((end, skip)) = find_frame_boundary(&buf) {
                let raw = buf[..end].to_vec();
                buf.drain(..end + skip);
                let text = String::from_utf8_lossy(&raw).into_owned();
                let mut event: Option<String> = None;
                let mut data_lines: Vec<String> = Vec::new();
                for line in text.lines() {
                    if line.starts_with(':') || line.trim().is_empty() {
                        continue;
                    }
                    if let Some(rest) = line.strip_prefix("event:") {
                        event = Some(rest.trim().to_string());
                    } else if let Some(rest) = line.strip_prefix("data:") {
                        data_lines.push(rest.strip_prefix(' ').unwrap_or(rest).to_string());
                    }
                }
                if data_lines.is_empty() {
                    continue;
                }
                yield Ok(SseEvent { event, data: data_lines.join("\n") });
            }
        }
    }
}

fn find_frame_boundary(buf: &[u8]) -> Option<(usize, usize)> {
    // 优先匹配 CRLF CRLF
    if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
        return Some((pos, 4));
    }
    buf.windows(2).position(|w| w == b"\n\n").map(|p| (p, 2))
}

/// 通用的可重试包装：429/5xx 指数退避，最多 `attempts` 次。
/// 只在"还没有吐出任何 token"时重试，避免同一个回答被打断两次。
pub async fn with_retry<F, Fut, T>(attempts: u32, mut f: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    let mut last = None;
    for i in 0..attempts.max(1) {
        match f().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let retryable = e.retryable();
                last = Some(e);
                if !retryable || i + 1 == attempts {
                    break;
                }
                let backoff = 400u64 * (1 << i.min(4));
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            }
        }
    }
    Err(last.unwrap_or_else(|| Error::Other("重试耗尽".into())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures_util::stream;

    #[tokio::test]
    async fn sse_parses_crlf_and_multiline_data() {
        let raw = "event: message_start\r\ndata: {\"a\":1}\r\n\r\ndata: line1\ndata: line2\n\n";
        let bytes = bytes::Bytes::from_static(raw.as_bytes());
        let s = parse_sse(stream::iter(vec![Ok::<_, reqwest::Error>(bytes)]));
        let events: Vec<SseEvent> = s.map(|e| e.unwrap()).collect().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event.as_deref(), Some("message_start"));
        assert_eq!(events[0].data, "{\"a\":1}");
        assert_eq!(events[1].data, "line1\nline2");
    }

    #[tokio::test]
    async fn sse_handles_split_frames_across_chunks() {
        let a = bytes::Bytes::from_static(b"data: {\"x\"");
        let b = bytes::Bytes::from_static(b":1}\n\n");
        let s = parse_sse(stream::iter(vec![Ok::<_, reqwest::Error>(a), Ok(b)]));
        let events: Vec<SseEvent> = s.map(|e| e.unwrap()).collect().await;
        assert_eq!(events[0].data, "{\"x\":1}");
    }
}
