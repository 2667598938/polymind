//! Tauri 命令层：只做参数校验、状态编排与事件桥接，所有业务逻辑仍在内核里。
//!
//! 前端只能看到 `Result<Value, String>`，错误文案统一为「错误码: 人话」。

use polymind_core::agent::permission::Decision;
use polymind_core::config::{McpServerConfig, ProviderConfig};
use polymind_core::model::{Message, Pricing};
use polymind_core::provider::ProviderKind;
use polymind_core::secrets::mask_key;
use serde::Deserialize;
use serde_json::{json, Value};
use std::sync::Arc;
use tauri::{AppHandle, Emitter, State};
use tokio::sync::mpsc;

use crate::state::{AppState, EVENT_STREAM};

fn to_err(e: impl std::fmt::Display) -> String {
    e.to_string()
}

#[tauri::command]
pub async fn bootstrap(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let cfg = state.config.read().await.clone();
    let sessions = state.store.list_sessions().map_err(to_err)?;
    let workspace = state.workspace.read().await.clone();
    let usage = state.store.usage_summary(None).map_err(to_err)?;
    let tools = state.toolset().await;
    let specs = tools.specs();
    let mcp = state.mcp.lock().await.summaries();
    Ok(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "config": cfg.redacted(),
        "sessions": sessions,
        "workspace": workspace,
        "default_model": cfg.default_model,
        "tools": specs,
        "mcp": mcp,
        "usage": usage,
    }))
}

#[tauri::command]
pub async fn set_workspace(state: State<'_, Arc<AppState>>, path: String) -> Result<Value, String> {
    let canonical = std::fs::canonicalize(&path).map_err(|e| format!("目录不可访问: {e}"))?;
    if !canonical.is_dir() {
        return Err("选择的路径不是目录".into());
    }
    {
        let mut workspace = state.workspace.write().await;
        *workspace = canonical.clone();
    }
    let mut cfg = state.config.write().await;
    let key = canonical.to_string_lossy().to_string();
    if !cfg.allowed_roots.iter().any(|r| r == &key) {
        cfg.allowed_roots.push(key.clone());
    }
    cfg.save(&state.config_path).map_err(to_err)?;
    Ok(json!({ "workspace": key, "allowed_roots": cfg.allowed_roots }))
}

#[tauri::command]
pub async fn create_session(state: State<'_, Arc<AppState>>, title: Option<String>) -> Result<Value, String> {
    let cfg = state.config.read().await.clone();
    let workspace = state.workspace.read().await.clone();
    let model_ref = if cfg.default_model.trim().is_empty() {
        let reg = state.registry().await;
        match reg.default_target() {
            Ok((p, m)) => format!("{}/{}", p.info().id, m),
            Err(_) => String::new(),
        }
    } else {
        cfg.default_model.clone()
    };
    let title = title.unwrap_or_else(|| "新会话".to_string());
    let session = state
        .store
        .create_session(&title, &model_ref, &workspace.to_string_lossy())
        .map_err(to_err)?;
    Ok(serde_json::to_value(session).map_err(to_err)?)
}

#[tauri::command]
pub async fn list_sessions(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let sessions = state.store.list_sessions().map_err(to_err)?;
    Ok(serde_json::to_value(sessions).map_err(to_err)?)
}

#[tauri::command]
pub async fn delete_session(state: State<'_, Arc<AppState>>, session_id: String) -> Result<(), String> {
    state.store.delete_session(&session_id).map_err(to_err)
}

#[tauri::command]
pub async fn rename_session(state: State<'_, Arc<AppState>>, session_id: String, title: String) -> Result<(), String> {
    state.store.rename_session(&session_id, &title).map_err(to_err)
}

#[tauri::command]
pub async fn load_messages(state: State<'_, Arc<AppState>>, session_id: String) -> Result<Value, String> {
    let messages = state.store.load_messages(&session_id).map_err(to_err)?;
    Ok(serde_json::to_value(messages).map_err(to_err)?)
}

