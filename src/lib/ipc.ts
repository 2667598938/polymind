import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
import type { AgentEvent, ApprovalRequest, Bootstrap, Message, SendResult, Session, UsageSummary } from "./types";

export const EVENT_STREAM = "polymind://event";
export const EVENT_APPROVAL = "polymind://approval";

export const isTauri = typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

function requireTauri() {
  if (!isTauri) {
    throw new Error("当前不是 Tauri 运行环境。请用 `npm run tauri dev` 启动桌面端。");
  }
}

export const ipc = {
  async bootstrap(): Promise<Bootstrap> {
    requireTauri();
    return invoke<Bootstrap>("bootstrap");
  },
  async setWorkspace(path: string): Promise<{ workspace: string; allowed_roots: string[] }> {
    return invoke("set_workspace", { path });
  },
  async createSession(title?: string): Promise<Session> {
    return invoke<Session>("create_session", { title });
  },
  async listSessions(): Promise<Session[]> {
    return invoke<Session[]>("list_sessions");
  },
  async deleteSession(sessionId: string): Promise<void> {
    return invoke("delete_session", { sessionId });
  },
  async renameSession(sessionId: string, title: string): Promise<void> {
    return invoke("rename_session", { sessionId, title });
  },
  async loadMessages(sessionId: string): Promise<Message[]> {
    return invoke<Message[]>("load_messages", { sessionId });
  },
  async sendMessage(sessionId: string, text: string, modelRef?: string): Promise<SendResult> {
    return invoke<SendResult>("send_message", { sessionId, text, modelRef: modelRef ?? null });
  },
  async cancelTurn(): Promise<void> {
    return invoke("cancel_turn");
  },
  async resolveApproval(requestId: string, decision: "allow" | "session" | "deny"): Promise<void> {
    return invoke("resolve_approval", { requestId, decision });
  },
  async upsertProvider(payload: Record<string, unknown>): Promise<unknown> {
    return invoke("upsert_provider", { input: payload });
  },
  async deleteProvider(providerId: string): Promise<unknown> {
    return invoke("delete_provider", { providerId });
  },
  async testProvider(providerId: string): Promise<{ ok: boolean; latency_ms: number; models: { id: string }[] }> {
    return invoke("test_provider", { providerId });
  },
  async saveSettings(patch: Record<string, unknown>): Promise<unknown> {
    return invoke("save_settings", { patch });
  },
  async usageSummary(sessionId?: string): Promise<UsageSummary> {
    return invoke<UsageSummary>("usage_summary", { sessionId: sessionId ?? null });
  },
  async addMcpServer(name: string, command: string, args: string[], env: Record<string, string>): Promise<unknown> {
    return invoke("add_mcp_server", { name, command, args, env });
  },
  async removeMcpServer(name: string): Promise<void> {
    return invoke("remove_mcp_server", { name });
  },
  async exportSession(sessionId: string): Promise<{ path: string }> {
    return invoke("export_session", { sessionId });
  },
  async onEvent(handler: (ev: AgentEvent) => void): Promise<() => void> {
    requireTauri();
    return listen<AgentEvent>(EVENT_STREAM, (e) => handler(e.payload));
  },
  async onApproval(handler: (req: ApprovalRequest) => void): Promise<() => void> {
    requireTauri();
    return listen<ApprovalRequest>(EVENT_APPROVAL, (e) => handler(e.payload));
  },
};
