import { useEffect, useRef } from "react";
import { useStore } from "../stores/store";

export default function Composer() {
  const draft = useStore((s) => s.draft);
  const setDraft = useStore((s) => s.setDraft);
  const send = useStore((s) => s.send);
  const streaming = useStore((s) => s.streaming);
  const modelRef = useStore((s) => s.modelRef);
  const workspace = useStore((s) => s.workspace);
  const approvalMode = useStore((s) => s.approvalMode);

  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.style.height = "auto";
    el.style.height = `${Math.min(el.scrollHeight, 220)}px`;
  }, [draft]);

  const canSend = draft.trim().length > 0 && !streaming;

  const submit = () => {
    if (!canSend) return;
    void send(draft);
  };

  return (
    <div className="composer">
      <div className="composer-box">
        <textarea
          ref={ref}
          value={draft}
          placeholder={streaming ? "Agent 正在执行，可点右上角停止…" : "描述目标，例如：把这个目录的前端代码改成 TypeScript 严格模式，并跑通构建"}
          onChange={(e) => setDraft(e.target.value)}
          onKeyDown={(e) => {
            if (e.key === "Enter" && !e.shiftKey && !e.nativeEvent.isComposing) {
              e.preventDefault();
              submit();
            }
          }}
          rows={3}
          spellCheck={false}
          disabled={streaming}
        />
        <div className="composer-bar">
          <div className="composer-hints">
            <span className="hint-item" title={workspace}>
              工作区 {workspace ? shortPath(workspace) : "未授权"}
            </span>
            <span className="hint-sep">·</span>
            <span className="hint-item">{modelRef || "未选模型"}</span>
            <span className="hint-sep">·</span>
            <span className="hint-item">{approvalMode === "ask" ? "写操作需审批" : approvalMode === "auto" ? "自动执行" : "只读"}</span>
          </div>
          <div className="composer-actions">
            <span className="char-count">{draft.length}</span>
            <button className="send-btn" onClick={submit} disabled={!canSend}>
              {streaming ? "执行中" : "发送"}
            </button>
          </div>
        </div>
      </div>
      <div className="composer-foot">
        <span>Enter 发送 · Shift+Enter 换行</span>
        <span>工具调用与文件写入均会记录在本地 SQLite 审计表</span>
      </div>
    </div>
  );
}

function shortPath(p: string): string {
  const parts = p.replace(/\\/g, "/").split("/");
  return parts.length > 2 ? `…/${parts.slice(-2).join("/")}` : p;
}
