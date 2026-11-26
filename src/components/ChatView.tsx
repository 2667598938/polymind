import { useEffect, useRef } from "react";
import RichText from "./RichText";
import ToolCardView from "./ToolCard";
import { useStore } from "../stores/store";
import type { ContentPart, Message } from "../lib/types";

const SUGGESTIONS = [
  "先列出当前工作区的目录结构，再用一句话告诉我这个项目是做什么的",
  "把 README 整理成一份 800 字的项目介绍，写进 docs/overview.md",
  "跑一遍测试，把失败原因归类后给出修复顺序",
  "检查核心模块，找出最容易出问题的地方并说明理由",
];

export default function ChatView() {
  const messages = useStore((s) => s.messages);
  const streaming = useStore((s) => s.streaming);
  const streamText = useStore((s) => s.streamText);
  const streamReasoning = useStore((s) => s.streamReasoning);
  const liveTools = useStore((s) => s.liveTools);
  const turn = useStore((s) => s.turn);
  const error = useStore((s) => s.error);
  const setDraft = useStore((s) => s.setDraft);
  const workspace = useStore((s) => s.workspace);

  const bottomRef = useRef<HTMLDivElement>(null);
  useEffect(() => {
    bottomRef.current?.scrollIntoView({ behavior: "smooth", block: "end" });
  }, [messages, streamText, liveTools.length, streaming]);

  const empty = messages.length === 0 && !streaming;

  return (
    <section className="chat">
      {error && <div className="error-bar">{error}</div>}

      {empty ? (
        <div className="empty-state">
          <div className="empty-mark">⌬</div>
          <h1>自带模型，自建智能体</h1>
          <p className="muted">
            当前工作区 <code>{workspace || "未授权"}</code>。选一个模型，描述目标，Agent 会自己决定读哪些文件、跑哪条命令。
          </p>
          <div className="suggestions">
            {SUGGESTIONS.map((s) => (
              <button key={s} className="suggestion" onClick={() => setDraft(s)}>
                {s}
              </button>
            ))}
          </div>
        </div>
      ) : (
        <div className="msg-list">
          {messages.map((m, i) => (
            <MessageRow key={i} message={m} />
          ))}

          {streaming && (
            <div className="msg assistant live">
              <div className="msg-head">
                <span className="role-tag">Agent</span>
                {turn.modelRef && <span className="msg-model">{turn.modelRef}</span>}
                {turn.steps > 0 && <span className="msg-model">第 {turn.steps} 步</span>}
              </div>
              {streamReasoning && (
                <details className="reasoning">
                  <summary>思考过程</summary>
                  <pre>{streamReasoning}</pre>
                </details>
              )}
              {streamText ? <RichText text={streamText} /> : <span className="typing"><i /><i /><i /></span>}
              {liveTools.map((t) => (
                <ToolCardView key={t.callId} card={t} />
              ))}
            </div>
          )}
          <div ref={bottomRef} />
        </div>
      )}
    </section>
  );
}

function MessageRow({ message }: { message: Message }) {
  if (message.role === "tool") {
    return (
      <div className="msg tool">
        {message.parts.map((p, i) =>
          p.type === "tool_result" ? (
            <details key={i} className={`tool-result ${p.is_error ? "err" : ""}`}>
              <summary>
                工具结果 · <code>{p.name}</code> {p.is_error ? "（失败）" : ""}
              </summary>
              <pre>{p.content.length > 6000 ? `${p.content.slice(0, 6000)}\n…（已截断）` : p.content}</pre>
            </details>
          ) : null,
        )}
      </div>
    );
  }

  if (message.role === "system") {
    return <div className="msg system">{message.parts.map((p, i) => (p.type === "text" ? <span key={i}>{p.text}</span> : null))}</div>;
  }

  const isUser = message.role === "user";
  const textParts = message.parts.filter((p): p is Extract<ContentPart, { type: "text" }> => p.type === "text");
  const toolUses = message.parts.filter((p): p is Extract<ContentPart, { type: "tool_use" }> => p.type === "tool_use");
  const meta = message.meta;

  return (
    <div className={`msg ${isUser ? "user" : "assistant"}`}>
      <div className="msg-head">
        <span className="role-tag">{isUser ? "你" : "Agent"}</span>
        {meta?.model_ref && <span className="msg-model">{meta.model_ref}</span>}
        {meta?.latency_ms != null && <span className="msg-model">{meta.latency_ms}ms</span>}
        {meta?.usage && (
          <span className="msg-model">
            {meta.usage.input_tokens}↑ {meta.usage.output_tokens}↓
          </span>
        )}
        {meta?.cost_usd != null && meta.cost_usd > 0 && <span className="msg-model">${meta.cost_usd.toFixed(5)}</span>}
      </div>
      {textParts.map((p, i) => (
        <RichText key={i} text={p.text} />
      ))}
      {toolUses.length > 0 && (
        <div className="tool-chips">
          {toolUses.map((t) => (
            <span key={t.id} className="tool-chip">
              调用 <code>{t.name}</code>
            </span>
          ))}
        </div>
      )}
    </div>
  );
}
