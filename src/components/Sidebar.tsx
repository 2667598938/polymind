import { useMemo } from "react";
import { useStore } from "../stores/store";

function timeAgo(ts: number): string {
  const diff = Math.max(0, Date.now() - ts);
  const m = Math.floor(diff / 60000);
  if (m < 1) return "刚刚";
  if (m < 60) return `${m} 分钟前`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} 小时前`;
  const d = Math.floor(h / 24);
  if (d < 7) return `${d} 天前`;
  return new Date(ts).toLocaleDateString();
}

export default function Sidebar() {
  const sessions = useStore((s) => s.sessions);
  const activeId = useStore((s) => s.activeId);
  const workspace = useStore((s) => s.workspace);
  const usage = useStore((s) => s.usage);
  const selectSession = useStore((s) => s.selectSession);
  const newSession = useStore((s) => s.newSession);
  const removeSession = useStore((s) => s.removeSession);
  const renameSession = useStore((s) => s.renameSession);
  const pickWorkspace = useStore((s) => s.pickWorkspace);
  const setSettingsOpen = useStore((s) => s.setSettingsOpen);

  const totalCost = usage?.cost_usd ?? 0;

  const workspaceLabel = useMemo(() => {
    if (!workspace) return "未选择工作区";
    const parts = workspace.replace(/\\/g, "/").split("/");
    return parts[parts.length - 1] || workspace;
  }, [workspace]);

  return (
    <aside className="sidebar">
      <header className="brand">
        <div className="brand-mark" aria-hidden>
          <svg viewBox="0 0 32 32" width="22" height="22">
            <path d="M6 24 L16 6 L26 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round" />
            <circle cx="16" cy="19" r="3.2" fill="currentColor" />
          </svg>
        </div>
        <div className="brand-text">
          <span className="brand-name">PolyMind</span>
          <span className="brand-sub">多模智能体工作台</span>
        </div>
      </header>

      <button className="workspace-chip" onClick={() => void pickWorkspace()} title={workspace}>
        <span className="ws-dot" />
        <span className="ws-name">{workspaceLabel}</span>
        <span className="ws-hint">切换</span>
      </button>

      <button className="new-session" onClick={() => void newSession()}>
        <span className="plus">+</span> 新建会话
      </button>

      <div className="session-scroll">
        <div className="section-label">会话</div>
        {sessions.length === 0 && <div className="empty-sessions">还没有会话，点上面新建一个。</div>}
        {sessions.map((s) => (
          <div
            key={s.id}
            className={`session-item ${s.id === activeId ? "active" : ""}`}
            onClick={() => void selectSession(s.id)}
            onDoubleClick={() => {
              const title = window.prompt("重命名会话", s.title);
              if (title && title.trim()) void renameSession(s.id, title.trim());
            }}
          >
            <div className="session-main">
              <span className="session-title">{s.title || "未命名会话"}</span>
              <span className="session-meta">
                {timeAgo(s.updated_at)} · {s.message_count} 条
                {s.total_cost_usd > 0 ? ` · $${s.total_cost_usd.toFixed(4)}` : ""}
              </span>
            </div>
            <button
              className="session-del"
              title="删除会话"
              onClick={(e) => {
                e.stopPropagation();
                if (window.confirm(`删除「${s.title}」？消息与用量记录会一并移除。`)) void removeSession(s.id);
              }}
            >
              ×
            </button>
          </div>
        ))}
      </div>

      <footer className="sidebar-foot">
        <div className="usage-mini">
          <span className="usage-label">累计消耗</span>
          <span className="usage-value">${totalCost.toFixed(4)}</span>
        </div>
        <button className="ghost-btn" onClick={() => setSettingsOpen(true)}>
          设置
        </button>
      </footer>
    </aside>
  );
}
