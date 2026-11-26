//! PolyMind 命令行入口（GUI 的兜底形态）。
//!
//! 用途：
//! 1. 没有图形环境时验证供应商连通与工具循环；
//! 2. 作为"零成本"入口，方便排查 Tauri 层问题；
//! 3. 演示同一份内核可以被不同前端复用。
//!
//! 用法：
//!   polymind-cli --home .polymind providers
//!   polymind-cli --home .polymind set-key openai sk-xxxx
//!   polymind-cli --home .polymind run --prompt "列一下当前目录结构"
//!   polymind-cli --home .polymind run --prompt "..." --auto

use async_trait::async_trait;
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use polymind_core::agent::permission::{ApprovalRequest, Approver, Decision, PermissionGate};
use polymind_core::agent::runtime::{default_system_prompt, AgentConfig, AgentEvent, AgentRuntime};
use polymind_core::agent::tools::ToolRegistry;
use polymind_core::config::AppConfig;
use polymind_core::mcp::McpManager;
use polymind_core::model::Message;
use polymind_core::provider::Registry;
use polymind_core::secrets::SecretBox;
use polymind_core::store::Store;
use polymind_core::{Error, Result};

struct Args {
    home: PathBuf,
    command: String,
    prompt: Option<String>,
    model: Option<String>,
    provider_id: Option<String>,
    key: Option<String>,
    auto: bool,
    workspace: PathBuf,
}

fn parse_args() -> Args {
    let mut home = std::env::var("POLYMIND_HOME").unwrap_or_else(|_| ".polymind".into());
    let mut command = String::new();
    let mut prompt = None;
    let mut model = None;
    let mut provider_id = None;
    let mut key = None;
    let mut auto = false;
    let mut workspace = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));

    let argv: Vec<String> = std::env::args().skip(1).collect();
    let mut i = 0;
    while i < argv.len() {
        match argv[i].as_str() {
            "--home" => {
                if let Some(v) = argv.get(i + 1) {
                    home = v.clone();
                    i += 1;
                }
            }
            "--prompt" | "-p" => {
                if let Some(v) = argv.get(i + 1) {
                    prompt = Some(v.clone());
                    i += 1;
                }
            }
            "--model" | "-m" => {
                if let Some(v) = argv.get(i + 1) {
                    model = Some(v.clone());
                    i += 1;
                }
            }
            "--provider" => {
                if let Some(v) = argv.get(i + 1) {
                    provider_id = Some(v.clone());
                    i += 1;
                }
            }
            "--workspace" | "-C" => {
                if let Some(v) = argv.get(i + 1) {
                    workspace = PathBuf::from(v);
                    i += 1;
                }
            }
            "--auto" => auto = true,
            other if !other.starts_with('-') && command.is_empty() => command = other.to_string(),
            other => {
                // set-key 的第二个位置参数是 key
                if command == "set-key" && key.is_none() {
                    key = Some(other.to_string());
                }
            }
        }
        i += 1;
    }
    if command.is_empty() {
        command = "run".into();
    }
    Args { home: PathBuf::from(home), command, prompt, model, provider_id, key, auto, workspace }
}

struct CliApprover {
    auto: bool,
}

