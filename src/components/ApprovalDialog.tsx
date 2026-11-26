import { useEffect } from "react";
import { useStore } from "../stores/store";
import type { ApprovalRequest, RiskLevel } from "../lib/types";

const RISK_TEXT: Record<RiskLevel, string> = {
  low: "低风险 · 只读访问",
  medium: "中风险 · 修改本地文件",
  high: "高风险 · 执行命令 / 第三方工具",
};

export default function ApprovalDialog({ request }: { request: ApprovalRequest }) {
  const resolve = useStore((s) => s.resolveApproval);
  const stop = useStore((s) => s.stop);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") void resolve("deny");
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [resolve]);

  return (
    <div className="modal-backdrop">
      <div className="approval-card">
        <div className={`approval-risk risk-${request.risk}`}>{RISK_TEXT[request.risk]}</div>
        <h2>Agent 请求执行工具</h2>
        <p className="approval-reason">{request.reason}</p>
        <div className="approval-meta">
          <span>
            工具 <code>{request.tool}</code>
          </span>
          <span>会话 {request.session_id.slice(0, 8)}</span>
        </div>
        <pre className="approval-args">{pretty(request.args)}</pre>
        <div className="approval-actions">
          <button className="danger-btn" onClick={() => void resolve("deny")}>
            拒绝
          </button>
          <button className="ghost-btn" onClick={() => void resolve("session")}>
            本会话内同类放行
          </button>
          <button className="primary-btn" onClick={() => void resolve("allow")}>
            允许一次
          </button>
        </div>
        <div className="approval-foot">
          <span>审批 10 分钟未响应将自动拒绝</span>
          <button
            className="link-btn"
            onClick={() => {
              void stop();
              void resolve("deny");
            }}
          >
            终止整个任务
          </button>
        </div>
      </div>
    </div>
  );
}

function pretty(v: unknown): string {
  try {
    const s = JSON.stringify(v, null, 2);
    return s.length > 2400 ? `${s.slice(0, 2400)}\n…` : s;
  } catch {
    return String(v);
  }
}
