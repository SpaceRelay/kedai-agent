# 构建正式 Windows 便携目录:dist\Kedai-portable\Kedai.exe
# -NoWeb:跳过前端构建,复用现有 web/dist(供 build.ps1 接续调用,避免重复构建)
param(
    [string]$OutputDir = "",
    [switch]$NoWeb
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path)
. (Join-Path $Root "tools\Write-BuildStamp.ps1")
if ([string]::IsNullOrWhiteSpace($OutputDir)) {
    $OutputDir = Join-Path $Root "dist\Kedai-portable"
} elseif (-not [System.IO.Path]::IsPathRooted($OutputDir)) {
    $OutputDir = Join-Path $Root $OutputDir
}

Write-Host "========== Kedai Portable Build ==========" -ForegroundColor Cyan
Push-Location $Root
try {
    if (-not $NoWeb) {
        Write-Host "[1/3] 构建前端 ..." -ForegroundColor Green
        if (-not (Test-Path (Join-Path $Root "node_modules"))) {
            npm install
            if ($LASTEXITCODE -ne 0) { throw "npm install 失败(exit=$LASTEXITCODE)" }
        }
        npm run build -w web
        if ($LASTEXITCODE -ne 0) { throw "前端构建失败(exit=$LASTEXITCODE)" }
    } else {
        Write-Host "[1/3] 跳过前端构建(-NoWeb),复用现有 web/dist" -ForegroundColor Yellow
        if (-not (Test-Path (Join-Path $Root "web\dist\index.html"))) {
            throw "指定了 -NoWeb 但缺少 web\dist\index.html,请先执行前端构建"
        }
    }

    Write-Host "[2/3] 编译 Tauri release 可执行文件 ..." -ForegroundColor Green
    # 前端新鲜度检测(同 build.ps1):kedai-server 编译期嵌入 web/dist,
    # 清掉 kedai-server 指纹防止增量编译复用旧前端(不触发全量重编)。
    $DistIndex = Join-Path $Root "web\dist\index.html"
    $DesktopExe = Join-Path $Root "src-tauri\target\release\kedai-portable.exe"
    if ((Test-Path $DistIndex) -and (Test-Path $DesktopExe)) {
        $distTime = (Get-Item $DistIndex).LastWriteTime
        $exeTime = (Get-Item $DesktopExe).LastWriteTime
        if ($distTime -gt $exeTime) {
            Write-Host "[检测] 前端更新于 $distTime,晚于现有 exe($exeTime),清 kedai-server 指纹强制重编" -ForegroundColor Yellow
            $ManifestPath = Join-Path $Root "src-tauri\Cargo.toml"
            & $env:ComSpec /d /c "cargo clean -p kedai-server --manifest-path `"$ManifestPath`" 2>&1"
            if ($LASTEXITCODE -ne 0) { throw "cargo clean 失败(exit=$LASTEXITCODE)" }
        }
    }
    # 独立二进制名避免已运行的开发版锁住 kedai-desktop.exe。
    # 与 build.ps1 同款处理:cargo 进度写 stderr,PS 5.1 在 EAP=Stop 下会包成
    # NativeCommandError 中止脚本,且经 powershell -File 调用时退出码可能为 0(失败被吞)。
    # 经 cmd 内联合并流,成败以退出码判断,保证失败如实传播。
    $ErrorActionPreference = "Continue"
    & $env:ComSpec /d /c "cargo build --release --manifest-path `"$(Join-Path $Root 'src-tauri\Cargo.toml')`" --bin kedai-portable 2>&1"
    $code = $LASTEXITCODE
    $ErrorActionPreference = "Stop"
    if ($code -ne 0) { throw "Tauri 编译失败(exit=$code)" }
} finally {
    Pop-Location
}

$DesktopExe = Join-Path $Root "src-tauri\target\release\kedai-portable.exe"
if (-not (Test-Path $DesktopExe)) {
    throw "未生成桌面可执行文件:$DesktopExe"
}

Write-Host "[3/3] 组装便携目录 ..." -ForegroundColor Green
# 目录内 exe 可能正在运行:Windows 不允许删除运行中的 exe,整体 Remove-Item 会失败。
# 失败时把旧目录改名让位(.old)再新建,与 build.ps1 复制 dist\kedai-server.exe 的
# 「改名让位」策略一致——旧进程继续跑旧代码,新产物正常就位。
if (Test-Path $OutputDir) {
    try {
        Remove-Item -Recurse -Force $OutputDir -ErrorAction Stop
    } catch {
        $staleDir = "$OutputDir.old"
        if (Test-Path $staleDir) {
            try {
                Remove-Item -Recurse -Force $staleDir -ErrorAction Stop
            } catch {
                throw "旧便携目录被占用,且历史备份 $staleDir 也无法清理;请关闭 Kedai.exe 后重试"
            }
        }
        Move-Item $OutputDir $staleDir -Force -ErrorAction Stop
        Write-Host "[提示] 旧便携目录被运行中的进程占用,已改名为 $(Split-Path $staleDir -Leaf) 让位;关闭旧进程后可删除" -ForegroundColor Yellow
    }
}
New-Item -ItemType Directory -Force -Path $OutputDir | Out-Null
Copy-Item $DesktopExe (Join-Path $OutputDir "Kedai.exe")

# 构建指纹 sidecar:与 dist\kedai-server.exe.build.json 同算法,
# start.ps1 / launcher 启动前比对两者 dist_hash 即可判断测试版与便携版是否同步
$stampPath = Write-KedaiBuildStamp -Root $Root -ExePath (Join-Path $OutputDir "Kedai.exe")
Write-Host "[指纹] 已写出 $(Split-Path $stampPath -Leaf)" -ForegroundColor Green

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

# src-tauri\target 删除后,之前 Android 构建在 jniLibs 里留的 .so 符号链接会悬空,
# 导致后续压缩/备份仓库报「系统找不到指定的路径」并中断;此处一并清理
# (派生文件,见 gen/android/app/.gitignore 的 jniLibs 规则,下次 Android 构建自动重建)。
Clear-KedaiDanglingJniLibs -Root $Root | Out-Null
