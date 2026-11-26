import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { realpathSync } from "node:fs";
import { fileURLToPath } from "node:url";

// 项目目录可能位于 junction/符号链接下（例如 C:\Users\x\Doubao -> D:\folder）。
// 统一用真实路径作为 root，否则 Rollup 的 html 插件会拿到两个不同前缀而构建失败。
const root = realpathSync(fileURLToPath(new URL(".", import.meta.url)));

export default defineConfig({
  root,
  plugins: [react()],
  clearScreen: false,
  // 目录经 junction 链接时，Vite 的依赖预构建扫描会把同一模块解析成两个路径，
  // 触发 depsInfo 取空导致进程崩溃；改为显式声明依赖，跳过自动扫描。
  optimizeDeps: {
    noDiscovery: true,
    include: ["react", "react-dom/client", "zustand", "@tauri-apps/api/core", "@tauri-apps/api/event", "@tauri-apps/plugin-dialog"],
  },
  server: { port: 1420, strictPort: true, host: "127.0.0.1", watch: { ignored: ["**/src-tauri/**", "**/core/**"] } },
  build: { target: "es2021", outDir: "dist", sourcemap: false },
});
