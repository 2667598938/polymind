# 构建与验证

## 环境要求

| 组件 | 版本 | 检查命令 |
| --- | --- | --- |
| Node.js | ≥ 20 | `node -v` |
| npm | ≥ 10 | `npm -v` |
| Rust | ≥ 1.77（stable，MSVC 工具链） | `rustc -V` |
| Windows | 10/11 + VS 2022 Build Tools（C++ 桌面开发） | `vswhere` 能定位 |
| macOS | 12+ + Xcode Command Line Tools | `xcode-select -p` |
| Linux | Ubuntu 22.04+，需 WebKitGTK | `pkg-config --modversion webkit2gtk-4.1` |

## 本地验证（提交前必跑）

```powershell
pwsh -File scripts/verify.ps1
```

等价于：

```bash
cd core && cargo test            # 内核单测
npx tsc --noEmit                 # 前端类型
npx vite build                   # 产物校验
```

## 开发模式

```bash
npm install
npm run tauri dev
```

前端改动热更新；Rust 改动会触发增量编译（首次约 3—8 分钟）。

## 打包

```bash
npm run tauri build
```

产物：

- Windows：`src-tauri/target/release/bundle/nsis/*.exe`
- macOS：`.../bundle/dmg/*.dmg`、`*.app`
- Linux：`.../bundle/appimage/*.AppImage`

图标已由 `npx tauri icon app-icon.png` 生成到 `src-tauri/icons/`，替换品牌时重跑该命令即可。

## 常见问题

**1. Vite 构建报 `fileName ... must be neither absolute nor relative`**

项目目录位于 junction/符号链接下时会出现（例如 `C:\Users\<你>\Doubao` 指向别的盘）。
`vite.config.ts` 已用 `realpathSync` 把 root 固定为真实路径；若仍失败，直接在真实路径下执行构建。

**2. `cargo` 报 `can't find crate for std`**

Rust 安装不完整（只有 rustc 没有 rust-std）。修复：

```bash
rustup toolchain install stable --force --no-self-update
rustc --print sysroot   # 确认 lib/rustlib/<target>/lib 下有 libstd-*.rlib
```

**3. 打包时报缺少图标**

确认 `tauri.conf.json` 的 `bundle.icon` 指向 `icons/*`，且 `src-tauri/icons/icon.ico` 存在。

**4. 首次启动白屏**

用 `npm run tauri dev` 而不是直接打开 `index.html`；前端通过 `@tauri-apps/api` 的 IPC 访问后端，浏览器里没有 `__TAURI_INTERNALS__`。

**5. 工具全部提示"未授权工作区"**

设置 → 权限与上下文 → 添加工作区；或在首次启动时选择目录。内核不会默认把当前目录当工作区，这是刻意的。

**6. `npm run dev` 启动即崩溃：`Cannot read properties of undefined (reading 'imports')`**

junction 目录下 Vite 依赖预构建的 metafile 路径与实际 cacheDir 对不上，属 Vite 6 的路径别名问题。
`package.json` 的 `dev` 已改为 `node scripts/dev.mjs`：先 `chdir` 到 realpath 再启动 Vite，
保证 cwd、root、cacheDir 三者前缀一致。换机器时保留这个启动器即可。

## CI 建议（GitHub Actions）

```yaml
name: verify
on: [push, pull_request]
jobs:
  core:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: dtolnay/rust-toolchain@stable
      - run: cargo test --manifest-path core/Cargo.toml
  ui:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: actions/setup-node@v4
        with: { node-version: 20 }
      - run: npm ci
      - run: npx tsc --noEmit
      - run: npx vite build
```
