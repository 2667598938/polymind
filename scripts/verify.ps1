# PolyMind 本地验证脚本
# 用法：pwsh -File scripts/verify.ps1
# 会依次做：Rust 内核单测 → 前端类型检查 → 前端构建

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

Write-Host "[1/3] cargo test (core)" -ForegroundColor Cyan
Push-Location "$root\core"
cargo test --color never
if ($LASTEXITCODE -ne 0) { Pop-Location; throw "Rust 测试失败" }
Pop-Location

Write-Host "[2/3] tsc --noEmit" -ForegroundColor Cyan
npx tsc --noEmit
if ($LASTEXITCODE -ne 0) { throw "TypeScript 类型检查失败" }

Write-Host "[3/3] vite build" -ForegroundColor Cyan
npx vite build
if ($LASTEXITCODE -ne 0) { throw "前端构建失败" }

Write-Host "全部通过。" -ForegroundColor Green
