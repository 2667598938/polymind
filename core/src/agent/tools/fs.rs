//! 文件系统工具：读、写、列目录、正则检索。
//!
//! 所有路径都先过 `PermissionGate::jail`，工具本身不允许直接摸到工作区外。

use async_trait::async_trait;
use regex::RegexBuilder;
use serde_json::{json, Value};
use std::io::{BufRead, BufReader};
use std::path::Path;
use walkdir::WalkDir;

use super::{Tool, ToolCtx, ToolOutput};
use crate::agent::permission::RiskLevel;
use crate::model::{ToolSource, ToolSpec};
use crate::{Error, Result};

pub struct ReadFile;

#[async_trait]
impl Tool for ReadFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fs_read".into(),
            description: "读取工作区内文本文件，返回带行号的内容。大文件请用 offset/limit 分段读取。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "相对工作区或工作区内的绝对路径" },
                    "offset": { "type": "integer", "description": "起始行号，从 1 开始", "minimum": 1 },
                    "limit": { "type": "integer", "description": "最多读取行数，默认 400，上限 2000" }
                },
                "required": ["path"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn reason(&self, args: &Value) -> String {
        format!("读取文件 {}", args.get("path").and_then(|p| p.as_str()).unwrap_or("?"))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|p| p.as_str()).ok_or_else(|| Error::Tool("缺少 path".into()))?;
        let real = ctx.gate.jail(path)?;
        if real.is_dir() {
            return Ok(ToolOutput::err(format!("{} 是目录，请改用 fs_list", real.display())));
        }
        let offset = args.get("offset").and_then(|o| o.as_u64()).unwrap_or(1).max(1) as usize;
        let limit = args.get("limit").and_then(|l| l.as_u64()).unwrap_or(400).clamp(1, 2000) as usize;

        let file = std::fs::File::open(&real)?;
        let meta = file.metadata()?;
        if meta.len() > 8 * 1024 * 1024 {
            return Ok(ToolOutput::err("文件超过 8MB，请用 fs_grep 定向检索或分段读取".to_string()));
        }
        let reader = BufReader::new(file);
        let mut out = String::new();
        let mut total = 0usize;
        let mut emitted = 0usize;
        for (idx, line) in reader.lines().enumerate() {
            total += 1;
            let line = line?;
            if idx + 1 < offset {
                continue;
            }
            if emitted >= limit {
                continue;
            }
            out.push_str(&format!("{:>5}\t{}\n", idx + 1, line));
            emitted += 1;
        }
        if out.is_empty() {
            out.push_str("<空文件或起始行超出文件末尾>\n");
        }
        Ok(ToolOutput::ok(format!(
            "文件 {} 共 {} 行，本次输出 {}-{} 行：\n{}",
            real.display(),
            total,
            offset,
            offset + emitted.saturating_sub(1),
            out
        )))
    }
}

pub struct WriteFile;

#[async_trait]
impl Tool for WriteFile {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fs_write".into(),
            description: "写入或追加文本文件；会自动创建父目录。覆盖写入前请确认内容完整。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string" },
                    "content": { "type": "string" },
                    "mode": { "type": "string", "enum": ["overwrite", "append"], "description": "默认 overwrite" }
                },
                "required": ["path", "content"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::Medium
    }

    fn reason(&self, args: &Value) -> String {
        let p = args.get("path").and_then(|p| p.as_str()).unwrap_or("?");
        let mode = args.get("mode").and_then(|m| m.as_str()).unwrap_or("overwrite");
        let bytes = args.get("content").and_then(|c| c.as_str()).map(|c| c.len()).unwrap_or(0);
        format!("写入 {p}（{mode}，{} 字节）", bytes)
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|p| p.as_str()).ok_or_else(|| Error::Tool("缺少 path".into()))?;
        let content = args.get("content").and_then(|c| c.as_str()).ok_or_else(|| Error::Tool("缺少 content".into()))?;
        let mode = args.get("mode").and_then(|m| m.as_str()).unwrap_or("overwrite");
        let real = ctx.gate.jail(path)?;
        if let Some(parent) = real.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let existed = real.exists();
        if mode == "append" {
            use std::io::Write;
            let mut f = std::fs::OpenOptions::new().create(true).append(true).open(&real)?;
            f.write_all(content.as_bytes())?;
        } else {
            std::fs::write(&real, content)?;
        }
        let lines = content.lines().count();
        Ok(ToolOutput::ok(format!(
            "{} {}（{} 行 / {} 字节）",
            if existed { "已更新" } else { "已创建" },
            real.display(),
            lines,
            content.len()
        )))
    }
}

pub struct ListDir;

const IGNORED_DIRS: &[&str] = &[".git", "node_modules", "target", "dist", ".next", "__pycache__", ".venv", "venv", ".idea"];

