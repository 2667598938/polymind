// 与 Rust 侧 serde 结构一一对应的类型定义。
// 改动这里时必须同步 core/src/model.rs 与 agent/runtime.rs。

export type Role = "system" | "user" | "assistant" | "tool";

export interface Usage {
  input_tokens: number;
  output_tokens: number;
  cache_read_tokens: number;
  cache_write_tokens: number;
  reasoning_tokens: number;
}

export type ContentPart =
  | { type: "text"; text: string }
  | { type: "image"; mime: string; data_b64: string }
  | { type: "tool_use"; id: string; name: string; input: unknown }
  | { type: "tool_result"; id: string; name: string; content: string; is_error: boolean };

export interface MessageMeta {
  model_ref?: string;
  provider?: string;
  usage?: Usage;
  cost_usd?: number;
  latency_ms?: number;
  compacted_from?: number;
}

export interface Message {
  role: Role;
  parts: ContentPart[];
  meta?: MessageMeta;
}

export interface ToolSpec {
  name: string;
  description: string;
  parameters: Record<string, unknown>;
  source: "builtin" | "mcp";
}

export interface Session {
  id: string;
  title: string;
  model_ref: string;
  workspace: string;
  created_at: number;
  updated_at: number;
  message_count: number;
  total_cost_usd: number;
}

export interface ModelUsage {
  model_ref: string;
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
}

export interface UsageSummary {
  requests: number;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
  by_model: ModelUsage[];
}

export type ApprovalMode = "ask" | "auto" | "read_only";
export type RiskLevel = "low" | "medium" | "high";

export interface ProviderView {
  id: string;
  kind: string;
  label: string;
  base_url: string;
  default_model: string;
  models: string[];
  enabled: boolean;
  key_masked: string;
  has_key: boolean;
}

export interface McpServerConfig {
  name: string;
  command: string;
  args: string[];
  env: Record<string, string>;
  enabled: boolean;
  auto_start: boolean;
}

export interface ProviderConfigView {
  version: number;
  default_model: string;
  approval_mode: ApprovalMode;
  allowed_roots: string[];
  command_allowlist: string[];
  max_steps: number;
  context_budget_tokens: number;
  fallback_models: string[];
  system_prompt_extra: string;
  theme: string;
  providers: ProviderView[];
  mcp_servers: McpServerConfig[];
}

export interface Bootstrap {
  version: string;
  config: ProviderConfigView;
  sessions: Session[];
  workspace: string;
  default_model: string;
  tools: ToolSpec[];
  mcp: { name: string; connected: boolean }[];
  usage: UsageSummary;
}

export interface ApprovalRequest {
  request_id: string;
  session_id: string;
  tool: string;
  args: unknown;
  risk: RiskLevel;
  reason: string;
}

export type AgentEvent =
  | { kind: "turn_started"; session_id: string; model_ref: string; provider: string; model: string }
  | { kind: "step_started"; step: number }
  | { kind: "text_delta"; text: string }
  | { kind: "reasoning_delta"; text: string }
  | { kind: "tool_call_started"; call_id: string; tool: string; args: unknown; risk: RiskLevel }
  | { kind: "tool_call_finished"; call_id: string; tool: string; ok: boolean; output: string; duration_ms: number }
  | { kind: "usage"; usage: Usage; cost_usd: number; model_ref: string }
  | { kind: "notice"; level: string; message: string }
  | { kind: "turn_finished"; finish: string; steps: number; cost_usd: number }
  | { kind: "error"; code: string; message: string; retryable: boolean };

export interface SendResult {
  finish: string;
  steps: number;
  cost_usd: number;
  usage: Usage;
  messages: Message[] | null;
  model_ref: string;
}

export interface ToolCard {
  callId: string;
  tool: string;
  args: unknown;
  risk: RiskLevel;
  status: "running" | "ok" | "error";
  output?: string;
  durationMs?: number;
}