#[async_trait]
impl Approver for CliApprover {
    async fn approve(&self, req: ApprovalRequest) -> Decision {
        if self.auto {
            return Decision::AllowOnce;
        }
        println!("\n╭─ 需要审批 ─────────────────────────────");
        println!("│ 工具: {}", req.tool);
        println!("│ 原因: {}", req.reason);
        println!("│ 参数: {}", serde_json::to_string(&req.args).unwrap_or_default());
        println!("╰────────────────────────────────────────");
        print!("允许执行？[y]是 [n]否 [s]本会话内同类放行: ");
        let _ = std::io::stdout().flush();
        let mut line = String::new();
        if std::io::stdin().read_line(&mut line).is_err() {
            return Decision::Deny;
        }
        match line.trim().to_lowercase().as_str() {
            "y" | "yes" => Decision::AllowOnce,
            "s" => Decision::AllowSession,
            _ => Decision::Deny,
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = parse_args();
    std::fs::create_dir_all(&args.home)?;
    let cfg_path = args.home.join("config.json");
    let db_path = args.home.join("polymind.db");
    let mut cfg = AppConfig::load_or_init(&cfg_path)?;
    let secrets = SecretBox::open(&args.home)?;

    // 解密所有 key 到内存
    for p in &mut cfg.providers {
        p.api_key = secrets.open_sealed(&p.api_key_cipher).unwrap_or_default();
    }

    match args.command.as_str() {
        "providers" => {
            if cfg.providers.is_empty() {
                println!("尚未配置供应商。示例：\n  polymind-cli set-key openai sk-xxx\n  （然后在 config.json 里补 default_model 与 models）");
            }
            for p in &cfg.providers {
                println!(
                    "- {:<12} kind={:<10} model={:<28} key={}",
                    p.id,
                    format!("{:?}", p.kind),
                    p.default_model,
                    polymind_core::secrets::mask_key(&p.api_key)
                );
            }
            Ok(())
        }
        "set-key" => {
            let (Some(pid), Some(key)) = (args.provider_id.clone(), args.key.clone()) else {
                return Err(Error::Config("用法: set-key --provider <id> <api-key>".into()));
            };
            let cipher = secrets.seal(&key)?;
            match cfg.provider_mut(&pid) {
                Some(p) => {
                    p.api_key_cipher = cipher;
                    p.api_key = key.clone();
                }
                None => {
                    println!("供应商 `{pid}` 不存在，请先在 {} 中新增条目", cfg_path.display());
                    return Ok(());
                }
            }
            cfg.save(&cfg_path)?;
            // 顺手修复权限：配置文件里只有密文
            println!("已保存 `{pid}` 的密钥：{}", polymind_core::secrets::mask_key(&key));
            Ok(())
        }
        "run" | _ => run_turn(args, cfg, cfg_path, db_path, secrets).await,
    }
}

async fn run_turn(mut args: Args, cfg: AppConfig, _cfg_path: PathBuf, db_path: PathBuf, _secrets: SecretBox) -> Result<()> {
    let registry = Registry::from_configs(&cfg.providers)?;
    let model_ref = args.model.clone().or_else(|| {
        if cfg.default_model.trim().is_empty() { None } else { Some(cfg.default_model.clone()) }
    });
    let model_ref = match model_ref {
        Some(m) => m,
        None => {
            let (p, m) = registry.default_target()?;
            format!("{}/{}", p.info().id, m)
        }
    };

    args.workspace = args.workspace.canonicalize().unwrap_or(args.workspace.clone());
    if cfg.allowed_roots.is_empty() {
        println!("提示：配置里 allowed_roots 为空，工具将无法访问任何路径。");
    }

    let gate = Arc::new(PermissionGate::from_config(&cfg, &args.workspace, Arc::new(CliApprover { auto: args.auto })));
    let mut tools = ToolRegistry::builtin();
    let mcp = McpManager::start(&cfg.mcp_servers).await;
    let mcp_count = mcp.register_into(&mut tools).await;
    if mcp_count > 0 {
        println!("已接入 {mcp_count} 个 MCP 工具");
    }

    let runtime = AgentRuntime {
        registry: Arc::new(registry),
        tools: Arc::new(tools),
        gate,
        config: AgentConfig {
            system_prompt: default_system_prompt(&args.workspace, &cfg.system_prompt_extra),
            max_steps: cfg.max_steps,
            context_budget_tokens: cfg.context_budget_tokens,
            temperature: None,
            max_tokens: Some(8192),
            fallback_models: cfg.fallback_models.clone(),
            tool_timeout: Duration::from_secs(180),
        },
        workspace: args.workspace.clone(),
    };

    let store = Store::open(&db_path)?;
    let session = store.create_session("CLI 会话", &model_ref, &args.workspace.to_string_lossy())?;
    println!("会话 {} | 模型 {} | 工作区 {}", session.id, model_ref, args.workspace.display());

    let prompt = match args.prompt.clone() {
        Some(p) => p,
        None => {
            print!("> ");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            std::io::stdin().read_line(&mut line)?;
            line.trim().to_string()
        }
    };
    if prompt.is_empty() {
        return Ok(());
    }

    let user_msg = Message::user(prompt.clone());
    store.append_message(&session.id, &user_msg)?;

    let (tx, mut rx) = tokio::sync::mpsc::channel::<AgentEvent>(256);
    let printer = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            match ev {
                AgentEvent::TextDelta { text } => {
                    print!("{text}");
                    let _ = std::io::stdout().flush();
                }
                AgentEvent::ReasoningDelta { .. } => {}
                AgentEvent::ToolCallStarted { tool, .. } => {
                    eprintln!("\n[工具] {tool}");
                }
                AgentEvent::ToolCallFinished { tool, ok, output, duration_ms, .. } => {
                    eprintln!("[{}] {tool} {}ms\n{}", if ok { "成功" } else { "失败" }, duration_ms, truncate(&output, 600));
                }
                AgentEvent::Usage { usage, cost_usd, model_ref } => {
                    eprintln!(
                        "\n[用量] {model_ref} in={} out={} cache={} cost=${:.5}",
                        usage.input_tokens, usage.output_tokens, usage.cache_read_tokens, cost_usd
                    );
                }
                AgentEvent::Notice { level, message } => eprintln!("[{level}] {message}"),
                AgentEvent::TurnFinished { finish, steps, cost_usd } => {
                    println!("\n── 结束: {finish}，{steps} 步，累计 ${cost_usd:.5}");
                }
                AgentEvent::Error { message, .. } => eprintln!("\n[错误] {message}"),
                _ => {}
            }
        }
    });

    let cancel = tokio_util::sync::CancellationToken::new();
    let outcome = runtime.run_turn(&session.id, &model_ref, vec![user_msg], &tx, &cancel).await;
    drop(tx);
    let _ = printer.await;
    let outcome = outcome?;

    for m in &outcome.messages {
        store.append_message(&session.id, m)?;
    }
    store.record_usage(&session.id, &model_ref, &outcome.usage, outcome.cost_usd)?;
    mcp.shutdown().await;
    println!("\n会话已保存：{}", db_path.display());
    Ok(())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}
