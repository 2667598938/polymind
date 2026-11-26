import { useEffect } from "react";
import Sidebar from "./components/Sidebar";
import TopBar from "./components/TopBar";
import ChatView from "./components/ChatView";
import Composer from "./components/Composer";
import ApprovalDialog from "./components/ApprovalDialog";
import SettingsPanel from "./components/SettingsPanel";
import { ipc } from "./lib/ipc";
import { useStore } from "./stores/store";

export default function App() {
  const bootstrap = useStore((s) => s.bootstrap);
  const applyEvent = useStore((s) => s.applyEvent);
  const setApproval = useStore((s) => s.setApproval);
  const ready = useStore((s) => s.ready);
  const error = useStore((s) => s.error);
  const toast = useStore((s) => s.toast);
  const clearToast = useStore((s) => s.clearToast);
  const approval = useStore((s) => s.approval);
  const settingsOpen = useStore((s) => s.settingsOpen);

  useEffect(() => {
    void bootstrap();
    let unlistenStream: (() => void) | undefined;
    let unlistenApproval: (() => void) | undefined;
    (async () => {
      try {
        unlistenStream = await ipc.onEvent((ev) => {
          void applyEvent(ev);
        });
        unlistenApproval = await ipc.onApproval((req) => setApproval(req));
      } catch {
        /* 浏览器预览模式下没有 Tauri 事件总线，忽略 */
      }
    })();
    return () => {
      unlistenStream?.();
      unlistenApproval?.();
    };
  }, [bootstrap, applyEvent, setApproval]);

  useEffect(() => {
    if (!toast) return;
    const t = window.setTimeout(() => clearToast(), 4000);
    return () => window.clearTimeout(t);
  }, [toast, clearToast]);

  return (
    <div className="app-shell">
      <Sidebar />
      <main className="main-pane">
        <TopBar />
        {ready ? <ChatView /> : <BootScreen error={error} />}
        <Composer />
      </main>
      {approval && <ApprovalDialog request={approval} />}
      {settingsOpen && <SettingsPanel />}
      {toast && <div className="toast">{toast}</div>}
    </div>
  );
}

function BootScreen({ error }: { error: string | null }) {
  return (
    <div className="boot-screen">
      <div className="boot-mark" />
      <h2>正在加载工作台</h2>
      {error ? (
        <p className="boot-error">{error}</p>
      ) : (
        <p className="muted">读取配置、会话与工具清单…</p>
      )}
    </div>
  );
}