#[async_trait]
impl Tool for ListDir {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fs_list".into(),
            description: "列出目录结构（默认深度 2），自动跳过 node_modules/target/.git 等噪音目录。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "path": { "type": "string", "description": "默认工作区根目录" },
                    "depth": { "type": "integer", "minimum": 1, "maximum": 4 }
                },
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn reason(&self, args: &Value) -> String {
        format!("列出目录 {}", args.get("path").and_then(|p| p.as_str()).unwrap_or("."))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let path = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let real = ctx.gate.jail(path)?;
        if !real.is_dir() {
            return Ok(ToolOutput::err(format!("{} 不是目录", real.display())));
        }
        let depth = args.get("depth").and_then(|d| d.as_u64()).unwrap_or(2).clamp(1, 4) as usize;
        let mut out = String::new();
        let mut count = 0usize;
        for entry in WalkDir::new(&real).max_depth(depth).follow_links(false).into_iter().filter_entry(|e| {
            e.file_name()
                .to_str()
                .map(|n| !IGNORED_DIRS.contains(&n))
                .unwrap_or(true)
        }) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if entry.path() == real {
                continue;
            }
            if count >= 500 {
                out.push_str("…（超过 500 条已截断）\n");
                break;
            }
            let rel = entry.path().strip_prefix(&real).unwrap_or(entry.path());
            let is_dir = entry.file_type().is_dir();
            let size = entry.metadata().map(|m| m.len()).unwrap_or(0);
            out.push_str(&format!(
                "{}{}{}\n",
                rel.display(),
                if is_dir { "/" } else { "" },
                if is_dir { String::new() } else { format!("  ({} B)", size) }
            ));
            count += 1;
        }
        if out.is_empty() {
            out.push_str("(空目录)\n");
        }
        Ok(ToolOutput::ok(format!("目录 {}：\n{}", real.display(), out)))
    }
}

pub struct Grep;

#[async_trait]
impl Tool for Grep {
    fn spec(&self) -> ToolSpec {
        ToolSpec {
            name: "fs_grep".into(),
            description: "在工作区内用正则检索文本，返回 路径:行号:内容。适合先定位再精读。".into(),
            parameters: json!({
                "type": "object",
                "properties": {
                    "pattern": { "type": "string", "description": "Rust/POSIX 风格正则" },
                    "path": { "type": "string", "description": "检索起点，默认工作区" },
                    "max_results": { "type": "integer", "minimum": 1, "maximum": 200 }
                },
                "required": ["pattern"],
                "additionalProperties": false
            }),
            source: ToolSource::Builtin,
        }
    }

    fn risk(&self, _args: &Value) -> RiskLevel {
        RiskLevel::Low
    }

    fn reason(&self, args: &Value) -> String {
        format!("在工作区检索 `{}`", args.get("pattern").and_then(|p| p.as_str()).unwrap_or("?"))
    }

    async fn call(&self, ctx: &ToolCtx, args: Value) -> Result<ToolOutput> {
        let pattern = args.get("pattern").and_then(|p| p.as_str()).ok_or_else(|| Error::Tool("缺少 pattern".into()))?;
        let start = args.get("path").and_then(|p| p.as_str()).unwrap_or(".");
        let max = args.get("max_results").and_then(|m| m.as_u64()).unwrap_or(80).clamp(1, 200) as usize;
        let real = ctx.gate.jail(start)?;
        let re = RegexBuilder::new(pattern).size_limit(4 * 1024 * 1024).build().map_err(|e| Error::Tool(format!("正则无效: {e}")))?;

        let mut hits = Vec::new();
        'outer: for entry in WalkDir::new(&real).follow_links(false).into_iter().filter_entry(|e| {
            e.file_name().to_str().map(|n| !IGNORED_DIRS.contains(&n)).unwrap_or(true)
        }) {
            let entry = match entry {
                Ok(e) => e,
                Err(_) => continue,
            };
            if !entry.file_type().is_file() {
                continue;
            }
            let path = entry.path();
            if is_binary_ext(path) {
                continue;
            }
            let meta = match entry.metadata() {
                Ok(m) => m,
                Err(_) => continue,
            };
            if meta.len() > 2 * 1024 * 1024 {
                continue;
            }
            let file = match std::fs::File::open(path) {
                Ok(f) => f,
                Err(_) => continue,
            };
            for (idx, line) in BufReader::new(file).lines().enumerate() {
                let line = match line {
                    Ok(l) => l,
                    Err(_) => break,
                };
                if re.is_match(&line) {
                    let rel = path.strip_prefix(&real).unwrap_or(path);
                    hits.push(format!("{}:{}: {}", rel.display(), idx + 1, truncate(&line, 240)));
                    if hits.len() >= max {
                        break 'outer;
                    }
                }
            }
        }
        if hits.is_empty() {
            return Ok(ToolOutput::ok(format!("未命中 `{pattern}`（范围 {}）", real.display())));
        }
        Ok(ToolOutput::ok(format!("命中 {} 处：\n{}", hits.len(), hits.join("\n"))))
    }
}

fn truncate(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        return s.to_string();
    }
    let cut: String = s.chars().take(max_chars).collect();
    format!("{cut}…")
}

fn is_binary_ext(path: &Path) -> bool {
    const BIN: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "ico", "pdf", "zip", "gz", "7z", "exe", "dll", "so", "dylib", "woff", "woff2", "ttf", "mp4", "mp3", "wav", "sqlite", "db"];
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| BIN.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}
