// 以真实路径启动 Vite。
// 项目常被放在 junction（C:\Users\...\Doubao -> D:\doubaofolder）下：
// 若 Vite 的 cwd 是链接路径而 root 是真实路径，依赖预构建的 metafile 路径
// 会对不上，导致 "Cannot read properties of undefined (reading 'imports')" 崩溃。
// 这里先 chdir 到 realpath，再用同一个 Node 启动 Vite。
import { realpathSync } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import path from "node:path";

const root = realpathSync(fileURLToPath(new URL("..", import.meta.url)));
process.chdir(root);

const viteBin = path.join(root, "node_modules", "vite", "bin", "vite.js");
const child = spawn(process.execPath, [viteBin, ...process.argv.slice(2)], {
  cwd: root,
  stdio: "inherit",
  env: process.env,
});

child.on("exit", (code, signal) => {
  if (signal) process.kill(process.pid, signal);
  else process.exit(code ?? 0);
});
