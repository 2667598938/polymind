import { useState } from "react";
import { useStore } from "../stores/store";
import type { ProviderView } from "../lib/types";

const KIND_OPTIONS = [
  { value: "openai", label: "OpenAI / 兼容网关（DeepSeek、Moonshot、豆包 Ark、通义、one-api…）" },
  { value: "anthropic", label: "Anthropic Claude" },
  { value: "gemini", label: "Google Gemini" },
  { value: "azure_openai", label: "Azure OpenAI" },
  { value: "ollama", label: "本地 Ollama" },
];

interface Props {
  editing: ProviderView | null;
  onDone: () => void;
}

export default function ProviderForm({ editing, onDone }: Props) {
  const saveProvider = useStore((s) => s.saveProvider);
  const testProvider = useStore((s) => s.testProvider);

  const [id, setId] = useState(editing?.id ?? "");
  const [kind, setKind] = useState(editing?.kind ?? "openai");
  const [label, setLabel] = useState(editing?.label ?? "");
  const [baseUrl, setBaseUrl] = useState(editing?.base_url ?? "");
  const [apiKey, setApiKey] = useState("");
  const [defaultModel, setDefaultModel] = useState(editing?.default_model ?? "");
  const [models, setModels] = useState((editing?.models ?? []).join(", "));
  const [inputPrice, setInputPrice] = useState("0");
  const [outputPrice, setOutputPrice] = useState("0");
  const [busy, setBusy] = useState(false);
  const [note, setNote] = useState<string | null>(null);

  const submit = async () => {
    if (!id.trim() || !defaultModel.trim()) {
      setNote("id 与默认模型必填");
      return;
    }
    const modelList = models
      .split(",")
      .map((m) => m.trim())
      .filter(Boolean);
    if (modelList.length === 0) modelList.push(defaultModel.trim());
    setBusy(true);
    setNote(null);
    try {
      await saveProvider({
        id: id.trim(),
        kind,
        label: label.trim(),
        base_url: baseUrl.trim(),
        api_key: apiKey.trim() ? apiKey.trim() : null,
        default_model: defaultModel.trim(),
        models: modelList,
        pricing: {
          input_per_mtok: Number(inputPrice) || 0,
          output_per_mtok: Number(outputPrice) || 0,
          cache_read_per_mtok: 0,
        },
        enabled: true,
      });
      onDone();
    } catch (e) {
      setNote(String(e));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div className="provider-form">
      <h3>{editing ? `编辑供应商 · ${editing.id}` : "新增供应商"}</h3>
      <div className="form-grid">
        <label>
          <span>标识 id</span>
          <input value={id} onChange={(e) => setId(e.target.value)} placeholder="deepseek" disabled={!!editing} />
        </label>
        <label>
          <span>协议类型</span>
          <select value={kind} onChange={(e) => setKind(e.target.value)}>
            {KIND_OPTIONS.map((o) => (
              <option key={o.value} value={o.value}>
                {o.label}
              </option>
            ))}
          </select>
        </label>
        <label>
          <span>显示名称</span>
          <input value={label} onChange={(e) => setLabel(e.target.value)} placeholder="DeepSeek" />
        </label>
        <label>
          <span>Base URL</span>
          <input value={baseUrl} onChange={(e) => setBaseUrl(e.target.value)} placeholder="留空用默认；Azure 填 deployments 地址" />
        </label>
        <label className="full">
          <span>API Key {editing?.has_key ? `（已保存 ${editing.key_masked}，留空则不修改）` : ""}</span>
          <input type="password" value={apiKey} onChange={(e) => setApiKey(e.target.value)} placeholder="sk-…" autoComplete="off" />
        </label>
        <label>
          <span>默认模型</span>
          <input value={defaultModel} onChange={(e) => setDefaultModel(e.target.value)} placeholder="deepseek-chat" />
        </label>
        <label>
          <span>可选模型（逗号分隔）</span>
          <input value={models} onChange={(e) => setModels(e.target.value)} placeholder="deepseek-chat, deepseek-reasoner" />
        </label>
        <label>
          <span>输入价格 $/M tokens</span>
          <input value={inputPrice} onChange={(e) => setInputPrice(e.target.value)} placeholder="0" />
        </label>
        <label>
          <span>输出价格 $/M tokens</span>
          <input value={outputPrice} onChange={(e) => setOutputPrice(e.target.value)} placeholder="0" />
        </label>
      </div>

      {note && <div className="form-note">{note}</div>}

      <div className="form-actions">
        {editing && (
          <button
            className="ghost-btn"
            onClick={async () => {
              setBusy(true);
              try {
                const res = await testProvider(editing.id);
                setNote(`连通正常 · ${res.latency_ms}ms · 发现 ${res.models.length} 个模型`);
              } catch (e) {
                setNote(String(e));
              } finally {
                setBusy(false);
              }
            }}
            disabled={busy}
          >
            测试连通
          </button>
        )}
        <button className="primary-btn" onClick={() => void submit()} disabled={busy}>
          {busy ? "保存中…" : "保存"}
        </button>
      </div>
    </div>
  );
}