#[tauri::command]
pub async fn send_message(
    app: AppHandle,
    state: State<'_, Arc<AppState>>,
    session_id: String,
    text: String,
    model_ref: Option<String>,
) -> Result<Value, String> {
    let text = text.trim().to_string();
    if text.is_empty() {
        return Err("消息为空".into());
    }
    let cfg = state.config.read().await.clone();
    let session = state
        .store
        .list_sessions()
        .map_err(to_err)?
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let model_ref = model_ref
        .filter(|m| !m.trim().is_empty())
        .or_else(|| {
            if session.model_ref.trim().is_empty() { None } else { Some(session.model_ref.clone()) }
        })
        .or_else(|| {
            if cfg.default_model.trim().is_empty() { None } else { Some(cfg.default_model.clone()) }
        })
        .ok_or_else(|| "尚未选择模型：请先在设置里配置供应商".to_string())?;

    let user_msg = Message::user(text.clone());
    state.store.append_message(&session_id, &user_msg).map_err(to_err)?;
    let mut history = state.store.load_messages(&session_id).map_err(to_err)?;

    let runtime = state.runtime(app.clone()).await;
    let (tx, mut rx) = mpsc::channel(512);
    let app_for_events = app.clone();
    let forward = tokio::spawn(async move {
        while let Some(ev) = rx.recv().await {
            if app_for_events.emit(EVENT_STREAM, &ev).is_err() {
                break;
            }
        }
    });

    let token = tokio_util::sync::CancellationToken::new();
    *state.cancel.lock().await = Some(token.clone());

    let outcome = runtime.run_turn(&session_id, &model_ref, std::mem::take(&mut history), &tx, &token).await;
    drop(tx);
    let _ = forward.await;
    *state.cancel.lock().await = None;

    match outcome {
        Ok(out) => {
            for m in &out.messages {
                if let Err(e) = state.store.append_message(&session_id, m) {
                    tracing::warn!("消息落库失败: {e}");
                }
            }
            let usage = out.usage.clone();
            if let Err(e) = state.store.record_usage(&session_id, &model_ref, &usage, out.cost_usd) {
                tracing::warn!("用量落库失败: {e}");
            }
            Ok(json!({
                "finish": out.finish.as_label(),
                "steps": out.steps,
                "cost_usd": out.cost_usd,
                "usage": usage,
                "messages": serde_json::to_value(&out.messages).unwrap_or(Value::Null),
                "model_ref": model_ref,
            }))
        }
        Err(e) => Err(format!("{}: {e}", e.code())),
    }
}

#[tauri::command]
pub async fn cancel_turn(state: State<'_, Arc<AppState>>) -> Result<(), String> {
    if let Some(token) = state.cancel.lock().await.as_ref() {
        token.cancel();
    }
    Ok(())
}

#[tauri::command]
pub async fn resolve_approval(state: State<'_, Arc<AppState>>, request_id: String, decision: String) -> Result<(), String> {
    let decision = match decision.as_str() {
        "allow" => Decision::AllowOnce,
        "session" => Decision::AllowSession,
        _ => Decision::Deny,
    };
    let tx = state.pending.lock().await.remove(&request_id);
    match tx {
        Some(tx) => tx.send(decision).map_err(|_| "审批请求已失效".to_string()),
        None => Err("审批请求已失效（可能已超时）".into()),
    }
}

#[derive(Debug, Deserialize)]
pub struct ProviderInput {
    pub id: String,
    pub kind: ProviderKind,
    #[serde(default)]
    pub label: String,
    #[serde(default)]
    pub base_url: String,
    #[serde(default)]
    pub api_key: Option<String>,
    pub default_model: String,
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub pricing: Option<Pricing>,
    #[serde(default = "yes")]
    pub enabled: bool,
}

fn yes() -> bool {
    true
}

#[tauri::command]
pub async fn upsert_provider(state: State<'_, Arc<AppState>>, input: ProviderInput) -> Result<Value, String> {
    if input.id.trim().is_empty() {
        return Err("供应商 id 不能为空".into());
    }
    let mut pc = ProviderConfig {
        id: input.id.trim().to_string(),
        kind: input.kind,
        label: input.label,
        base_url: input.base_url,
        api_key: String::new(),
        api_key_cipher: String::new(),
        default_model: input.default_model,
        models: input.models,
        pricing: input.pricing.unwrap_or_default(),
        enabled: input.enabled,
    };

    if let Some(key) = input.api_key.filter(|k| !k.trim().is_empty()) {
        pc.api_key_cipher = state.secrets.seal(key.trim()).map_err(to_err)?;
        pc.api_key = key.trim().to_string();
    } else {
        let cfg = state.config.read().await;
        if let Some(existing) = cfg.provider(&pc.id) {
            pc.api_key_cipher = existing.api_key_cipher.clone();
            pc.api_key = existing.api_key.clone();
        }
    }
    if pc.default_model.trim().is_empty() {
        pc.default_model = pc.models.first().cloned().unwrap_or_default();
    }

    {
        let mut cfg = state.config.write().await;
        let fallback_ref = format!("{}/{}", pc.id, pc.default_model);
        cfg.upsert_provider(pc);
        if cfg.default_model.trim().is_empty() {
            cfg.default_model = fallback_ref;
        }
        cfg.save(&state.config_path).map_err(to_err)?;
    }
    state.rebuild_registry().await.map_err(to_err)?;
    let cfg = state.config.read().await.clone();
    Ok(cfg.redacted())
}

