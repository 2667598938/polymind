//! 命令执行工具。
//!
//! 关键约束：
//! - 不经 shell，`program` 与 `args` 分开传，天然没有管道/重定向/变量展开；
//! - 程序名必须在白名单内（默认 git/npm/node/cargo/python 等）；
//! - 默认 60s 超时，超时就 kill，不留僵尸进程；
//! - 输出统一截断到 6k 字符，防止一次 `cargo build` 把上下文打爆。

use async_trait::async_trait;
use serde_json::{json, Value};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;

use super::{Tool, ToolCtx, ToolOutput};
use crate::agent::permission::RiskLevel;
use crate::model::{ToolSource, ToolSpec};
use crate::{Error, Result};

pub struct RunCommand;

#[async_trait]
impl Tool for RunCommand {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "run_command".into(),
            description: "在工作区内执行一条命令（参数数组，不经 shell）。仅允许白名单程序。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "program": { "type": "string", "description": "例如 cargo / git / npm / python" },
                    "args": { "type": "array", "items": { "type": "string" } },
                    "cwd": { "type": "string", "description": "工作区内的相对路径" },
                    "timeout_ms": { "type": "integer", "minimum": 1000, "maximum": 300000 }
                },
                "required": ["program"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::High
    }

    fn reason(&self, args: &Value) -> String {
        let program = args.get("program").and_then(|p| p.as_str()).unwrap_or("?");
        let argv: Vec<String> = args
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        format!("执行命令：{program} {}", argv.join(" "))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let program = args.get("program").and_then(|p| p.as_str()).ok_or_else(|| Error::Tool("缺少 program".into()))?;
        ctx.gate.check_command(program)?;
        let argv: Vec<String> = args
            .get("args")
            .and_then(|a| a.as_array())
            .map(|arr| arr.iter().filter_map(|v| v.as_str().map(|s| s.to_string())).collect())
            .unwrap_or_default();
        let timeout_ms = args.get("timeout_ms").and_then(|t| t.as_u64()).unwrap_or(60_000).clamp(1_000, 300_000);
        let cwd = match args.get("cwd").and_then(|c| c.as_str()) {
            Some(c) if !c.trim().is_empty() => ctx.gate.jail(c)?,
            _ => ctx.workspace.clone(),
        };
        if !cwd.is_dir() {
            return Ok(ToolOutput::err(format!("cwd 不是目录: {}", cwd.display())));
        }

        let mut cmd = build_command(program, &argv);
        cmd.current_dir(&cwd)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);

        let mut child = cmd.spawn().map_err(|e| Error::Tool(format!("启动 `{program}` 失败: {e}")))?;
        let stdout = child.stdout.take().ok_or_else(|| Error::Tool("无法捕获 stdout".into()))?;
        let stderr = child.stderr.take().ok_or_else(|| Error::Tool("无法捕获 stderr".into()))?;
        let out_task = tokio::spawn(read_capped(stdout, 256 * 1024));
        let err_task = tokio::spawn(read_capped(stderr, 128 * 1024));

        let started = std::time::Instant::now();
        let status = match tokio::time::timeout(Duration::from_millis(timeout_ms), child.wait()).await {
            Ok(s) => s?,
            Err(_) => {
                let _ = child.kill().await;
                let _ = out_task.await;
                let _ = err_task.await;
                return Ok(ToolOutput::err(format!(
                    "命令超时（{timeout_ms}ms）已终止。请缩小任务范围或提高超时上限。"
                )));
            }
        };
        let stdout = out_task.await.unwrap_or_else(|_| Ok(String::new()))?;
        let stderr = err_task.await.unwrap_or_else(|_| Ok(String::new()))?;

        let code = status.code().map(|c| c.to_string()).unwrap_or_else(|| "signal".into());
        let mut body = String::new();
        if !stdout.trim().is_empty() {
            body.push_str("--- stdout ---\n");
            body.push_str(&stdout);
            if !body.ends_with('\n') {
                body.push('\n');
            }
        }
        if !stderr.trim().is_empty() {
            body.push_str("--- stderr ---\n");
            body.push_str(&stderr);
        }
        if body.trim().is_empty() {
            body.push_str("(无输出)\n");
        }
        let summary = format!(
            "{} $ {} {}\nexit={} 耗时={}ms\n{}",
            cwd.display(),
            program,
            argv.join(" "),
            code,
            started.elapsed().as_millis(),
            cap_chars(&body, 6_000)
        );
        Ok(ToolOutput { content: summary, is_error: !status.success() })
    }
}

fn build_command(program: &str, args: &[String]) -> Command {
    #[cfg(windows)]
    {
        let lower = program.to_lowercase();
        if lower.ends_with(".cmd") || lower.ends_with(".bat") {
            // Windows 的 CreateProcess 不能直接拉起 .cmd，需要走 cmd /C，
            // 因此这里对每个参数做最小转义，避免参数里的空格被拆开。
            let mut c = Command::new("cmd.exe");
            c.arg("/C").arg(quote_for_cmd(&format!("{program} {}", args.iter().map(|a| quote_for_cmd(a)).collect::<Vec<_>>().join(" "))));
            return c;
        }
    }
    let mut c = Command::new(program);
    c.args(args);
    c
}

#[cfg(windows)]
fn quote_for_cmd(arg: &str) -> String {
    if !arg.contains(' ') && !arg.contains('"') && !arg.contains('&') {
        return arg.to_string();
    }
    format!("\"{}\"", arg.replace('"', "\"\""))
}

async fn read_capped<R>(mut reader: R, cap: usize) -> Result<String>
where
    R: AsyncRead + Unpin + Send,
{
    let mut buf = Vec::new();
    let mut chunk = vec![0u8; 8192];
    let mut truncated = false;
    loop {
        let n = reader.read(&mut chunk).await?;
        if n == 0 {
            break;
        }
        if buf.len() < cap {
            let take = (cap - buf.len()).min(n);
            buf.extend_from_slice(&chunk[..take]);
            if take < n {
                truncated = true;
            }
        } else {
            truncated = true;
        }
    }
    let mut s = String::from_utf8_lossy(&buf).into_owned();
    if truncated {
        s.push_str("\n…（输出过长，已截断）");
    }
    Ok(s)
}

fn cap_chars(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().take(max / 2).collect();
    let tail: String = s.chars().rev().take(max / 2).collect::<Vec<_>>().into_iter().rev().collect();
    format!("{head}\n…（中间省略）…\n{tail}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cap_keeps_head_and_tail() {
        let s = "A".repeat(100) + &"B".repeat(100);
        let out = cap_chars(&s, 40);
        assert!(out.starts_with("AAAA"));
        assert!(out.ends_with("BBBB"));
        assert!(out.contains("省略"));
    }
}
