//! 网页抓取工具（只读）。
//!
//! 为了不变成 SSRF 跳板，默认拒绝本机/内网/云元数据地址；
//! HTML 只做"去标签 + 反转义 + 压空白"的轻量清洗，不引入完整 DOM 解析。

use async_trait::async_trait;
use regex::{Regex, RegexBuilder};
use serde_json::{json, Value};
use std::sync::OnceLock;
use std::time::Duration;

use super::{Tool, ToolCtx, ToolOutput};
use crate::agent::permission::RiskLevel;
use crate::model::{ToolSource, ToolSpec};
use crate::{Error, Result};

pub struct WebFetch;

fn tag_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| RegexBuilder::new(r"<[^>]+>").dot_matches_new_line(true).build().expect("正则常量"))
}

fn script_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    // Rust 的 regex 不支持反向引用，因此三个标签分别写一条分支
    RE.get_or_init(|| {
        RegexBuilder::new(r"(?is)<script\b[^>]*>.*?</script>|<style\b[^>]*>.*?</style>|<noscript\b[^>]*>.*?</noscript>")
            .dot_matches_new_line(true)
            .build()
            .expect("正则常量")
    })
}

fn ws_re() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"[ \t\f]+").expect("正则常量"))
}

#[async_trait]
impl Tool for WebFetch {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "web_fetch".into(),
            description: "抓取一个 http(s) 网页并返回清洗后的正文（自动拒绝内网/本机地址）。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "url": { "type": "string", "description": "以 http:// 或 https:// 开头" }
                },
                "required": ["url"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn reason(&self, args: &Value) -> String {
        format!("抓取网页 {}", args.get("url").and_then(|u| u.as_str()).unwrap_or("?"))
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let url = args.get("url").and_then(|u| u.as_str()).ok_or_else(|| Error::Tool("缺少 url".into()))?;
        validate_url(url)?;

        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(30))
            .user_agent("PolyMind/0.1 (+desktop agent)")
            .redirect(reqwest::redirect::Policy::limited(3))
            .build()
            .map_err(|e| Error::Tool(e.to_string()))?;
        let resp = client.get(url).send().await?;
        let status = resp.status();
        if !status.is_success() {
            return Ok(ToolOutput::err(format!("HTTP {} ({url})", status.as_u16())));
        }
        let ctype = resp.headers().get("content-type").and_then(|v| v.to_str().ok()).unwrap_or("").to_string();
        if ctype.contains("application/json") {
            let text = resp.text().await?;
            return Ok(ToolOutput::ok(format!("JSON 响应（{} 字符）：\n{}", text.len(), cap(&text, 10_000))));
        }
        if !ctype.is_empty() && !ctype.contains("text") {
            return Ok(ToolOutput::err(format!("不支持的 Content-Type: {ctype}")));
        }
        let body = resp.text().await?;
        let cleaned = html_to_text(&body);
        Ok(ToolOutput::ok(format!(
            "来源 {url}（HTTP {}，原始 {} 字符，清洗后 {} 字符）：\n{}",
            status.as_u16(),
            body.len(),
            cleaned.chars().count(),
            cap(&cleaned, 10_000)
        )))
    }
}

fn validate_url(url: &str) -> Result<()> {
    let lower = url.trim().to_lowercase();
    if !(lower.starts_with("http://") || lower.starts_with("https://")) {
        return Err(Error::PermissionDenied("只允许 http/https URL".into()));
    }
    let host_part = lower
        .trim_start_matches("https://")
        .trim_start_matches("http://")
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("");
    let host = host_part.split(':').next().unwrap_or("");
    let blocked = ["localhost", "127.0.0.1", "0.0.0.0", "::1", "169.254.169.254", "metadata.google.internal"];
    if blocked.contains(&host) || host.ends_with(".local") {
        return Err(Error::PermissionDenied(format!("禁止访问本机/内网地址: {host}")));
    }
    if host.starts_with("10.") || host.starts_with("192.168.") || host.starts_with("172.16.") || host.starts_with("172.17.") {
        return Err(Error::PermissionDenied(format!("禁止访问私有网段: {host}")));
    }
    Ok(())
}

pub fn html_to_text(html: &str) -> String {
    let without_scripts = script_re().replace_all(html, " ");
    let stripped = tag_re().replace_all(&without_scripts, " ");
    let decoded = stripped
        .replace("&nbsp;", " ")
        .replace("&amp;", "&")
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&#39;", "'");
    let mut lines: Vec<String> = decoded
        .lines()
        .map(|l| ws_re().replace_all(l, " ").trim().to_string())
        .filter(|l| !l.is_empty())
        .collect();
    // 去掉连续重复行，网页导航栏常造成大量重复
    lines.dedup();
    lines.join("\n")
}

fn cap(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let cut: String = s.chars().take(max).collect();
    format!("{cut}\n…（已截断）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn blocks_ssrf_targets() {
        assert!(validate_url("http://127.0.0.1:8080/admin").is_err());
        assert!(validate_url("http://169.254.169.254/latest/meta-data").is_err());
        assert!(validate_url("file:///etc/passwd").is_err());
        assert!(validate_url("https://example.com/a").is_ok());
    }

    #[test]
    fn strips_tags_and_entities() {
        let html = "<html><head><style>a{}</style></head><body><h1>Hi</h1><p>a &amp; b</p><script>var x=1</script></body></html>";
        let text = html_to_text(html);
        assert!(text.contains("Hi"));
        assert!(text.contains("a & b"));
        assert!(!text.contains("var x"));
        assert!(!text.contains("<h1"));
    }
}
