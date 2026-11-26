# 供应商配置手册

所有供应商最终都归为五种协议类型。**只要对方兼容 OpenAI 的 `/chat/completions`，就选 `openai`。**

## 1. OpenAI 兼容（`openai`）

| 供应商 | Base URL | 默认模型示例 | 备注 |
| --- | --- | --- | --- |
| OpenAI | `https://api.openai.com/v1` | `gpt-4o` / `gpt-4o-mini` | 官方 |
| DeepSeek | `https://api.deepseek.com/v1` | `deepseek-chat` / `deepseek-reasoner` | 便宜、中文好 |
| 豆包（火山方舟） | `https://ark.cn-beijing.volces.com/api/v3` | 接入点 ID（`ep-xxx`） | 需先在方舟创建推理接入点 |
| 通义（DashScope 兼容） | `https://dashscope.aliyuncs.com/compatible-mode/v1` | `qwen-plus` / `qwen-max` | 兼容模式 |
| Moonshot | `https://api.moonshot.cn/v1` | `moonshot-v1-8k` | 长上下文 |
| 智谱 | `https://open.bigmodel.cn/api/paas/v4` | `glm-4-plus` | |
| OpenRouter | `https://openrouter.ai/api/v1` | `anthropic/claude-sonnet-4-5` | 一个 key 走多家，注意加价 |
| 自建 one-api / new-api | `http://你的网关:3000/v1` | 网关里的模型名 | 内网部署首选 |
| Ollama | `http://127.0.0.1:11434/v1` | `qwen2.5:7b` | 本地、无需 key |

> 豆包 Ark 的"模型"字段填接入点 ID，不是模型家族名；这是它和其他网关最大的差异。

## 2. Azure OpenAI（`azure_openai`）

Base URL 要带到 deployments：

```
https://<resource>.openai.azure.com/openai/deployments/<deployment>
```

内核会自动追加 `?api-version=2024-10-21`，并使用 `api-key` 头而不是 `Authorization`。
模型字段填 deployment 名（通常与模型同名）。

## 3. Anthropic（`anthropic`）

Base URL：`https://api.anthropic.com`（可换成中转，内核会自动补 `/v1`）。
模型示例：`claude-sonnet-4-5`、`claude-opus-4-1`。
注意：`max_tokens` 必填，内核默认 8192；Claude 的思考内容以 `thinking_delta` 事件回传，界面可折叠显示。

## 4. Gemini（`gemini`）

Base URL：`https://generativelanguage.googleapis.com`。
模型示例：`gemini-2.5-pro`、`gemini-2.5-flash`。
Gemini 的 function call 是一次性完整参数（不是增量），内核会合成 `ToolCallStart + ToolCallArgs` 两个事件，Agent 层无感。

## 5. 在配置里手写一条

也可以直接编辑 `config.json`（应用退出后改，否则会被 UI 覆盖）：

```json
{
  "id": "deepseek",
  "kind": "openai",
  "label": "DeepSeek",
  "base_url": "https://api.deepseek.com/v1",
  "default_model": "deepseek-chat",
  "models": ["deepseek-chat", "deepseek-reasoner"],
  "pricing": { "input_per_mtok": 0.27, "output_per_mtok": 1.1, "cache_read_per_mtok": 0.07 },
  "enabled": true
}
```

**不要手写 `api_key`**：明文 key 不落盘，正确做法是用设置界面填入，或：

```bash
cargo run --manifest-path core/Cargo.toml --bin polymind-cli -- --home .polymind set-key --provider deepseek sk-xxx
```

## 6. 计价字段

`input_per_mtok` / `output_per_mtok` / `cache_read_per_mtok` 单位是**美元 / 百万 token**。
填 0 不会报错，但账本里该模型的成本会显示为 0；建议按官网价格填，便于比较"这轮到底花了多少"。

## 7. 兜底模型链

设置里填：

```
gemini/gemini-2.5-flash, openai/gpt-4o-mini
```

主模型在**尚未输出任何 token**时遇到 429/5xx/网络错误，会按顺序切换；已经输出一半的回答不会中途换模型（避免语义断裂）。
