import { create } from "zustand";
import { ipc } from "../lib/ipc";
import type {
  AgentEvent,
  ApprovalMode,
  ApprovalRequest,
  Bootstrap,
  Message,
  ProviderConfigView,
  Session,
  ToolCard,
  ToolSpec,
  UsageSummary,
} from "../lib/types";

interface TurnInfo {
  modelRef?: string;
  provider?: string;
  steps: number;
  costUsd: number;
  inputTokens: number;
  outputTokens: number;
}

interface State {
  ready: boolean;
  error: string | null;
  toast: string | null;
  config: ProviderConfigView | null;
  sessions: Session[];
  activeId: string | null;
  messages: Message[];
  tools: ToolSpec[];
  mcp: { name: string; connected: boolean }[];
  usage: UsageSummary | null;
  workspace: string;
  modelRef: string;
  approvalMode: ApprovalMode;
  streaming: boolean;
  streamText: string;
  streamReasoning: string;
  liveTools: ToolCard[];
  approval: ApprovalRequest | null;
  turn: TurnInfo;
  draft: string;
  settingsOpen: boolean;

  bootstrap: () => Promise<void>;
  refreshSessions: () => Promise<void>;
  selectSession: (id: string) => Promise<void>;
  newSession: () => Promise<void>;
  removeSession: (id: string) => Promise<void>;
  renameSession: (id: string, title: string) => Promise<void>;
  send: (text: string) => Promise<void>;
  stop: () => Promise<void>;
  applyEvent: (ev: AgentEvent) => Promise<void>;
  setApproval: (req: ApprovalRequest | null) => void;
  resolveApproval: (decision: "allow" | "session" | "deny") => Promise<void>;
  setDraft: (text: string) => void;
  setModelRef: (ref: string) => void;
  setSettingsOpen: (open: boolean) => void;
  setWorkspace: (path: string) => Promise<void>;
  pickWorkspace: () => Promise<void>;
  saveProvider: (payload: Record<string, unknown>) => Promise<void>;
  deleteProvider: (id: string) => Promise<void>;
  testProvider: (id: string) => Promise<{ ok: boolean; latency_ms: number; models: { id: string }[] }>;
  saveSettings: (patch: Record<string, unknown>) => Promise<void>;
  addMcp: (name: string, command: string, args: string[], env: Record<string, string>) => Promise<void>;
  removeMcp: (name: string) => Promise<void>;
  exportActive: () => Promise<void>;
  clearToast: () => void;
}

const emptyTurn: TurnInfo = { steps: 0, costUsd: 0, inputTokens: 0, outputTokens: 0 };

function errText(e: unknown): string {
  if (typeof e === "string") return e;
  if (e && typeof e === "object" && "message" in e) return String((e as { message: unknown }).message);
  return String(e);
}

