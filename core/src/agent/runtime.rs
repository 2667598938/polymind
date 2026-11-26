//! Agent 内核：ReAct 循环 + 事件流 + 上下文预算 + 模型回退。

use super::permission::{PermissionGate, RiskLevel};
use super::tools::{ToolCtx, ToolOutput, ToolRegistry};
use crate::model::*;
use crate::provider::{EventStream, Registry};
use crate::{Error, Result};
use futures_util::StreamExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

#[derive(Debug, Clone)]
pub struct AgentConfig {
    pub system_prompt: String,
    pub max_steps: usize,
    pub context_budget_tokens: usize,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    pub fallback_models: Vec<String>,
    pub tool_timeout: std::time::Duration,
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            system_prompt: default_system_prompt(std::path::Path::new("."), ""),
            max_steps: 16,
            context_budget_tokens: 96_000,
            temperature: None,
            max_tokens: Some(8192),
            fallback_models: vec![],
            tool_timeout: std::time::Duration::from_secs(180),
        }
    }
}

pub fn default_system_prompt(workspace: &std::path::Path, extra: &str) -> String {
    let mut p = format!(
        "你是 PolyMind 的桌面智能体，运行在用户本机的 Tauri 客户端中，可以通过工具直接操作本地工作区。\n\
工作区根目录：{}\n\n\
行为准则：\n\
1. 涉及文件时先用 fs_list / fs_grep 定位，再用 fs_read 精读；不要凭空猜测文件内容。\n\
2. 写文件前用一句话说明要改什么，写入内容必须完整可运行。\n\
3. run_command 只用于构建、测试、格式检查等必要操作，不执行删除、格式化、批量重命名等破坏性命令。\n\
4. 工具报错时先读错误信息再调整，同一个失败调用不要连续重复超过两次。\n\
5. 回答使用简体中文，代码、命令、路径保持原样。\n\
6. 不要声称执行了并未实际调用的工具；不确定时先查证。\n\
7. 任务完成时给出一句话结论，必要时附下一步建议。\n",
        workspace.display()
    );
    if !extra.trim().is_empty() {
        p.push_str("\n用户附加要求：\n");
        p.push_str(extra.trim());
        p.push('\n');
    }
    p
}

/// 前端消费的事件流（Tauri 通过 `polymind://event` 转发）
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentEvent {
    TurnStarted { session_id: String, model_ref: String, provider: String, model: String },
    StepStarted { step: usize },
    TextDelta { text: String },
    ReasoningDelta { text: String },
    ToolCallStarted { call_id: String, tool: String, args: Value, risk: RiskLevel },
    ToolCallFinished { call_id: String, tool: String, ok: bool, output: String, duration_ms: u64 },
    Usage { usage: Usage, cost_usd: f64, model_ref: String },
    Notice { level: String, message: String },
    TurnFinished { finish: String, steps: usize, cost_usd: f64 },
    Error { code: String, message: String, retryable: bool },
}

#[derive(Debug, Clone)]
pub struct RunOutcome {
    /// 本次新增的消息（assistant / tool），调用方负责落库
    pub messages: Vec<Message>,
    pub steps: usize,
    pub cost_usd: f64,
    pub finish: FinishReason,
    pub usage: Usage,
}

pub struct AgentRuntime {
    pub registry: Arc<Registry>,
    pub tools: Arc<ToolRegistry>,
    pub gate: Arc<PermissionGate>,
    pub config: AgentConfig,
    pub workspace: std::path::PathBuf,
}