#[tauri::command]
pub async fn delete_provider(state: State<'_, Arc<AppState>>, provider_id: String) -> Result<Value, String> {
    {
        let mut cfg = state.config.write().await;
        cfg.providers.retain(|p| p.id != provider_id);
        if cfg.default_model.starts_with(&format!("{provider_id}/")) {
            cfg.default_model = cfg.providers.first().map(|p| format!("{}/{}", p.id, p.default_model)).unwrap_or_default();
        }
        cfg.save(&state.config_path).map_err(to_err)?;
    }
    state.rebuild_registry().await.map_err(to_err)?;
    let cfg = state.config.read().await.clone();
    Ok(cfg.redacted())
}

#[tauri::command]
pub async fn test_provider(state: State<'_, Arc<AppState>>, provider_id: String) -> Result<Value, String> {
    let reg = state.registry().await;
    let provider = reg.get(&provider_id).map_err(to_err)?;
    let t0 = std::time::Instant::now();
    provider.health().await.map_err(|e| format!("{}: {e}", e.code()))?;
    let models = provider.list_models().await.unwrap_or_default();
    let info = provider.info();
    Ok(json!({
        "ok": true,
        "latency_ms": t0.elapsed().as_millis(),
        "models": models,
        "key_masked": mask_key(&state.config.read().await.provider(&provider_id).map(|p| p.api_key.clone()).unwrap_or_default()),
        "kind_label": info.kind.label(),
    }))
}

#[tauri::command]
pub async fn list_models(state: State<'_, Arc<AppState>>, provider_id: String) -> Result<Value, String> {
    let reg = state.registry().await;
    let provider = reg.get(&provider_id).map_err(to_err)?;
    let models = provider.list_models().await.map_err(to_err)?;
    Ok(serde_json::to_value(models).map_err(to_err)?)
}

#[tauri::command]
pub async fn save_settings(state: State<'_, Arc<AppState>>, patch: Value) -> Result<Value, String> {
    {
        let mut cfg = state.config.write().await;
        if let Some(v) = patch.get("approval_mode") {
            cfg.approval_mode = serde_json::from_value(v.clone()).map_err(|_| "审批模式取值非法".to_string())?;
        }
        if let Some(v) = patch.get("allowed_roots") {
            cfg.allowed_roots = serde_json::from_value(v.clone()).map_err(|_| "allowed_roots 必须是字符串数组".to_string())?;
        }
        if let Some(v) = patch.get("command_allowlist") {
            cfg.command_allowlist = serde_json::from_value(v.clone()).map_err(|_| "command_allowlist 必须是字符串数组".to_string())?;
        }
        if let Some(v) = patch.get("max_steps") {
            cfg.max_steps = serde_json::from_value::<usize>(v.clone()).map_err(to_err)?.clamp(1, 64);
        }
        if let Some(v) = patch.get("context_budget_tokens") {
            cfg.context_budget_tokens = serde_json::from_value::<usize>(v.clone()).map_err(to_err)?.clamp(4_000, 400_000);
        }
        if let Some(v) = patch.get("fallback_models") {
            cfg.fallback_models = serde_json::from_value(v.clone()).map_err(|_| "fallback_models 必须是字符串数组".to_string())?;
        }
        if let Some(v) = patch.get("default_model") {
            cfg.default_model = serde_json::from_value(v.clone()).map_err(to_err)?;
        }
        if let Some(v) = patch.get("system_prompt_extra") {
            cfg.system_prompt_extra = serde_json::from_value(v.clone()).map_err(to_err)?;
        }
        if let Some(v) = patch.get("theme") {
            cfg.theme = serde_json::from_value(v.clone()).map_err(to_err)?;
        }
        cfg.save(&state.config_path).map_err(to_err)?;
    }
    let cfg = state.config.read().await.clone();
    Ok(cfg.redacted())
}

