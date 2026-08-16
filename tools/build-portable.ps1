# 构建正式 Windows 便携目录:dist\Kedai-portable\Kedai.exe
param(
    [string]$OutputDir = ""
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $Root "dist\Kedai-portable"
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDir)) {
    $OutputDir = Join-Path $Root $OutputDir
}

Write-Host "========== Kedai Portable Build ==========" -ForegroundColor Cyan
Write-Host "[1/3] 构建前端 ..." -ForegroundColor Green
Push-Location $Root
try {
    if (-not (Test-Path (Join-Path $Root "node_modules"))) {
        npm install
        if ($LASTEXITCODE -ne 0) { throw "npm install 失败(exit=$LASTEXITCODE)" }
    }
    npm run build -w web
    if ($LASTEXITCODE -ne 0) { throw "前端构建失败(exit=$LASTEXITCODE)" }

    Write-Host "[2/3] 编译 Tauri release 可执行文件 ..." -ForegroundColor Green
    # 独立二进制名避免已运行的开发版锁住 kedai-desktop.exe。
    cargo build --release --manifest-path (Join-Path $Root "src-tauri\Cargo.toml") --bin kedai-portable
    if ($LASTEXITCODE -ne 0) { throw "Tauri 编译失败(exit=$LASTEXITCODE)" }
} finally {
    Pop-Location
}

$DesktopExe = Join-Path $Root "src-tauri\target\release\kedai-portable.exe"
if (-not (Test-Path $DesktopExe)) {
    throw "未生成桌面可执行文件:$DesktopExe"
}

Write-Host "[3/3] 组装便携目录 ..." -ForegroundColor Green
if (Test-Path $OutputDir) {
    Remove-Item -Recurse -Force $OutputDir
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
Copy-Item $DesktopExe (Join-Path $OutputDir "Kedai.exe")

$PortableReadme = @(
    "Kedai Windows 便携版",
    "",
    "1. 双击 Kedai.exe 启动,无需安装 Node.js 或 Rust。",
    "2. 数据统一保存在 %APPDATA%\com.kedai.app\data。",
    "3. 若便携目录来自项目根目录,首次启动会幂等复制项目 data;不会删除源数据,也不会覆盖已使用的桌面数据。",
    "4. 系统需要 Microsoft Edge WebView2 Runtime(Windows 10/11 通常已内置)。"
)
$PortableReadme | Set-Content -Path (Join-Path $OutputDir "README.txt") -Encoding UTF8

Write-Host "[OK] 便携目录:$OutputDir" -ForegroundColor Green
Write-Host "入口:$OutputDir\Kedai.exe"

# 打包完成,清除编译产物目录(释放磁盘;下次构建需重新编译)。
# 文件被占用(如 Kedai.exe 仍在运行)时删除可能失败,仅警告不中止。
$CleanDirs = @((Join-Path $Root "server-rs\target"), (Join-Path $Root "src-tauri\target"))
foreach ($dir in $CleanDirs) {
    if (Test-Path $dir) {
        try {
            Remove-Item -Recurse -Force $dir -ErrorAction Stop
            Write-Host "[清理] 已删除 $dir" -ForegroundColor Yellow
        } catch {
            Write-Host "[警告] 清理 $dir 失败: $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }
}
