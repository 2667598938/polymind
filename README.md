# PolyMind · 多模型智能体工作台

> 自带模型（BYOK）、本地执行、全程可审批的桌面 Agent。
> 对标 Trae / Qoder / WorkBuddy 的「能干活」，但把模型选择权、额度与数据都留在你自己手里。

## 这是什么

PolyMind 是一个 Tauri 2 桌面应用：你在里面接入任意模型 API（OpenAI 兼容网关、Claude、Gemini、Azure、本地 Ollama），
它就能像一个 Coding Agent 那样在你的工作区里读文件、改代码、跑命令、调 MCP 工具——每一步写操作都先问你。

和同类产品的差别只有一句话：**不卖积分，不做云端中转，密钥不出本机。**

## 核心能力（V0.1）

| 能力 | 实现位置 | 说明 |
| --- | --- | --- |
| 多供应商统一接入 | `core/src/provider` | OpenAI 兼容 / Anthropic / Gemini / Azure / Ollama，含自定义 baseURL |
| 流式对话 | `core/src/provider/*` + `src/` | 自研 SSE 解析，逐 token 上屏，usage 实时结算 |
| Agent 工具循环 | `core/src/agent/runtime.rs` | ReAct：文本 → 工具调用 → 结果回灌 → 再推理，最多 16 步 |
| 六个内置工具 | `core/src/agent/tools` | `fs_read` `fs_write` `fs_list` `fs_grep` `run_command` `web_fetch` |
| 权限门 | `core/src/agent/permission.rs` | 路径牢笼 + 命令白名单 + 三级审批（逐次/本会话/拒绝） |
| MCP 插件 | `core/src/mcp` | stdio JSON-RPC，自动发现 `tools/list`，工具名加 `mcp__服务__工具` 前缀 |
| 会话与账本 | `core/src/store` | SQLite：会话、消息、用量、工具审计四张核心表 |
| 密钥保护 | `core/src/secrets.rs` | ChaCha20-Poly1305，密文落盘，明文只活在内核内存 |
| 上下文预算 | `core/src/agent/runtime.rs` | 超预算自动折叠早期消息，保留工具结果完整性 |
| 兜底模型链 | `core/src/agent/runtime.rs` | 主模型 5xx/429 时自动切备用，且只在该轮尚未出 token 时切 |

## 快速开始

前置：Node ≥ 20、Rust ≥ 1.77（带 MSVC 工具链）、Windows 10/11 或 macOS。

```bash
npm install
npm run tauri dev
```

首次启动会创建 `%APPDATA%\com.polymind.desktop\`（macOS 为 `~/Library/Application Support/com.polymind.desktop/`）：

```
config.json      # 供应商、审批策略、白名单（密钥是密文）
secrets.key      # 32 字节主密钥，仅当前用户可读
polymind.db      # 会话 / 消息 / 用量 / 审计
exports/         # 导出的 Markdown
```

### 配置第一个模型

1. 侧栏点「设置 → 供应商 → 添加供应商」；
2. 协议类型选 `openai`（DeepSeek、Moonshot、豆包 Ark、通义兼容模式都走这个）；
3. 填 Base URL 与 API Key，默认模型写 `deepseek-chat` 这类模型名；
4. 保存后点「测试连通」，再在顶栏模型下拉里选中它。

> 不想开图形界面时，可以用同一份内核的 CLI：
> ```bash
> cargo run --manifest-path core/Cargo.toml --bin polymind-cli -- --home .polymind providers
> cargo run --manifest-path core/Cargo.toml --bin polymind-cli -- --home .polymind run --prompt "列一下目录结构"
> ```

## 目录结构

```
polymind/
├─ core/                 # 纯 Rust 内核（可单测、可被别的前端复用）
│  ├─ src/model.rs       # 统一消息 / 流事件 / 工具描述 / 计价
│  ├─ src/provider/      # 三类适配器 + 注册表 + SSE 解析
│  ├─ src/agent/         # ReAct 循环、工具、权限门、上下文预算
│  ├─ src/mcp/           # JSON-RPC over stdio 客户端 + 管理器
│  ├─ src/store/         # SQLite 迁移与查询
│  ├─ src/secrets.rs     # 本地密钥库
│  └─ src/bin/cli.rs     # 命令行兜底入口
├─ src-tauri/            # Tauri 外壳：命令、状态、事件桥
├─ src/                  # React 18 + Zustand 界面
├─ docs/                 # PRD、架构、竞品、安全、路线图
└─ scripts/              # 构建与验证脚本
```

## 关键设计取舍

- **内核独立成 crate**：供应商协议、Agent 循环、权限判定都能 `cargo test`，不用起窗口。
- **不做"全家桶编辑器"**：不内嵌 Monaco、不做 LSP，专注"把任务交给 Agent 执行"这一件事；
  代码浏览交给用户自己的 IDE，PolyMind 只负责改和跑。
- **审批不是弹窗装饰**：风险分级写进工具实现里，路径由内核 canonicalize 后校验，命令不走 shell。
- **成本是显式的**：每个模型可配单价，流式 usage 落账本，界面上能看到"这一轮花了多少"。

## 文档索引

- [产品需求文档（PRD）](docs/PRD.md)
- [架构设计](docs/ARCHITECTURE.md)
- [竞品分析](docs/COMPETITIVE.md)
- [安全与威胁模型](docs/SECURITY.md)
- [路线图](docs/ROADMAP.md)
- [供应商配置手册](docs/PROVIDERS.md)

## 当前状态

V0.1 已在本机（Windows 11 / Node 22 / Rust 1.100）实测通过：

- **内核**：`cargo test` → 33 passed / 0 failed（SSE 分帧、三家协议的工具调用往返、路径越界、命令白名单、密钥加解密、SQLite 聚合、上下文折叠）。
- **前端**：`npx tsc --noEmit` 退出码 0；`npx vite build` 产出 `dist/`（JS 约 182 kB）。
- **外壳**：`cargo check` 通过；`npm run tauri dev` 能编译并打开窗口（标题 `PolyMind · 多模智能体工作台`）。

尚未验证的部分，不假装已完成：`tauri build` 安装包未产出；没有用真实 API Key 跑通过一次完整会话，
运行时链路由单元测试与类型/编译检查保证，首次真机联调仍需要你配置一个供应商。