#[tauri::command]
pub async fn usage_summary(state: State<'_, Arc<AppState>>, session_id: Option<String>) -> Result<Value, String> {
    let summary = state.store.usage_summary(session_id.as_deref()).map_err(to_err)?;
    Ok(serde_json::to_value(summary).map_err(to_err)?)
}

#[tauri::command]
pub async fn list_tools(state: State<'_, Arc<AppState>>) -> Result<Value, String> {
    let tools = state.toolset().await;
    Ok(serde_json::to_value(tools.specs()).map_err(to_err)?)
}

#[tauri::command]
pub async fn add_mcp_server(
    state: State<'_, Arc<AppState>>,
    name: String,
    command: String,
    args: Vec<String>,
    env: Option<std::collections::BTreeMap<String, String>>,
) -> Result<Value, String> {
    if name.trim().is_empty() || command.trim().is_empty() {
        return Err("name 与 command 均不能为空".into());
    }
    let cfg = McpServerConfig {
        name: name.trim().to_string(),
        command: command.trim().to_string(),
        args,
        env: env.unwrap_or_default(),
        enabled: true,
        auto_start: true,
    };
    {
        let mut c = state.config.write().await;
        c.upsert_mcp(cfg.clone());
        c.save(&state.config_path).map_err(to_err)?;
    }
    let result = state.mcp.lock().await.add(&cfg).await;
    match result {
        Ok(()) => Ok(json!({ "ok": true, "name": cfg.name })),
        Err(e) => Err(format!("连接失败: {e}")),
    }
}

#[tauri::command]
pub async fn remove_mcp_server(state: State<'_, Arc<AppState>>, name: String) -> Result<(), String> {
    {
        let mut c = state.config.write().await;
        c.mcp_servers.retain(|s| s.name != name);
        c.save(&state.config_path).map_err(to_err)?;
    }
    state.mcp.lock().await.remove(&name).await;
    Ok(())
}

#[tauri::command]
pub async fn export_session(state: State<'_, Arc<AppState>>, session_id: String) -> Result<Value, String> {
    let messages = state.store.load_messages(&session_id).map_err(to_err)?;
    let session = state
        .store
        .list_sessions()
        .map_err(to_err)?
        .into_iter()
        .find(|s| s.id == session_id)
        .ok_or_else(|| "会话不存在".to_string())?;

    let mut md = format!("# {}\n\n- 模型：`{}`\n- 工作区：`{}`\n- 导出时间：{}\n\n", session.title, session.model_ref, session.workspace, chrono::Local::now().format("%Y-%m-%d %H:%M"));
    for m in &messages {
        match m.role {
            polymind_core::model::Role::User => md.push_str(&format!("## 用户\n\n{}\n\n", m.plain_text())),
            polymind_core::model::Role::Assistant => {
                md.push_str("## 助手\n\n");
                for p in &m.parts {
                    match p {
                        polymind_core::model::ContentPart::Text { text } => md.push_str(&format!("{text}\n\n")),
                        polymind_core::model::ContentPart::ToolUse { name, input, .. } => {
                            md.push_str(&format!("> 调用工具 `{name}`：`{}`\n\n", input))
                        }
                        _ => {}
                    }
                }
            }
            polymind_core::model::Role::Tool => {
                for p in &m.parts {
                    if let polymind_core::model::ContentPart::ToolResult { name, content, is_error, .. } = p {
                        md.push_str(&format!(
                            "<details><summary>工具结果 `{name}`{}</summary>\n\n```\n{}\n```\n\n</details>\n\n",
                            if *is_error { "（失败）" } else { "" },
                            content
                        ));
                    }
                }
            }
            _ => {}
        }
    }

    let dir = state.home.join("exports");
    std::fs::create_dir_all(&dir).map_err(to_err)?;
    let safe: String = session.title.chars().map(|c| if c.is_alphanumeric() || c == '-' || c == '_' { c } else { '_' }).collect();
    let path = dir.join(format!("{}-{}.md", safe, &session_id[..8.min(session_id.len())]));
    std::fs::write(&path, md).map_err(to_err)?;
    Ok(json!({ "path": path }))
}