export const useStore = create<State>((set, get) => ({
  ready: false,
  error: null,
  toast: null,
  config: null,
  sessions: [],
  activeId: null,
  messages: [],
  tools: [],
  mcp: [],
  usage: null,
  workspace: "",
  modelRef: "",
  approvalMode: "ask",
  streaming: false,
  streamText: "",
  streamReasoning: "",
  liveTools: [],
  approval: null,
  turn: emptyTurn,
  draft: "",
  settingsOpen: false,

  async bootstrap() {
    try {
      const data: Bootstrap = await ipc.bootstrap();
      const first = data.sessions[0];
      set({
        ready: true,
        config: data.config,
        sessions: data.sessions,
        tools: data.tools,
        mcp: data.mcp,
        usage: data.usage,
        workspace: data.workspace,
        modelRef: data.default_model || first?.model_ref || "",
        approvalMode: data.config.approval_mode,
        activeId: first?.id ?? null,
        error: null,
      });
      if (first) {
        const messages = await ipc.loadMessages(first.id);
        set({ messages });
      }
    } catch (e) {
      set({ error: errText(e), ready: false });
    }
  },

  async refreshSessions() {
    try {
      const [sessions, usage] = await Promise.all([ipc.listSessions(), ipc.usageSummary()]);
      set({ sessions, usage });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async selectSession(id) {
    if (get().streaming) await get().stop();
    set({ activeId: id, messages: [], streamText: "", streamReasoning: "", liveTools: [], error: null, turn: emptyTurn });
    try {
      const messages = await ipc.loadMessages(id);
      const session = get().sessions.find((s) => s.id === id);
      set({ messages, modelRef: session?.model_ref || get().modelRef });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async newSession() {
    try {
      const session = await ipc.createSession();
      set({
        sessions: [session, ...get().sessions],
        activeId: session.id,
        messages: [],
        streamText: "",
        liveTools: [],
        approval: null,
        error: null,
        turn: emptyTurn,
      });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async removeSession(id) {
    try {
      await ipc.deleteSession(id);
      const rest = get().sessions.filter((s) => s.id !== id);
      set({ sessions: rest });
      if (get().activeId === id) {
        const next = rest[0];
        if (next) {
          await get().selectSession(next.id);
        } else {
          set({ activeId: null, messages: [] });
        }
      }
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async renameSession(id, title) {
    try {
      await ipc.renameSession(id, title);
      set({ sessions: get().sessions.map((s) => (s.id === id ? { ...s, title } : s)) });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async send(text) {
    const trimmed = text.trim();
    if (!trimmed || get().streaming) return;
    if (!get().activeId) {
      await get().newSession();
    }
    const sessionId = get().activeId;
    if (!sessionId) return;

    const optimistic: Message = { role: "user", parts: [{ type: "text", text: trimmed }] };
    set({
      messages: [...get().messages, optimistic],
      draft: "",
      streaming: true,
      streamText: "",
      streamReasoning: "",
      liveTools: [],
      error: null,
      turn: emptyTurn,
    });

    try {
      const result = await ipc.sendMessage(sessionId, trimmed, get().modelRef || undefined);
      set({
        turn: {
          modelRef: result.model_ref,
          steps: result.steps,
          costUsd: result.cost_usd,
          inputTokens: result.usage.input_tokens,
          outputTokens: result.usage.output_tokens,
        },
      });
      const messages = await ipc.loadMessages(sessionId);
      set({ messages, streaming: false, streamText: "", streamReasoning: "", liveTools: [] });
      await get().refreshSessions();
    } catch (e) {
      set({ error: errText(e), streaming: false });
      await get().refreshSessions();
    }
  },

  async stop() {
    try {
      await ipc.cancelTurn();
    } catch {
      /* 已结束则忽略 */
    }
  },

  async applyEvent(ev) {
    const activeId = get().activeId;
    switch (ev.kind) {
      case "turn_started":
        if (ev.session_id !== activeId) return;
        set({ streaming: true, turn: { ...get().turn, modelRef: ev.model_ref, provider: ev.provider } });
        break;
      case "text_delta":
        set({ streamText: get().streamText + ev.text });
        break;
      case "reasoning_delta":
        set({ streamReasoning: get().streamReasoning + ev.text });
        break;
      case "tool_call_started":
        set({
          liveTools: [
            ...get().liveTools,
            { callId: ev.call_id, tool: ev.tool, args: ev.args, risk: ev.risk, status: "running" },
          ],
        });
        break;
      case "tool_call_finished":
        set({
          liveTools: get().liveTools.map((t) =>
            t.callId === ev.call_id ? { ...t, status: ev.ok ? "ok" : "error", output: ev.output, durationMs: ev.duration_ms } : t,
          ),
        });
        break;
      case "usage":
        set({
          turn: {
            ...get().turn,
            costUsd: get().turn.costUsd + ev.cost_usd,
            inputTokens: get().turn.inputTokens + ev.usage.input_tokens,
            outputTokens: get().turn.outputTokens + ev.usage.output_tokens,
          },
        });
        break;
      case "notice":
        set({ toast: ev.message });
        break;
      case "turn_finished":
        set({ streaming: false });
        break;
      case "error":
        set({ error: `${ev.code}: ${ev.message}`, streaming: false });
        break;
      default:
        break;
    }
  },

  setApproval(req) {
    set({ approval: req });
  },

  async resolveApproval(decision) {
    const req = get().approval;
    if (!req) return;
    set({ approval: null });
    try {
      await ipc.resolveApproval(req.request_id, decision);
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  setDraft(text) {
    set({ draft: text });
  },

  setModelRef(ref) {
    set({ modelRef: ref });
  },

  setSettingsOpen(open) {
    set({ settingsOpen: open });
  },

  async setWorkspace(path) {
    try {
      const res = await ipc.setWorkspace(path);
      set({
        workspace: res.workspace,
        config: get().config ? { ...get().config!, allowed_roots: res.allowed_roots } : null,
      });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async pickWorkspace() {
    try {
      const { open } = await import("@tauri-apps/plugin-dialog");
      const picked = await open({ directory: true, multiple: false, title: "选择 Agent 的工作区目录" });
      if (typeof picked === "string" && picked) await get().setWorkspace(picked);
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async saveProvider(payload) {
    try {
      await ipc.upsertProvider(payload);
      const data = await ipc.bootstrap();
      set({ config: data.config, tools: data.tools, modelRef: data.default_model || get().modelRef, toast: "供应商已保存" });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async deleteProvider(id) {
    try {
      await ipc.deleteProvider(id);
      const data = await ipc.bootstrap();
      set({ config: data.config, tools: data.tools, toast: "供应商已删除" });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async testProvider(id) {
    return ipc.testProvider(id);
  },

  async saveSettings(patch) {
    try {
      await ipc.saveSettings(patch);
      const data = await ipc.bootstrap();
      set({
        config: data.config,
        approvalMode: data.config.approval_mode,
        tools: data.tools,
        modelRef: data.default_model || get().modelRef,
        toast: "设置已保存",
      });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async addMcp(name, command, args, env) {
    try {
      await ipc.addMcpServer(name, command, args, env);
      const data = await ipc.bootstrap();
      set({ config: data.config, mcp: data.mcp, tools: data.tools, toast: `MCP 服务 ${name} 已连接` });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async removeMcp(name) {
    try {
      await ipc.removeMcpServer(name);
      const data = await ipc.bootstrap();
      set({ config: data.config, mcp: data.mcp, tools: data.tools, toast: "MCP 服务已移除" });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  async exportActive() {
    const id = get().activeId;
    if (!id) return;
    try {
      const res = await ipc.exportSession(id);
      set({ toast: `已导出：${res.path}` });
    } catch (e) {
      set({ error: errText(e) });
    }
  },

  clearToast() {
    set({ toast: null });
  },
}));
