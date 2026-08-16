# Kedai 一键构建脚本
# 1) 构建前端 web/dist(Vue 3 + Vite)
# 2) 编译 Rust 后端(内嵌 web/dist 的单二进制 kedai-server.exe)
# 3) -Tauri 时额外打包桌面应用(NSIS 安装程序)
#
# 用法:
#   .\build.ps1            # 前端 + Rust release,exe 复制到 dist\ 后删除 server-rs\target
#   .\build.ps1 -Dev       # 前端 + Rust debug(供 cargo run 开发调试;保留编译缓存不清理)
#   .\build.ps1 -NoWeb     # 仅 Rust release(复用现有 web/dist),exe 复制到 dist\ 后清理
#   .\build.ps1 -Tauri     # 前端 + Rust release + Tauri 桌面打包(需 tauri CLI;打包后清理 server-rs 与 src-tauri 的 target)

param(
    [switch]$Dev,
    [switch]$NoWeb,
    [switch]$Tauri
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path

Write-Host "========== Kedai Build ==========" -ForegroundColor Cyan

# 1) 前端构建
if (-not $NoWeb) {
    Write-Host "[1/2] 构建前端 (web/dist) ..." -ForegroundColor Green
    Push-Location $Root
    try {
        if (-not (Test-Path "$Root\package-lock.json")) {
            throw "缺少 package-lock.json,无法执行确定性构建。请先恢复 lockfile。"
        }
        if (-not (Test-Path "$Root\node_modules")) {
            throw "缺少前端依赖。构建脚本不会自动安装依赖,请先在项目根目录执行 npm ci。"
        }
        # vite 的 reporter 警告写 stderr,PS 5.1 会包成 NativeCommandError 中止脚本;
        # 与 cargo 同款处理:经 cmd 合并流(不附加 exit 语句,否则 %ERRORLEVEL% 在解析期
        # 恒为 0 会吞掉失败退出码),成败以退出码判断
        $ErrorActionPreference = "Continue"
        & $env:ComSpec /d /c "npm run build -w web 2>&1"
        $code = $LASTEXITCODE
        if ($code -ne 0) {
            throw "前端构建失败(exit=$code)"
        }
    } finally {
        $ErrorActionPreference = "Stop"
        Pop-Location
    }
    if (-not (Test-Path "$Root\web\dist\index.html")) {
        throw "前端构建失败:未生成 web/dist/index.html"
    }
    Write-Host "[OK] 前端构建完成" -ForegroundColor Green
}

# 2) Rust 编译
Write-Host "[2/2] 编译 Rust 后端 ..." -ForegroundColor Green
Push-Location "$Root\server-rs"
try {
    # cargo 的编译进度写 stderr;PowerShell 5.1 会把 native stderr 包成 NativeCommandError
    # 显示红字(即使编译成功)。经 cmd 内联合并 stdout/stderr,PS 只看到普通字符串,
    # 不再产生 ErrorRecord;成败以退出码判断(不附加 exit 语句,见前端构建处说明)。
    $ErrorActionPreference = "Continue"
    if ($Dev) {
        & $env:ComSpec /d /c "cargo build 2>&1"
    } else {
        & $env:ComSpec /d /c "cargo build --release 2>&1"
    }
    $code = $LASTEXITCODE
    if ($code -ne 0) {
        throw "cargo 编译失败(exit=$code)"
    }
} finally {
    # 无条件恢复脚本默认值(脚本开头即为 "Stop")
    $ErrorActionPreference = "Stop"
    Pop-Location
}

$exe = if ($Dev) { "$Root\server-rs\target\debug\kedai-server.exe" } else { "$Root\server-rs\target\release\kedai-server.exe" }
if (-not (Test-Path $exe)) {
    throw "Rust 编译失败:未生成 $exe"
}
Write-Host "[OK] 后端编译完成: $exe" -ForegroundColor Green

# 3) Tauri 桌面打包(可选)
if ($Tauri) {
    Write-Host "[3/3] 打包 Tauri 桌面应用 ..." -ForegroundColor Green
    Push-Location $Root
    try {
        $ErrorActionPreference = "Continue"
        & $env:ComSpec /d /c "npx --no-install @tauri-apps/cli build 2>&1"
        $code = $LASTEXITCODE
        if ($code -ne 0) {
            throw "Tauri 打包失败(exit=$code),请先安装: npm install -D @tauri-apps/cli"
        }
    } finally {
        $ErrorActionPreference = "Stop"
        Pop-Location
    }
    $installer = Get-ChildItem "$Root\src-tauri\target\release\bundle\nsis\*.exe" -ErrorAction SilentlyContinue | Select-Object -First 1
    if ($installer) {
        Write-Host "[OK] 安装程序: $($installer.FullName)" -ForegroundColor Green
        # 安装包位于 target 内,清理 target 前先复制到 dist\ 保留产物
        $BackupDir = Join-Path $Root "dist"
        New-Item -ItemType Directory -Force -Path $BackupDir | Out-Null
        Copy-Item $installer.FullName $BackupDir -Force
        Write-Host "[OK] 安装包已保留至: $(Join-Path $BackupDir $installer.Name)" -ForegroundColor Green
    } else {
        Write-Host "[提示] 未找到 NSIS 安装程序,请检查 src-tauri\target\release\bundle\nsis\" -ForegroundColor Yellow
    }
}

# 编译完成,清除编译产物目录(释放磁盘;下次构建全量重编,即约 500 个 crate)。
# - 生产构建(默认 / -NoWeb):先把 kedai-server.exe 复制到 dist\ 保留,
#   再删除整个 server-rs\target(含缓存与中间产物,回收数 GB 磁盘)。
# - -Tauri:额外删除整个 src-tauri\target(安装包已复制到 dist\)。
# - -Dev(开发调试):保留,便于 cargo run 增量编译。
# 文件被占用时删除可能失败,仅警告不中止。
if (-not $Dev) {
    if (Test-Path $exe) {
        $distDir = Join-Path $Root "dist"
        New-Item -ItemType Directory -Force -Path $distDir | Out-Null
        Copy-Item $exe (Join-Path $distDir (Split-Path $exe -Leaf)) -Force
        Write-Host "[保留] 已复制 $(Split-Path $exe -Leaf) 到 dist\" -ForegroundColor Green
    }
    if (Test-Path "$Root\server-rs\target") {
        try {
            Remove-Item -Recurse -Force "$Root\server-rs\target" -ErrorAction Stop
            Write-Host "[清理] 已删除 $Root\server-rs\target" -ForegroundColor Yellow
        } catch {
            Write-Host "[警告] 清理 $Root\server-rs\target 失败: $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }
}
if ($Tauri -and (Test-Path "$Root\src-tauri\target")) {
    try {
        Remove-Item -Recurse -Force "$Root\src-tauri\target" -ErrorAction Stop
        Write-Host "[清理] 已删除 $Root\src-tauri\target" -ForegroundColor Yellow
    } catch {
        Write-Host "[警告] 清理 $Root\src-tauri\target 失败: $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

Write-Host "==================================" -ForegroundColor Cyan
if ($Dev) {
    Write-Host "启动方式: $exe"
} else {
    Write-Host "后端产物: $Root\dist\kedai-server.exe"
}
if ($Tauri) { Write-Host "桌面安装包: $Root\dist\" -ForegroundColor Green }
