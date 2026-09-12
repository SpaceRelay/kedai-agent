# 统一修改全仓库版本号(单一入口,避免 6 处手工同步漏改)。
# 覆盖:根 package.json、web/package.json、server-rs/Cargo.toml、
#       src-tauri/Cargo.toml、launcher/Cargo.toml、src-tauri/tauri.conf.json、
#       MAINTENANCE.md 的版本行与「最后更新」日期。
# Cargo.lock 中包自身版本无需手改,下次 cargo 构建会自动同步。
#
# 用法:
#   .\tools\bump-version.ps1 0.3.0            # 把全仓库版本号改为 0.3.0
#   .\tools\bump-version.ps1 0.3.0 -DryRun    # 只预览改动,不写文件
#   npm run version:bump -- 0.3.0             # 等效 npm 入口
param(
    [Parameter(Mandatory = $true, Position = 0)]
    [string]$Version,
    [switch]$DryRun
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)

if ($Version -notmatch '^\d+\.\d+\.\d+$') {
    throw "版本号格式非法:「$Version」。仅支持 x.y.z 纯数字形式(预发布/构建后缀请手工评估 NSIS 兼容性后再加)。"
}

$utf8NoBom = New-Object System.Text.UTF8Encoding($false)
$changed = 0
$skipped = 0

# JSON 类:替换第一处顶层 "version": "x.y.z"
function Update-JsonVersion([string]$RelativePath) {
    $path = Join-Path $Root $RelativePath
    $content = [System.IO.File]::ReadAllText($path)
    $regex = New-Object regex '("version"\s*:\s*")[^"]*(")'
    $m = $regex.Match($content)
    if (-not $m.Success) {
        Write-Host "[警告] $RelativePath 未找到 version 字段,已跳过" -ForegroundColor Yellow
        $script:skipped++
        return
    }
    $old = $m.Groups[0].Value
    $new = $regex.Replace($content, "`${1}$Version`${2}", 1)
    if ($new -eq $content) {
        Write-Host "[跳过] $RelativePath 已是 $Version" -ForegroundColor DarkGray
        $script:skipped++
        return
    }
    Write-Host "[$(if ($DryRun) { '预览' } else { '修改' })] $RelativePath : $old -> `"version`": `"$Version`"" -ForegroundColor Green
    if (-not $DryRun) { [System.IO.File]::WriteAllText($path, $new, $utf8NoBom) }
    $script:changed++
}

# TOML 类:[package] 段永远位于文件头部,替换行首第一处 version = "x.y.z"
function Update-TomlVersion([string]$RelativePath) {
    $path = Join-Path $Root $RelativePath
    $content = [System.IO.File]::ReadAllText($path)
    $regex = New-Object regex '(?m)^version\s*=\s*"[^"]*"'
    $m = $regex.Match($content)
    if (-not $m.Success) {
        Write-Host "[警告] $RelativePath 未找到 version 行,已跳过" -ForegroundColor Yellow
        $script:skipped++
        return
    }
    $old = $m.Groups[0].Value
    $new = $regex.Replace($content, "version = `"$Version`"", 1)
    if ($new -eq $content) {
        Write-Host "[跳过] $RelativePath 已是 $Version" -ForegroundColor DarkGray
        $script:skipped++
        return
    }
    Write-Host "[$(if ($DryRun) { '预览' } else { '修改' })] $RelativePath : $old -> version = `"$Version`"" -ForegroundColor Green
    if (-not $DryRun) { [System.IO.File]::WriteAllText($path, $new, $utf8NoBom) }
    $script:changed++
}

# 文档:版本行 v 后版本号 + 「最后更新」日期
function Update-MaintenanceDoc([string]$RelativePath) {
    $path = Join-Path $Root $RelativePath
    $content = [System.IO.File]::ReadAllText($path)
    $today = Get-Date -Format "yyyy-MM-dd"
    $new = $content
    $verRegex = New-Object regex '(版本:v)\d+\.\d+\.\d+'
    $m = $verRegex.Match($new)
    if ($m.Success) {
        $new = $verRegex.Replace($new, "`${1}$Version", 1)
        Write-Host "[$(if ($DryRun) { '预览' } else { '修改' })] $RelativePath : $($m.Groups[0].Value) -> 版本:v$Version" -ForegroundColor Green
    } else {
        Write-Host "[警告] $RelativePath 未找到「版本:v...」行,已跳过版本替换" -ForegroundColor Yellow
    }
    $dateRegex = New-Object regex '(最后更新:)\d{4}-\d{2}-\d{2}'
    if ($dateRegex.Match($new).Success) {
        $new = $dateRegex.Replace($new, "`${1}$today", 1)
        Write-Host "[$(if ($DryRun) { '预览' } else { '修改' })] $RelativePath : 最后更新 -> $today" -ForegroundColor Green
    }
    if ($new -ne $content) {
        if (-not $DryRun) { [System.IO.File]::WriteAllText($path, $new, $utf8NoBom) }
        $script:changed++
    } else {
        $script:skipped++
    }
}

Write-Host "========== Kedai Bump Version -> $Version $(if ($DryRun) { '(DryRun 预览)' }) ==========" -ForegroundColor Cyan

Update-JsonVersion "package.json"
Update-JsonVersion "web\package.json"
Update-TomlVersion "server-rs\Cargo.toml"
Update-TomlVersion "src-tauri\Cargo.toml"
Update-TomlVersion "launcher\Cargo.toml"
Update-JsonVersion "src-tauri\tauri.conf.json"
Update-MaintenanceDoc "MAINTENANCE.md"

Write-Host "======================================================" -ForegroundColor Cyan
if ($DryRun) {
    Write-Host "预览完成:将修改 $changed 个文件,跳过 $skipped 个。去掉 -DryRun 执行实际写入。" -ForegroundColor Yellow
} else {
    Write-Host "完成:修改 $changed 个文件,跳过 $skipped 个。" -ForegroundColor Green
    Write-Host "提示:Cargo.lock 中包自身版本会在下次 cargo 构建时自动同步;发版请跑 npm run build:all。" -ForegroundColor Yellow
}
