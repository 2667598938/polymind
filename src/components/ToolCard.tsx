import type { RiskLevel, ToolCard } from "../lib/types";

const RISK_LABEL: Record<RiskLevel, string> = { low: "只读", medium: "写入", high: "命令" };

export default function ToolCardView({ card }: { card: ToolCard }) {
  const statusText = card.status === "running" ? "执行中" : card.status === "ok" ? "完成" : "失败";
  return (
    <div className={`tool-card ${card.status}`}>
      <div className="tool-head">
        <span className="tool-name">{card.tool}</span>
        <span className={`risk-badge risk-${card.risk}`}>{RISK_LABEL[card.risk]}</span>
        <span className="tool-status">
          {statusText}
          {card.durationMs != null ? ` · ${card.durationMs}ms` : ""}
        </span>
      </div>
      <div className="tool-args">{previewArgs(card.args)}</div>
      {card.output && (
        <details className="tool-output" open={card.status !== "running"}>
          <summary>执行输出</summary>
          <pre>{card.output.length > 4000 ? `${card.output.slice(0, 4000)}\n…（已截断）` : card.output}</pre>
        </details>
      )}
    </div>
  );
}

function previewArgs(args: unknown): string {
  try {
    const raw = typeof args === "string" ? args : JSON.stringify(args);
    if (!raw) return "";
    return raw.length > 220 ? `${raw.slice(0, 220)}…` : raw;
  } catch {
    return String(args);
  }
}
