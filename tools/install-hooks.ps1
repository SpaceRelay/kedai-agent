# install-hooks.ps1 — 安装仓库 git hooks(当前为 pre-push 门禁)
#
# 用法:powershell -NoProfile -ExecutionPolicy Bypass -File tools/install-hooks.ps1
#
# 背景:.git/hooks/ 不在版本控制内(换机/重新 clone 会丢失),故把 hook 脚本放在
# tools/hooks/ 纳入版本控制,由本脚本复制安装。装了 hook 的分支在 push 前会跑
# check-all.ps1 -Quick,失败即中止推送。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$src = Join-Path $PSScriptRoot 'hooks\pre-push'
$dstDir = Join-Path $root '.git\hooks'
$dst = Join-Path $dstDir 'pre-push'

if (-not (Test-Path $src)) { throw "未找到 hook 源文件:$src" }
if (-not (Test-Path $dstDir)) { throw "未找到 .git\hooks 目录(是否在 git 仓库内?):$dstDir" }

Copy-Item -Path $src -Destination $dst -Force
Write-Host "[OK] 已安装 pre-push hook → $dst" -ForegroundColor Green

# Git for Windows 会直接执行该 Shell 脚本(bash 可用),无需 chmod;
# 若在 WSL/MSYS 环境使用,补一次可执行位。
$bash = Get-Command bash -ErrorAction SilentlyContinue
if ($bash) {
    & $bash.Source -c "chmod +x '$($dst -replace '\\','/')'" 2>$null
}

Write-Host "    推送前将运行:tools/check-all.ps1 -Quick" -ForegroundColor Cyan
Write-Host "    紧急绕过:git push --no-verify(交付前须补跑完整检查)" -ForegroundColor Yellow
