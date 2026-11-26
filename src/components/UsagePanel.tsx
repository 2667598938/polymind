import { useStore } from "../stores/store";

export default function UsagePanel() {
  const usage = useStore((s) => s.usage);
  const refreshSessions = useStore((s) => s.refreshSessions);

  if (!usage) return <div className="empty-block">暂无用量记录。</div>;

  return (
    <div className="usage-panel">
      <div className="usage-cards">
        <div className="usage-card">
          <span className="usage-card-label">累计请求</span>
          <span className="usage-card-value">{usage.requests}</span>
        </div>
        <div className="usage-card">
          <span className="usage-card-label">输入 tokens</span>
          <span className="usage-card-value">{usage.input_tokens.toLocaleString()}</span>
        </div>
        <div className="usage-card">
          <span className="usage-card-label">输出 tokens</span>
          <span className="usage-card-value">{usage.output_tokens.toLocaleString()}</span>
        </div>
        <div className="usage-card accent">
          <span className="usage-card-label">累计成本</span>
          <span className="usage-card-value">${usage.cost_usd.toFixed(4)}</span>
        </div>
      </div>

      <div className="usage-table">
        <div className="usage-tr head">
          <span>模型</span>
          <span>请求</span>
          <span>输入</span>
          <span>输出</span>
          <span>成本</span>
        </div>
        {usage.by_model.length === 0 && <div className="empty-block">还没有调用记录。</div>}
        {usage.by_model.map((m) => (
          <div key={m.model_ref} className="usage-tr">
            <span className="mono">{m.model_ref}</span>
            <span>{m.requests}</span>
            <span>{m.input_tokens.toLocaleString()}</span>
            <span>{m.output_tokens.toLocaleString()}</span>
            <span>${m.cost_usd.toFixed(5)}</span>
          </div>
        ))}
      </div>

      <div className="form-actions">
        <button className="ghost-btn" onClick={() => void refreshSessions()}>
          刷新
        </button>
      </div>
      <p className="muted small">
        成本按供应商设置的单价估算（美元/百万 token），只统计流式返回的真实 usage；缓存命中按 cache_read 单价结算。
      </p>
    </div>
  );
}
