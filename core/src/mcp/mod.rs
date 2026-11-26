//! MCP（Model Context Protocol）客户端：JSON-RPC 2.0 over stdio。
//!
//! 为什么自己实现而不直接引 SDK：
//! - MCP 传输层就三件事：写一行 JSON、读一行 JSON、按 id 配对；
//! - 自己实现可以精确控制超时、进程回收与错误面，排查问题不依赖黑盒；
//! - 供应商层已经统一抽象，MCP 工具只要实现 `Tool` 就自动进 Agent 工具循环。
//!
//! 支持：initialize 握手、tools/list（含 cursor 翻页）、tools/call。
//! 暂不支持：resources/prompts、SSE 远程传输（路线图 V1.2）。

use async_trait::async_trait;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, ChildStdin, Command};
use tokio::sync::{oneshot, Mutex};

use crate::agent::permission::RiskLevel;
use crate::agent::tools::{Tool, ToolCtx, ToolOutput, ToolRegistry};
use crate::config::McpServerConfig;
use crate::model::{ToolSource, ToolSpec};
use crate::{Error, Result};

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value>>>>>;

pub struct McpClient {
    pub name: String,
    child: Child,
    stdin: ChildStdin,
    pending: Pending,
    next_id: u64,
}

impl McpClient {
    pub async fn connect(cfg: &McpServerConfig) -> Result<Self> {
        let mut cmd = Command::new(&cfg.command);
        cmd.args(&cfg.args)
            .envs(cfg.env.iter())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = cmd
            .spawn()
            .map_err(|e| Error::Mcp(format!("启动 MCP 服务 `{}` 失败: {e}", cfg.name)))?;
        let stdin = child.stdin.take().ok_or_else(|| Error::Mcp("MCP stdin 不可用".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Mcp("MCP stdout 不可用".into()))?;
        let stderr = child.stderr.take();

        let pending: Pending = Arc::new(Mutex::new(HashMap::new()));
        let pending_reader = pending.clone();
        let server_name = cfg.name.clone();
        tokio::spawn(async move {
            let mut lines = BufReader::new(stdout).lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.trim().is_empty() {
                            continue;
                        }
                        let v: Value = match serde_json::from_str(&line) {
                            Ok(v) => v,
                            Err(e) => {
                                tracing::debug!("[{server_name}] 非 JSON 输出: {e}");
                                continue;
                            }
                        };
                        let Some(id) = v.get("id").and_then(|i| i.as_u64()) else {
                            // 通知（日志、进度）忽略；V1.2 接 progress 事件
                            continue;
                        };
                        let tx = pending_reader.lock().await.remove(&id);
                        if let Some(tx) = tx {
                            let payload = if let Some(err) = v.get("error") {
                                Err(Error::Mcp(format!(
                                    "[{}] {}",
                                    server_name,
                                    err.get("message").and_then(|m| m.as_str()).unwrap_or("MCP 返回 error")
                                )))
                            } else {
                                Ok(v.get("result").cloned().unwrap_or(Value::Null))
                            };
                            let _ = tx.send(payload);
                        }
                    }
                    Ok(None) => {
                        // 子进程退出：让所有等待者立刻失败，而不是干等超时
                        let mut map = pending_reader.lock().await;
                        for (_, tx) in map.drain() {
                            let _ = tx.send(Err(Error::Mcp(format!("[{server_name}] 服务进程已退出"))));
                        }
                        break;
                    }
                    Err(e) => {
                        tracing::warn!("[{server_name}] 读取失败: {e}");
                        break;
                    }
                }
            }
        });

        if let Some(stderr) = stderr {
            let server_name = cfg.name.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    tracing::debug!("[{server_name}] stderr: {line}");
                }
            });
        }

        let mut client = McpClient { name: cfg.name.clone(), child, stdin, pending, next_id: 1 };
        client.initialize().await?;
        Ok(client)
    }

    async fn write_line(&mut self, v: &Value) -> Result<()> {
        let line = serde_json::to_string(v)?;
        self.stdin.write_all(format!("{line}\n").as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    async fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);
        let payload = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        if let Err(e) = self.write_line(&payload).await {
            self.pending.lock().await.remove(&id);
            return Err(e);
        }
        match tokio::time::timeout(std::time::Duration::from_secs(30), rx).await {
            Ok(Ok(res)) => res,
            Ok(Err(_)) => Err(Error::Mcp(format!("[{}] 响应通道关闭", self.name))),
            Err(_) => {
                self.pending.lock().await.remove(&id);
                Err(Error::Mcp(format!("[{}] `{method}` 超时 30s", self.name)))
            }
        }
    }

    async fn notify(&mut self, method: &str, params: Value) -> Result<()> {
        self.write_line(&json!({ "jsonrpc": "2.0", "method": method, "params": params })).await
    }

    async fn initialize(&mut self) -> Result<()> {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": "2024-11-05",
                    "capabilities": { "roots": {}, "sampling": {} },
                    "clientInfo": { "name": "PolyMind", "version": env!("CARGO_PKG_VERSION") }
                }),
            )
            .await?;
        tracing::info!(
            "[{}] MCP 握手完成: {}",
            self.name,
            result.get("serverInfo").cloned().unwrap_or(serde_json::Value::Null)
        );
        let _ = self.notify("notifications/initialized", json!({})).await;
        Ok(())
    }

    /// 返回 `(原始工具名, ToolSpec)`
    pub async fn list_tools(&mut self) -> Result<Vec<(String, ToolSpec)>> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..5 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let result = self.request("tools/list", params).await?;
            if let Some(tools) = result.get("tools").and_then(|t| t.as_array()) {
                for t in tools {
                    let name = t.get("name").and_then(|n| n.as_str()).unwrap_or("").to_string();
                    if name.is_empty() {
                        continue;
                    }
                    out.push((
                        name.clone(),
                        ToolSpec {
                            name,
                            description: t.get("description").and_then(|d| d.as_str()).unwrap_or("MCP 工具").to_string(),
                            parameters: t.get("inputSchema").cloned().unwrap_or_else(|| json!({"type":"object","properties":{}})),
                            source: ToolSource::Mcp,
                        },
                    ));
                }
            }
            match result.get("nextCursor").and_then(|c| c.as_str()) {
                Some(c) if !c.is_empty() => cursor = Some(c.to_string()),
                _ => break,
            }
        }
        Ok(out)
    }

    pub async fn call_tool(&mut self, tool: &str, args: Value) -> Result<ToolOutput> {
        let result = self.request("tools/call", json!({ "name": tool, "arguments": args })).await?;
        let is_error = result.get("isError").and_then(|e| e.as_bool()).unwrap_or(false);
        let mut texts: Vec<String> = Vec::new();
        if let Some(content) = result.get("content").and_then(|c| c.as_array()) {
            for item in content {
                match item.get("type").and_then(|t| t.as_str()).unwrap_or("") {
                    "text" => {
                        if let Some(t) = item.get("text").and_then(|t| t.as_str()) {
                            texts.push(t.to_string());
                        }
                    }
                    "resource" => {
                        texts.push(format!("[resource] {}", item.get("resource").cloned().unwrap_or(Value::Null)));
                    }
                    other => texts.push(format!("[{other}] {}", item)),
                }
            }
        } else if let Some(sc) = result.get("structuredContent") {
            texts.push(sc.to_string());
        }
        let body = if texts.is_empty() { "(MCP 工具无文本返回)".to_string() } else { texts.join("\n") };
        Ok(ToolOutput { content: format!("[MCP:{}] {body}", self.name), is_error })
    }

    pub async fn shutdown(&mut self) {
        let _ = self.child.kill().await;
    }
}