impl AgentRuntime {
    pub async fn run_turn(
        &self,
        session_id: &str,
        model_ref: &str,
        history: Vec<Message>,
        emit: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Result<RunOutcome> {
        let (mut history, dropped) = self.compact(history);
        if dropped > 0 {
            let _ = emit
                .send(AgentEvent::Notice {
                    level: "info".into(),
                    message: format!("上下文超出预算，已折叠较早的 {dropped} 条消息（保留最近对话与工具结果）"),
                })
                .await;
        }

        let tool_specs = self.tools.specs();
        let ctx = ToolCtx {
            workspace: self.workspace.clone(),
            gate: self.gate.clone(),
            session_id: session_id.to_string(),
        };

        let mut new_messages: Vec<Message> = Vec::new();
        let mut steps = 0usize;
        let mut total_cost = 0.0f64;
        let mut total_usage = Usage::default();
        let mut current_ref = model_ref.to_string();

        loop {
            if steps >= self.config.max_steps {
                let _ = emit
                    .send(AgentEvent::TurnFinished { finish: "max_steps".into(), steps, cost_usd: total_cost })
                    .await;
                return Ok(RunOutcome {
                    messages: new_messages,
                    steps,
                    cost_usd: total_cost,
                    finish: FinishReason::Other("max_steps".into()),
                    usage: total_usage,
                });
            }
            if cancel.is_cancelled() {
                let _ = emit
                    .send(AgentEvent::TurnFinished { finish: "cancelled".into(), steps, cost_usd: total_cost })
                    .await;
                return Ok(RunOutcome {
                    messages: new_messages,
                    steps,
                    cost_usd: total_cost,
                    finish: FinishReason::Cancelled,
                    usage: total_usage,
                });
            }
            steps += 1;
            let _ = emit.send(AgentEvent::StepStarted { step: steps }).await;

            // 选择模型：主模型 -> 回退链，只在"还没吐 token"时切换
            let (provider, model, mut stream) = match self.open_stream(&current_ref, &history, &tool_specs, emit, cancel).await {
                Ok(v) => v,
                Err(e) => {
                    let _ = emit
                        .send(AgentEvent::Error { code: e.code().to_string(), message: e.to_string(), retryable: e.retryable() })
                        .await;
                    return Err(e);
                }
            };
            let provider_id = provider.info().id.clone();
            let resolved_ref = format!("{}/{}", provider_id, model);
            if steps == 1 {
                let _ = emit
                    .send(AgentEvent::TurnStarted {
                        session_id: session_id.to_string(),
                        model_ref: resolved_ref.clone(),
                        provider: provider_id.clone(),
                        model: model.clone(),
                    })
                    .await;
            }

            let started = Instant::now();
            let mut text = String::new();
            let mut reasoning = String::new();
            let mut pending: BTreeMap<u32, PendingCall> = BTreeMap::new();
            let mut step_usage = Usage::default();
            let mut finish: Option<FinishReason> = None;

            loop {
                tokio::select! {
                    _ = cancel.cancelled() => {
                        finish = Some(FinishReason::Cancelled);
                        break;
                    }
                    item = stream.next() => {
                        match item {
                            None => break,
                            Some(Err(e)) => {
                                let _ = emit.send(AgentEvent::Error {
                                    code: e.code().to_string(),
                                    message: e.to_string(),
                                    retryable: e.retryable(),
                                }).await;
                                return Err(e);
                            }
                            Some(Ok(ev)) => match ev {
                                StreamEvent::Start { .. } => {}
                                StreamEvent::Text { delta } => {
                                    text.push_str(&delta);
                                    let _ = emit.send(AgentEvent::TextDelta { text: delta }).await;
                                }
                                StreamEvent::Reasoning { delta } => {
                                    reasoning.push_str(&delta);
                                    let _ = emit.send(AgentEvent::ReasoningDelta { text: delta }).await;
                                }
                                StreamEvent::ToolCallStart { index, id, name } => {
                                    let entry = pending.entry(index).or_default();
                                    if !name.is_empty() { entry.name = name; }
                                    if !id.is_empty() { entry.id = id; }
                                }
                                StreamEvent::ToolCallArgs { index, delta } => {
                                    pending.entry(index).or_default().args.push_str(&delta);
                                }
                                StreamEvent::Usage { usage } => {
                                    step_usage.merge(&usage);
                                }
                                StreamEvent::Finish { reason } => {
                                    finish = Some(reason);
                                }
                                StreamEvent::Error { message, retryable } => {
                                    let _ = emit.send(AgentEvent::Error {
                                        code: "upstream".into(),
                                        message,
                                        retryable,
                                    }).await;
                                }
                            }
                        }
                    }
                }
            }

            let cost = provider.pricing(&model).cost_usd(&step_usage);
            total_cost += cost;
            total_usage.merge(&step_usage);
            let _ = emit
                .send(AgentEvent::Usage { usage: step_usage.clone(), cost_usd: cost, model_ref: resolved_ref.clone() })
                .await;

            // 组装 assistant 消息
            let mut parts: Vec<ContentPart> = Vec::new();
            if !text.is_empty() {
                parts.push(ContentPart::text(text));
            }
            let mut calls: Vec<(String, String, Value)> = Vec::new();
            for (index, call) in pending {
                if call.name.is_empty() {
                    continue;
                }
                let id = if call.id.is_empty() { format!("call_{steps}_{index}") } else { call.id };
                let input = parse_tool_args(&call.args);
                parts.push(ContentPart::tool_use(id.clone(), call.name.clone(), input.clone()));
                calls.push((id, call.name, input));
            }
            if !reasoning.is_empty() {
                // 思考内容不回传上游（部分厂商会 400），只留在事件流里给用户看
                let _ = emit;
            }
            let mut meta = MessageMeta::default();
            meta.model_ref = Some(resolved_ref.clone());
            meta.provider = Some(provider_id.clone());
            meta.usage = Some(step_usage.clone());
            meta.cost_usd = Some(cost);
            meta.latency_ms = Some(started.elapsed().as_millis() as u64);
            let assistant = Message::new(Role::Assistant, parts).with_meta(meta);
            history.push(assistant.clone());
            new_messages.push(assistant);

            if calls.is_empty() {
                let reason = finish.unwrap_or(FinishReason::Stop);
                let label = reason.as_label();
                let _ = emit
                    .send(AgentEvent::TurnFinished { finish: label, steps, cost_usd: total_cost })
                    .await;
                return Ok(RunOutcome { messages: new_messages, steps, cost_usd: total_cost, finish: reason, usage: total_usage });
            }

            // 串行执行工具：保持因果顺序，也避免并发写同一个文件
            for (call_id, name, input) in calls {
                let tool = match self.tools.get(&name) {
                    Some(t) => t,
                    None => {
                        let msg = format!("工具 `{name}` 不存在，可用：{}", self.tools.names().join(", "));
                        let part = ContentPart::tool_result(call_id.clone(), name.clone(), msg.clone(), true);
                        history.push(Message::new(Role::Tool, vec![part.clone()]));
                        new_messages.push(Message::new(Role::Tool, vec![part]));
                        let _ = emit
                            .send(AgentEvent::ToolCallFinished { call_id, tool: name, ok: false, output: msg, duration_ms: 0 })
                            .await;
                        continue;
                    }
                };
                let risk = tool.risk(&input);
                let reason = tool.reason(&input);
                let _ = emit
                    .send(AgentEvent::ToolCallStarted {
                        call_id: call_id.clone(),
                        tool: name.clone(),
                        args: input.clone(),
                        risk,
                    })
                    .await;

                let t0 = Instant::now();
                let output: ToolOutput = match self.gate.authorize(session_id, &name, &input, risk, &reason).await {
                    Err(e) => ToolOutput::err(format!("{}", e)),
                    Ok(()) => match tokio::time::timeout(self.config.tool_timeout, tool.call(&ctx, input.clone())).await {
                        Ok(Ok(out)) => out,
                        Ok(Err(e)) => ToolOutput::err(e.to_string()),
                        Err(_) => ToolOutput::err(format!("工具执行超时（{}s）", self.config.tool_timeout.as_secs())),
                    },
                };
                let duration_ms = t0.elapsed().as_millis() as u64;
                let _ = emit
                    .send(AgentEvent::ToolCallFinished {
                        call_id: call_id.clone(),
                        tool: name.clone(),
                        ok: !output.is_error,
                        output: output.content.clone(),
                        duration_ms,
                    })
                    .await;
                let part = ContentPart::tool_result(call_id, name, output.content, output.is_error);
                let msg = Message::new(Role::Tool, vec![part]);
                history.push(msg.clone());
                new_messages.push(msg);
            }

            // 若本轮已经是最后一次允许的步数，下一轮循环会在顶部收口
            if let Some(f) = finish {
                if f == FinishReason::Length {
                    let _ = emit
                        .send(AgentEvent::Notice { level: "warn".into(), message: "模型输出达到 max_tokens 上限，可能未完成".into() })
                        .await;
                }
            }
            current_ref = resolved_ref;
        }
    }

    async fn open_stream(
        &self,
        model_ref: &str,
        history: &[Message],
        tool_specs: &[ToolSpec],
        emit: &mpsc::Sender<AgentEvent>,
        cancel: &CancellationToken,
    ) -> Result<(Arc<dyn crate::provider::Provider>, String, EventStream)> {
        let mut candidates = vec![model_ref.to_string()];
        candidates.extend(self.config.fallback_models.iter().cloned());
        let mut last_err: Option<Error> = None;

        for (i, cand) in candidates.iter().enumerate() {
            if cancel.is_cancelled() {
                return Err(Error::Cancelled);
            }
            let (provider, model) = match self.registry.resolve(cand) {
                Ok(v) => v,
                Err(e) => {
                    last_err = Some(e);
                    continue;
                }
            };
            let mut req = ChatRequest::new(model.clone(), history.to_vec());
            req.tools = tool_specs.to_vec();
            req.system = Some(self.config.system_prompt.clone());
            req.temperature = self.config.temperature;
            req.max_tokens = self.config.max_tokens;

            let mut attempt = 0u32;
            loop {
                match provider.stream_chat(req.clone()).await {
                    Ok(s) => return Ok((provider, model, s)),
                    Err(e) => {
                        if e.retryable() && attempt < 1 {
                            attempt += 1;
                            tokio::time::sleep(std::time::Duration::from_millis(800)).await;
                            continue;
                        }
                        if i + 1 < candidates.len() {
                            let _ = emit
                                .send(AgentEvent::Notice {
                                    level: "warn".into(),
                                    message: format!("`{cand}` 调用失败（{}），切换到兜底模型", short(&e.to_string(), 80)),
                                })
                                .await;
                        }
                        last_err = Some(e);
                        break;
                    }
                }
            }
        }
        Err(last_err.unwrap_or_else(|| Error::Config("没有可用模型".into())))
    }

    /// 上下文压缩：保留系统消息 + 最近的对话，直到估算 token 进入预算。
    fn compact(&self, history: Vec<Message>) -> (Vec<Message>, usize) {
        let budget = self.config.context_budget_tokens.max(4_000);
        let total: usize = history.iter().map(|m| m.estimate_tokens()).sum();
        if total <= budget {
            return (history, 0);
        }

        let mut keep_from = history.len();
        let mut used = 0usize;
        for (idx, m) in history.iter().enumerate().rev() {
            used += m.estimate_tokens();
            if used > budget / 2 && history.len() - idx > 6 {
                keep_from = idx + 1;
                break;
            }
            keep_from = idx;
        }
        while keep_from < history.len() && history[keep_from].role == Role::Tool {
            keep_from += 1;
        }
        let dropped = keep_from;
        let mut kept: Vec<Message> = history[keep_from..].to_vec();
        if dropped > 0 {
            let note = Message::system(format!(
                "（上下文预算管理：更早的 {dropped} 条消息已折叠。如需细节，请用工具重新读取相关文件。）"
            ));
            kept.insert(0, note);
        }
        (kept, dropped)
    }
}

#[derive(Default)]
struct PendingCall {
    id: String,
    name: String,
    args: String,
}

fn parse_tool_args(raw: &str) -> Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return serde_json::json!({});
    }
    serde_json::from_str(trimmed).unwrap_or_else(|_| Value::String(trimmed.to_string()))
}

