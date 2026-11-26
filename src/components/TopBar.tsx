import { useMemo } from "react";
import { useStore } from "../stores/store";

export default function TopBar() {
  const config = useStore((s) => s.config);
  const modelRef = useStore((s) => s.modelRef);
  const setModelRef = useStore((s) => s.setModelRef);
  const approvalMode = useStore((s) => s.approvalMode);
  const streaming = useStore((s) => s.streaming);
  const stop = useStore((s) => s.stop);
  const turn = useStore((s) => s.turn);
  const activeId = useStore((s) => s.activeId);
  const sessions = useStore((s) => s.sessions);
  const exportActive = useStore((s) => s.exportActive);

  const options = useMemo(() => {
    const list: { value: string; label: string }[] = [];
    for (const p of config?.providers ?? []) {
      for (const m of p.models.length ? p.models : [p.default_model]) {
        list.push({ value: `${p.id}/${m}`, label: `${p.label || p.id} · ${m}` });
      }
    }
    if (modelRef && !list.some((o) => o.value === modelRef)) list.unshift({ value: modelRef, label: modelRef });
    return list;
  }, [config, modelRef]);

  const active = sessions.find((s) => s.id === activeId);
  const modeLabel: Record<string, string> = { ask: "逐次审批", auto: "自动放行", read_only: "只读模式" };

  return (
    <header className="topbar">
      <div className="topbar-title">
        <span className="crumb">工作区 /</span>
        <strong>{active?.title ?? "新会话"}</strong>
      </div>

      <div className="topbar-right">
        <div className={`mode-pill mode-${approvalMode}`} title="审批策略可在设置中调整">
          {modeLabel[approvalMode] ?? approvalMode}
        </div>

        {(turn.inputTokens > 0 || turn.costUsd > 0) && (
          <div className="cost-pill" title="本轮累计用量">
            <span>{turn.inputTokens}↑</span>
            <span>{turn.outputTokens}↓</span>
            <span className="cost">${turn.costUsd.toFixed(5)}</span>
          </div>
        )}

        <select className="model-select" value={modelRef} onChange={(e) => setModelRef(e.target.value)} title="切换模型">
          {options.length === 0 && <option value="">未配置模型</option>}
          {options.map((o) => (
            <option key={o.value} value={o.value}>
              {o.label}
            </option>
          ))}
        </select>

        {streaming ? (
          <button className="stop-btn" onClick={() => void stop()}>
            停止
          </button>
        ) : (
          <button className="ghost-btn" onClick={() => void exportActive()} title="导出为 Markdown 并打开">
            导出
          </button>
        )}
      </div>
    </header>
  );
}
