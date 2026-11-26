import { useEffect, useState } from "react";
import { useStore } from "../stores/store";
import ProviderForm from "./ProviderForm";
import McpPanel from "./McpPanel";
import UsagePanel from "./UsagePanel";
import type { ProviderView } from "../lib/types";

const TABS = ["供应商", "权限与上下文", "MCP 插件", "用量账本", "关于"] as const;
type Tab = (typeof TABS)[number];

export default function SettingsPanel() {
  const config = useStore((s) => s.config);
  const versionReady = useStore((s) => s.ready);
  const setSettingsOpen = useStore((s) => s.setSettingsOpen);
  const deleteProvider = useStore((s) => s.deleteProvider);
  const saveSettings = useStore((s) => s.saveSettings);
  const pickWorkspace = useStore((s) => s.pickWorkspace);

  const [tab, setTab] = useState<Tab>("供应商");
  const [editing, setEditing] = useState<ProviderView | null>(null);
  const [adding, setAdding] = useState(false);

  const [mode, setMode] = useState(config?.approval_mode ?? "ask");
  const [maxSteps, setMaxSteps] = useState(String(config?.max_steps ?? 16));
  const [budget, setBudget] = useState(String(config?.context_budget_tokens ?? 96000));
  const [allowlist, setAllowlist] = useState((config?.command_allowlist ?? []).join("\n"));
  const [fallback, setFallback] = useState((config?.fallback_models ?? []).join(", "));
  const [systemExtra, setSystemExtra] = useState(config?.system_prompt_extra ?? "");
  const [defaultModel, setDefaultModel] = useState(config?.default_model ?? "");
  const [roots, setRoots] = useState(config?.allowed_roots ?? []);

  useEffect(() => {
    if (!config) return;
    setMode(config.approval_mode);
    setMaxSteps(String(config.max_steps));
    setBudget(String(config.context_budget_tokens));
    setAllowlist(config.command_allowlist.join("\n"));
    setFallback(config.fallback_models.join(", "));
    setSystemExtra(config.system_prompt_extra);
    setDefaultModel(config.default_model);
    setRoots(config.allowed_roots);
  }, [config]);

  return (
    <div className="modal-backdrop settings-backdrop" onClick={() => setSettingsOpen(false)}>
      <div className="settings-panel" onClick={(e) => e.stopPropagation()}>
        <header className="settings-head">
          <div>
            <h2>设置</h2>
            <p className="muted small">配置只保存在本机，API Key 以 ChaCha20-Poly1305 加密后落盘</p>
          </div>
          <button className="icon-btn" onClick={() => setSettingsOpen(false)} aria-label="关闭">
            ×
          </button>
        </header>

        <nav className="settings-tabs">
          {TABS.map((t) => (
            <button key={t} className={t === tab ? "tab active" : "tab"} onClick={() => setTab(t)}>
              {t}
            </button>
          ))}
        </nav>

        <div className="settings-body">
          {tab === "供应商" && (
            <div className="providers-tab">
              {(config?.providers ?? []).length === 0 && (
                <div className="empty-block">
                  还没有供应商。添加第一个：填 Base URL 与 Key，选默认模型，就能开始对话。
                </div>
              )}
              <div className="provider-list">
                {(config?.providers ?? []).map((p) => (
                  <div key={p.id} className="provider-row">
                    <div className="provider-info">
                      <div className="provider-title">
                        <strong>{p.label || p.id}</strong>
                        <span className="kind-tag">{p.kind}</span>
                        {p.has_key ? <span className="key-ok">Key {p.key_masked}</span> : <span className="key-missing">未配置 Key</span>}
                      </div>
                      <div className="provider-sub mono">
                        {p.base_url || "(默认地址)"} · {p.default_model}
                      </div>
                    </div>
                    <div className="provider-actions">
                      <button
                        className="ghost-btn small"
                        onClick={() => {
                          setEditing(p);
                          setAdding(false);
                        }}
                      >
                        编辑
                      </button>
                      <button
                        className="danger-btn small"
                        onClick={() => {
                          if (window.confirm(`删除供应商 ${p.id}？相关 Key 会从配置中移除。`)) void deleteProvider(p.id);
                        }}
                      >
                        删除
                      </button>
                    </div>
                  </div>
                ))}
              </div>

              {(adding || editing) && (
                <ProviderForm
                  editing={editing}
                  onDone={() => {
                    setAdding(false);
                    setEditing(null);
                  }}
                />
              )}
              {!adding && !editing && (
                <button className="primary-btn" onClick={() => setAdding(true)}>
                  + 添加供应商
                </button>
              )}
            </div>
          )}

          {tab === "权限与上下文" && (
            <div className="settings-form">
              <label>
                <span>默认模型（provider/model）</span>
                <input value={defaultModel} onChange={(e) => setDefaultModel(e.target.value)} placeholder="deepseek/deepseek-chat" />
              </label>

              <div className="field-block">
                <span className="field-label">审批策略</span>
                <div className="radio-row">
                  {[
                    { v: "ask", t: "逐次审批", d: "只读自动放行，写入与命令逐次确认" },
                    { v: "auto", t: "自动放行", d: "写入自动执行，仅命令执行需审批" },
                    { v: "read_only", t: "只读模式", d: "禁止一切写入与命令，适合审查陌生项目" },
                  ].map((o) => (
                    <button key={o.v} className={mode === o.v ? "radio-card active" : "radio-card"} onClick={() => setMode(o.v as typeof mode)}>
                      <strong>{o.t}</strong>
                      <small>{o.d}</small>
                    </button>
                  ))}
                </div>
              </div>

              <div className="field-block">
                <span className="field-label">已授权工作区</span>
                <div className="roots-list">
                  {roots.map((r) => (
                    <span key={r} className="root-chip mono" title={r}>
                      {r}
                    </span>
                  ))}
                </div>
                <button className="ghost-btn small" onClick={() => void pickWorkspace()}>
                  添加工作区
                </button>
                <p className="muted small">工具只能访问这些目录内的路径，越界会被内核直接拒绝。</p>
              </div>

              <label>
                <span>命令白名单（每行一个可执行名）</span>
                <textarea rows={8} value={allowlist} onChange={(e) => setAllowlist(e.target.value)} className="mono" />
              </label>

              <div className="two-col">
                <label>
                  <span>单轮最大步数</span>
                  <input value={maxSteps} onChange={(e) => setMaxSteps(e.target.value)} />
                </label>
                <label>
                  <span>上下文预算（tokens）</span>
                  <input value={budget} onChange={(e) => setBudget(e.target.value)} />
                </label>
              </div>

              <label>
                <span>兜底模型链（逗号分隔，主模型失败时按序尝试）</span>
                <input value={fallback} onChange={(e) => setFallback(e.target.value)} placeholder="gemini/gemini-2.5-flash, openai/gpt-4o-mini" />
              </label>

              <label>
                <span>追加系统提示词</span>
                <textarea rows={4} value={systemExtra} onChange={(e) => setSystemExtra(e.target.value)} placeholder="例如：回答尽量短，先给结论再给依据" />
              </label>

              <div className="form-actions">
                <button
                  className="primary-btn"
                  onClick={() =>
                    void saveSettings({
                      approval_mode: mode,
                      max_steps: Number(maxSteps) || 16,
                      context_budget_tokens: Number(budget) || 96000,
                      command_allowlist: allowlist.split("\n").map((s) => s.trim()).filter(Boolean),
                      fallback_models: fallback.split(",").map((s) => s.trim()).filter(Boolean),
                      system_prompt_extra: systemExtra,
                      default_model: defaultModel,
                    })
                  }
                >
                  保存设置
                </button>
              </div>
            </div>
          )}

          {tab === "MCP 插件" && <McpPanel />}
          {tab === "用量账本" && <UsagePanel />}

          {tab === "关于" && (
            <div className="about-tab">
              <h3>PolyMind · 多模智能体工作台</h3>
              <p className="muted">
                自带模型（BYOK）的桌面 Agent：不绑定任何一家厂商，不代管额度，不把你的 Key 传到第三方中转。
              </p>
              <ul className="about-list">
                <li>内核：Rust（供应商适配 / ReAct 循环 / 权限门 / MCP 客户端 / SQLite 账本）</li>
                <li>外壳：Tauri 2 + React 18 + TypeScript</li>
                <li>密钥：本地 ChaCha20-Poly1305，密文入 SQLite / 配置文件</li>
                <li>数据：全部在本机，卸载即清除</li>
              </ul>
              <p className="muted small">版本 {versionReady ? "0.1.0" : "-"} · 开局 MVP</p>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}