fn short(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    s.chars().take(max).collect::<String>() + "…"
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime(budget: usize) -> AgentRuntime {
        AgentRuntime {
            registry: Arc::new(Registry::new()),
            tools: Arc::new(ToolRegistry::builtin()),
            gate: Arc::new(PermissionGate::new(
                crate::agent::permission::ApprovalMode::Auto,
                std::env::temp_dir(),
                vec![std::env::temp_dir()],
                vec![],
                Arc::new(crate::agent::permission::AllowAll),
            )),
            config: AgentConfig { context_budget_tokens: budget, ..Default::default() },
            workspace: std::env::temp_dir(),
        }
    }

    #[test]
    fn compaction_keeps_recent_and_never_orphans_tool() {
        let rt = runtime(1_000);
        let mut history = vec![Message::system("sys")];
        for i in 0..30 {
            history.push(Message::user(format!("第 {i} 条很长的用户消息，用来把预算撑爆 {}", "x".repeat(400))));
            history.push(Message::assistant("好的，我先看下文件，再给结论。".repeat(8)));
        }
        let (kept, dropped) = rt.compact(history);
        assert!(dropped > 0);
        assert!(kept.len() < 61);
        if let Some(first) = kept.first() {
            assert!(first.role != Role::Tool);
        }
    }

    #[test]
    fn parses_malformed_args_as_string() {
        assert_eq!(parse_tool_args("{\"a\":1}")["a"], 1);
        assert!(parse_tool_args("not json").is_string());
        assert_eq!(parse_tool_args(""), serde_json::json!({}));
    }
}
