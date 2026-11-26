import { useState } from "react";
import { useStore } from "../stores/store";

export default function McpPanel() {
  const config = useStore((s) => s.config);
  const mcp = useStore((s) => s.mcp);
  const addMcp = useStore((s) => s.addMcp);
  const removeMcp = useStore((s) => s.removeMcp);

  const [name, setName] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");
  const [env, setEnv] = useState("");

  const servers = config?.mcp_servers ?? [];

  const submit = async () => {
    if (!name.trim() || !command.trim()) return;
    const argList = args
      .split(/\s+/)
      .map((a) => a.trim())
      .filter(Boolean);
    const envMap: Record<string, string> = {};
    env
      .split("\n")
      .forEach((line) => {
        const idx = line.indexOf("=");
        if (idx > 0) envMap[line.slice(0, idx).trim()] = line.slice(idx + 1).trim();
      });
    await addMcp(name.trim(), command.trim(), argList, envMap);
    setName("");
    setCommand("");
    setArgs("");
    setEnv("");
  };

  return (
    <div className="mcp-panel">
      <p className="muted">
        MCP 服务以子进程方式运行，工具名会加上 <code>mcp__服务__工具</code> 前缀。第三方工具一律按高风险审批，不会自动放行。
      </p>

      <div className="mcp-list">
        {servers.length === 0 && <div className="empty-block">还没有 MCP 服务。可从 filesystem、git、sqlite 等官方 server 起步。</div>}
        {servers.map((s) => {
          const live = mcp.find((m) => m.name === s.name);
          return (
            <div key={s.name} className="mcp-row">
              <div>
                <strong>{s.name}</strong>
                <span className={`conn-dot ${live?.connected ? "on" : "off"}`} />
                <div className="mcp-cmd">
                  {s.command} {s.args.join(" ")}
                </div>
              </div>
              <button className="danger-btn small" onClick={() => void removeMcp(s.name)}>
                移除
              </button>
            </div>
          );
        })}
      </div>

      <div className="mcp-add">
        <h3>接入新的 MCP 服务</h3>
        <div className="form-grid">
          <label>
            <span>服务名</span>
            <input value={name} onChange={(e) => setName(e.target.value)} placeholder="filesystem" />
          </label>
          <label>
            <span>启动命令</span>
            <input value={command} onChange={(e) => setCommand(e.target.value)} placeholder="npx" />
          </label>
          <label className="full">
            <span>参数（空格分隔）</span>
            <input value={args} onChange={(e) => setArgs(e.target.value)} placeholder="-y @modelcontextprotocol/server-filesystem C:\workspace" />
          </label>
          <label className="full">
            <span>环境变量（每行 KEY=VALUE）</span>
            <textarea value={env} onChange={(e) => setEnv(e.target.value)} rows={3} placeholder="GITHUB_TOKEN=xxx" />
          </label>
        </div>
        <button className="primary-btn" onClick={() => void submit()}>
          启动并注册
        </button>
      </div>
    </div>
  );
}