/// 把 MCP 工具包装成内核统一工具
struct McpTool {
    server: String,
    raw_name: String,
    spec: ToolSpec,
    client: Arc<Mutex<McpClient>>,
}

#[async_trait]
impl Tool for McpTool {
    fn spec(&self) -> ToolSpec {
        self.spec.clone()
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        // 第三方 MCP 服务的语义不可预知，一律按最高风险走人工审批
        RiskLevel::High
    }

    fn reason(&self, args: &Value) -> String {
        format!("MCP 服务 `{}` 请求调用 `{}`，参数：{}", self.server, self.raw_name, truncate(&args.to_string(), 160))
    }

    async fn call(&self, _ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let mut client = self.client.lock().await;
        client.call_tool(&self.raw_name, args).await
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[derive(Default, Clone)]
pub struct McpManager {
    clients: HashMap<String, Arc<Mutex<McpClient>>>,
    pub errors: Vec<String>,
}

impl McpManager {
    pub async fn start(configs: &[McpServerConfig]) -> Self {
        let mut manager = McpManager::default();
        for cfg in configs.iter().filter(|c| c.enabled) {
            match McpClient::connect(cfg).await {
                Ok(client) => {
                    tracing::info!("MCP 服务 `{}` 已连接", cfg.name);
                    manager.clients.insert(cfg.name.clone(), Arc::new(Mutex::new(client)));
                }
                Err(e) => {
                    let msg = format!("{}: {e}", cfg.name);
                    tracing::warn!("MCP 连接失败 {msg}");
                    manager.errors.push(msg);
                }
            }
        }
        manager
    }

    pub async fn add(&mut self, cfg: &McpServerConfig) -> Result<()> {
        let client = McpClient::connect(cfg).await?;
        self.clients.insert(cfg.name.clone(), Arc::new(Mutex::new(client)));
        Ok(())
    }

    /// 把所有 MCP 工具注册进内核工具表，返回注册数量
    pub async fn register_into(&self, registry: &mut ToolRegistry) -> usize {
        let mut count = 0usize;
        for (server, client) in &self.clients {
            let tools = {
                let mut c = client.lock().await;
                match c.list_tools().await {
                    Ok(t) => t,
                    Err(e) => {
                        tracing::warn!("[{server}] tools/list 失败: {e}");
                        continue;
                    }
                }
            };
            for (raw_name, mut spec) in tools {
                let full_name = format!("mcp__{}__{}", sanitize(server), sanitize(&raw_name));
                if full_name.len() > 64 {
                    tracing::warn!("工具名过长已跳过: {full_name}");
                    continue;
                }
                spec.name = full_name;
                spec.source = ToolSource::Mcp;
                registry.register(Arc::new(McpTool {
                    server: server.clone(),
                    raw_name,
                    spec,
                    client: client.clone(),
                }));
                count += 1;
            }
        }
        count
    }

    pub fn is_empty(&self) -> bool {
        self.clients.is_empty()
    }

    pub async fn remove(&mut self, name: &str) {
        if let Some(client) = self.clients.remove(name) {
            client.lock().await.shutdown().await;
        }
    }

    pub fn summaries(&self) -> Vec<Value> {
        let mut out: Vec<Value> = self
            .clients
            .keys()
            .map(|name| json!({ "name": name, "connected": true }))
            .collect();
        out.sort_by(|a, b| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")));
        out
    }

    pub async fn shutdown(&self) {
        for (_, client) in &self.clients {
            client.lock().await.shutdown().await;
        }
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '_' || c == '-' { c } else { '_' })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_tool_names() {
        assert_eq!(sanitize("filesystem.read"), "filesystem_read");
        assert_eq!(sanitize("a b/c"), "a_b_c");
    }

    #[tokio::test]
    async fn missing_command_surfaces_as_error() {
        let cfg = McpServerConfig {
            name: "broken".into(),
            command: "definitely-not-a-real-binary-xyz".into(),
            args: vec![],
            env: Default::default(),
            enabled: true,
            auto_start: true,
        };
        assert!(McpClient::connect(&cfg).await.is_err());
    }
}
