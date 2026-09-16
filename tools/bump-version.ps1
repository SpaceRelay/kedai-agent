# 统一修改全仓库版本号(单一入口,避免 7 处手工同步漏改)。
# 覆盖:根 package.json、web/package.json、server-rs/Cargo.toml、
#       src-tauri/Cargo.toml、launcher/Cargo.toml、src-tauri/tauri.conf.json、
#       package-lock.json(顶层 + packages[""] 两处;依赖项版本不动)、
#       MAINTENANCE.md 的版本行与「最后更新」日期。
# Cargo.lock 中包自身版本无需手改,下次 cargo 构建会自动同步。
# 一致性由 build.ps1 开头的 Assert-VersionConsistency 把关(改漏即构建报错)。
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

# 版本号须为合法 semver:核心三段 + 可选预发布后缀(`-beta` / `-rc.1` 等)。
# 注意:后缀必须用 `-` 连接——cargo 只接受 `0.3.0-beta`,**不接受** `0.3.0beta`
# (报 unexpected character 'b' after patch version number),故按 semver 校验,
# 并在拒绝时给出可操作的提示。
if ($Version -notmatch '^\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?$') {
    throw "版本号格式非法:「$Version」。支持 x.y.z 或 x.y.z-预发布(如 0.3.0-beta);预发布后缀必须用 - 连接,不能写成 0.3.0beta。"
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

# lock 文件:package-lock.json v3 中前两处 "version" 恰为顶层与 packages[""] 的自身版本
# (其余 version 都属于依赖包,一律不动)。逐个替换,不整文件重排格式。
#
# 注意:必须**推进搜索偏移**再找第二处。早期实现每轮都从文件头 Match/Replace(count=1),
# 结果第二处始终命中已被替换的同一位置(替换后仍是合法匹配),导致 packages[""] 的版本
# 永远改不到——顶层改了、workspaces 根没改,校验严格时会被 npm ci 判为 lock 与 package.json
# 不一致。
function Update-LockFileVersion([string]$RelativePath) {
    $path = Join-Path $Root $RelativePath
    $content = [System.IO.File]::ReadAllText($path)
    $regex = New-Object regex '("version"\s*:\s*")[^"]*(")'
    $new = $content
    $offset = 0
    $replaced = 0
    for ($i = 0; $i -lt 2; $i++) {
        $m = $regex.Match($new, $offset)
        if (-not $m.Success) { break }
        $prefix = $m.Groups[1].Value
        $suffix = $m.Groups[2].Value
        $new = $new.Substring(0, $m.Index) + $prefix + $Version + $suffix +
               $new.Substring($m.Index + $m.Length)
        # 跳过本次替换结果,使下一轮匹配到「下一处」而非原地
        $offset = $m.Index + $prefix.Length + $Version.Length + $suffix.Length
        $replaced++
    }
    if ($replaced -lt 2) {
        Write-Host "[警告] $RelativePath 只匹配到 $replaced 处自身版本(预期 2),已跳过写入" -ForegroundColor Yellow
        $script:skipped++
        return
    }
    if ($new -eq $content) {
        Write-Host "[跳过] $RelativePath 已是 $Version" -ForegroundColor DarkGray
        $script:skipped++
        return
    }
    Write-Host "[$(if ($DryRun) { '预览' } else { '修改' })] $RelativePath : 自身版本 x$replaced 处 -> $Version" -ForegroundColor Green
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
    $verRegex = New-Object regex '(版本:v)\d+\.\d+\.\d+(?:-[0-9A-Za-z][0-9A-Za-z.-]*)?'
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
Update-LockFileVersion "package-lock.json"
Update-MaintenanceDoc "MAINTENANCE.md"

Write-Host "======================================================" -ForegroundColor Cyan
if ($DryRun) {
    Write-Host "预览完成:将修改 $changed 个文件,跳过 $skipped 个。去掉 -DryRun 执行实际写入。" -ForegroundColor Yellow
} else {
    Write-Host "完成:修改 $changed 个文件,跳过 $skipped 个。" -ForegroundColor Green
    Write-Host "提示:Cargo.lock 中包自身版本会在下次 cargo 构建时自动同步;发版请跑 npm run build:all。" -ForegroundColor Yellow
}
