# Kedai 统一启动器(PowerShell)
# 一个脚本管理两个版本:
#   - 测试版(默认):浏览器开发/调试模式,启动 kedai-server.exe + 自动打开浏览器。
#   - 正式版:启动 dist\Kedai-portable\Kedai.exe(便携版桌面应用,数据存 %APPDATA%\com.kedai.app)。
#
# 自动编译:启动前检测产品源码(web/src、server-rs/src、src-tauri/src 等)是否比
# 可执行产物新;过期则自动重新构建再启动,避免「改了代码还是旧版」。
# 用法:
#   .\start.ps1              # 测试版:启动服务并打开浏览器
#   .\start.ps1 -NoBrowser   # 测试版:启动服务但不打开浏览器
#   .\start.ps1 -Build       # 测试版:强制先执行 build.ps1 再启动(忽略过期检测)
#   .\start.ps1 -Portable    # 正式版:启动便携版 Kedai.exe(推荐日常使用;自动检测重建)
#   .\start.ps1 -NoRebuild   # 跳过自动重建,直接用现有产物启动(快速启动)
#
# 注意:正式版与测试版共用端口 3001 与同一数据目录,请勿同时运行。

param(
    [switch]$NoBrowser,
    [switch]$Build,
    [switch]$Portable,
    [switch]$NoRebuild
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path

# ===================== 源码过期检测 =====================
# 扫描产品源码目录/配置文件的最新修改时间;可执行产物(dist\Kedai-portable\Kedai.exe
# 或 server-rs\target\release\kedai-server.exe)比它旧则视为过期,启动前自动重建。
# 不纳入 start.ps1 自身(启动器改动不代表产品代码变化,避免误触发重建)。
function Get-LatestSourceTime {
    $ScanPaths = @(
        (Join-Path $Root "web\src"),
        (Join-Path $Root "web\public"),
        (Join-Path $Root "web\index.html"),
        (Join-Path $Root "web\vite.config.ts"),
        (Join-Path $Root "web\package.json"),
        (Join-Path $Root "server-rs\src"),
        (Join-Path $Root "server-rs\Cargo.toml"),
        (Join-Path $Root "server-rs\Cargo.lock"),
        (Join-Path $Root "src-tauri\src"),
        (Join-Path $Root "src-tauri\tauri.conf.json"),
        (Join-Path $Root "src-tauri\Cargo.toml"),
        (Join-Path $Root "src-tauri\Cargo.lock"),
        (Join-Path $Root "tools\build-portable.ps1"),
        (Join-Path $Root "build.ps1")
    )
    $latest = Get-Date "2000-01-01"
    foreach ($p in $ScanPaths) {
        if (-not (Test-Path $p)) { continue }
        $item = Get-Item $p
        if ($item.PSIsContainer) {
            $max = Get-ChildItem $p -Recurse -File -ErrorAction SilentlyContinue |
                Measure-Object -Property LastWriteTime -Maximum |
                Select-Object -ExpandProperty Maximum
            if ($null -ne $max -and $max -gt $latest) { $latest = $max }
        } elseif ($item.LastWriteTime -gt $latest) {
            $latest = $item.LastWriteTime
        }
    }
    return $latest
}

# 产物是否过期:产物缺失或源码比产物新超过 2 分钟。
# 2 分钟宽容窗口:构建过程/编辑器常会 touch 源文件但内容未变,
# 若按精确比较会每次启动都误判过期而触发 5 分钟全量重编(表现为「改了半天还是旧版」)。
function Test-ArtifactStale([string]$ArtifactPath) {
    if (-not (Test-Path $ArtifactPath)) { return $true }
    $srcTime = Get-LatestSourceTime
    $artTime = (Get-Item $ArtifactPath).LastWriteTime
    return $srcTime -gt $artTime.AddMinutes(2)
}

# ===================== 正式版(便携版桌面应用) =====================
if ($Portable) {
    $PortableExe = Join-Path $Root "dist\Kedai-portable\Kedai.exe"
    if (-not $NoRebuild -and (Test-ArtifactStale $PortableExe)) {
        Write-Host "========== Kedai 自动构建(检测到源码更新) ==========" -ForegroundColor Yellow
        Write-Host "[构建] 便携版产物过期或缺失,自动执行 build-portable.ps1 ..." -ForegroundColor Yellow
        & (Join-Path $Root "tools\build-portable.ps1")
        if ($LASTEXITCODE -ne 0) {
            Write-Host "[错误] 便携版构建失败,请查看上方错误输出。" -ForegroundColor Red
            Read-Host "按回车关闭窗口"
            exit 1
        }
        Write-Host "[OK] 构建完成,启动新版本。" -ForegroundColor Green
    } elseif (-not (Test-Path $PortableExe)) {
        Write-Host "[错误] 未找到正式版:$PortableExe" -ForegroundColor Red
        Write-Host "请先执行 npm run build:portable 构建便携目录,或使用测试版(.\start.ps1)。" -ForegroundColor Yellow
        exit 1
    }
    Write-Host "========== Kedai 正式版(便携版) ==========" -ForegroundColor Cyan
    Write-Host "[启动] $PortableExe" -ForegroundColor Green
    Start-Process -FilePath $PortableExe -WorkingDirectory (Split-Path $PortableExe)
    Write-Host "[OK] 正式版已启动;数据与日志位于 %APPDATA%\com.kedai.app。" -ForegroundColor Green
    Write-Host "[提示] 正式版与测试版共用端口 3001 与数据目录,运行正式版前请关闭测试版。" -ForegroundColor Yellow
    exit 0
}

# ===================== 测试版(浏览器开发/调试) =====================
# 服务端产物:优先用 build.ps1 复制到 dist\ 的持久产物,再回退到 target 目录(开发时直接 cargo build 的产物)。
$ServerCandidates = @(
    (Join-Path $Root "dist\kedai-server.exe"),
    (Join-Path $Root "server-rs\target\release\kedai-server.exe"),
    (Join-Path $Root "src-tauri\target\release\kedai-server.exe")
)
$ServerExe = $ServerCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
$WebDist = Join-Path $Root "web\dist\index.html"

Write-Host "========== Kedai Browser Dev ==========" -ForegroundColor Cyan

# 0) 构建触发:强制构建(-Build)/ 产物缺失 / 源码过期(-NoRebuild 跳过全部)
$NeedBuild = $Build
if (-not $NeedBuild -and -not $NoRebuild) {
    if (-not $ServerExe -or -not (Test-Path $WebDist)) {
        $NeedBuild = $true
    } elseif (Test-ArtifactStale $ServerExe) {
        $NeedBuild = $true
    } elseif ((Get-LatestSourceTime) -gt (Get-Item $WebDist).LastWriteTime) {
        $NeedBuild = $true
    }
}
if ($NeedBuild) {
    Write-Host "[自动构建] 产物缺失或源码更新,执行 build.ps1 ..." -ForegroundColor Yellow
    & (Join-Path $Root "build.ps1")
    if ($LASTEXITCODE -ne 0) { throw "构建失败" }
    # 重建后重新探测
    $ServerExe = $ServerCandidates | Where-Object { Test-Path $_ } | Select-Object -First 1
}
if (-not $ServerExe) { throw "未能定位 kedai-server.exe,请检查构建产物" }

# 健康检查:服务是否已在运行
function Test-ServerRunning {
    try {
        $resp = Invoke-WebRequest -Uri "http://127.0.0.1:3001/api/health" -UseBasicParsing -TimeoutSec 2
        return $resp.StatusCode -eq 200
    } catch { return $false }
}

if (-not (Test-ServerRunning)) {
    Write-Host "[启动] 启动 kedai-server.exe ..." -ForegroundColor Green
    $proc = Start-Process -FilePath $ServerExe -WorkingDirectory $Root -PassThru -WindowStyle Hidden

    # 等待就绪(最长 60 秒)
    $deadline = (Get-Date).AddSeconds(60)
    while ((Get-Date) -lt $deadline) {
        if (Test-ServerRunning) { break }
        Start-Sleep -Milliseconds 400
    }
    if (-not (Test-ServerRunning)) {
        Write-Host "[错误] 服务未能就绪,请查看 logs/ 目录。" -ForegroundColor Red
        exit 1
    }
    Write-Host "[OK] 服务已就绪 → http://127.0.0.1:3001" -ForegroundColor Green
} else {
    Write-Host "[OK] 服务已在运行 → http://127.0.0.1:3001" -ForegroundColor Green
}

if (-not $NoBrowser) {
    Start-Process "http://127.0.0.1:3001/"
}

Write-Host "==================================" -ForegroundColor Cyan
Write-Host "按任意键退出(将同时关闭服务)…"
$null = $Host.UI.RawUI.ReadKey("NoEcho,IncludeKeyDown")
if ($proc -and -not $proc.HasExited) {
    Stop-Process -Id $proc.Id -Force -ErrorAction SilentlyContinue
    Write-Host "[OK] 服务已关闭。"
}
