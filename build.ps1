# Kedai 一键构建脚本(默认双端同步)
# 1) 构建前端 web/dist(Vue 3 + Vite)
# 2) 编译 Rust 后端(内嵌 web/dist 的单二进制 kedai-server.exe)→ dist\
# 3) 默认继续构建便携版 dist\Kedai-portable\Kedai.exe:两版各自把编译那一刻的
#    web/dist 冻结进二进制,分开构建必然漂移,因此双端同步产出是默认行为而非选项。
#
# 用法:
#   .\build.ps1               # 前端 + Rust release + 便携版(双端同步;发布/交付/日常构建用这条)
#   .\build.ps1 -TestOnly     # 仅测试版(快速迭代);结尾会警告便携版未同步
#   .\build.ps1 -Dev          # 前端 + Rust debug(供 cargo run 开发调试;保留编译缓存,仅按需清 kedai-server 指纹)
#   .\build.ps1 -NoWeb        # 复用现有 web/dist,仅重编 Rust(双端)
#   .\build.ps1 -Tauri        # 双端同步之外再追加 NSIS 安装包(需 tauri CLI)
#   .\build.ps1 -WithPortable # 兼容旧用法;双端同步已是默认,此开关为无操作
#
# 前端新鲜度:web/dist 在编译期经 include_dir! 嵌入 kedai-server(见 server-rs/src/api/mod.rs),
# 由 server-rs/build.rs 声明 rerun-if-changed 保证 cargo 感知;脚本另做 mtime 检测兜底。
# 末尾删除 target 仅用于释放磁盘,不是新鲜度的保证手段。
#
# 构建指纹:每个 dist 产物写出 <exe>.build.json(version/build_time/dist_hash,
# 见 tools/Write-BuildStamp.ps1),供 start.ps1 与 launcher 启动前比对两版是否同步;
# server-rs/build.rs 另把 KEDAI_DIST_HASH/KEDAI_BUILD_TIME 编进二进制,经 /api/health 暴露。

param(
    [switch]$Dev,
    [switch]$NoWeb,
    [switch]$Tauri,
    [switch]$WithPortable,
    [switch]$TestOnly,
    [switch]$SkipChecks,
    # 后端 cargo target 根(仅作用于 server-rs,不影响 src-tauri 便携版产物路径)。
    # 默认 server-rs\target。当该目录被安全软件拦截「新建可执行文件的执行」时
    # (build script exe 报 os error 5),用本参数把后端产物外置到白名单目录,例如:
    #   .\build.ps1 -RustTargetDir D:\kedai-build
    [string]$RustTargetDir
)

$ErrorActionPreference = "Stop"
$Root = Split-Path -Parent $MyInvocation.MyCommand.Path
. (Join-Path $Root "tools\Write-BuildStamp.ps1")

# 版本一致性断言:全仓自身版本号必须一致,任一漏改即构建失败。
# 覆盖 7 处声明(package.json / web/package.json / package-lock.json /
# server-rs|src-tauri|launcher 的 Cargo.toml / tauri.conf.json)。
# 由 tools/bump-version.ps1 统一维护;此处只读校验,不代改。
function Assert-VersionConsistency {
    $declared = [ordered]@{}
    # 统一口径:取文件第一处 "version" 字段(与 tools/bump-version.ps1 同语义)。
    # 不用 ConvertFrom-Json:PS 5.1 解析 package-lock.json 会因依赖键名报错。
    function Get-JsonVersion([string]$rel) {
        $p = Join-Path $Root $rel
        if (-not (Test-Path $p)) { return "(缺失)" }
        $m = [regex]::Match([System.IO.File]::ReadAllText($p), '"version"\s*:\s*"([^"]*)"')
        if ($m.Success) { return $m.Groups[1].Value }
        return "(解析失败)"
    }
    function Get-TomlVersion([string]$rel) {
        $p = Join-Path $Root $rel
        if (-not (Test-Path $p)) { return "(缺失)" }
        $m = [regex]::Match([System.IO.File]::ReadAllText($p), '(?m)^version\s*=\s*"([^"]*)"')
        if ($m.Success) { return $m.Groups[1].Value }
        return "(解析失败)"
    }
    $declared["package.json"] = Get-JsonVersion "package.json"
    $declared["web/package.json"] = Get-JsonVersion "web\package.json"
    $declared["package-lock.json"] = Get-JsonVersion "package-lock.json"
    $declared["server-rs/Cargo.toml"] = Get-TomlVersion "server-rs\Cargo.toml"
    $declared["src-tauri/Cargo.toml"] = Get-TomlVersion "src-tauri\Cargo.toml"
    $declared["launcher/Cargo.toml"] = Get-TomlVersion "launcher\Cargo.toml"
    $declared["src-tauri/tauri.conf.json"] = Get-JsonVersion "src-tauri\tauri.conf.json"

    $distinct = @($declared.Values | Select-Object -Unique)
    if ($distinct.Count -gt 1) {
        Write-Host "[FAIL] 版本号不一致,构建中止。请跑 npm run version:bump -- <x.y.z> 统一:" -ForegroundColor Red
        foreach ($k in $declared.Keys) {
            Write-Host ("       {0,-30} {1}" -f $k, $declared[$k]) -ForegroundColor Red
        }
        throw "版本号不一致(详见上列)"
    }
    Write-Host "版本一致性校验通过:$($distinct[0])(7 处声明)" -ForegroundColor DarkGray
}


if ($Dev -and $WithPortable) {
    throw "-Dev 与 -WithPortable 不能同时使用:便携版构建会清理 -Dev 需要保留的编译缓存"
}
if ($TestOnly -and $WithPortable) {
    throw "-TestOnly 与 -WithPortable 语义相反:双端同步已是默认行为,请去掉 -WithPortable"
}
if ($TestOnly -and $Dev) {
    throw "-TestOnly 与 -Dev 语义重叠:-Dev 本就只构建测试版,请去掉 -TestOnly"
}

# 双端同步是默认行为;-TestOnly / -Dev 是明确的单端快速通道
$BuildPortable = -not $TestOnly -and -not $Dev

# 后端产物目录(仅 server-rs):优先级 -RustTargetDir 参数 > CARGO_TARGET_DIR 环境变量
# > 默认 server-rs\target。
#
# 为何需要:某些机器的安全软件会拦截「在 server-rs\target 下新建的可执行文件」的**执行**
# (实测 build.rs 编译出的 build-script-build.exe 报「拒绝访问 os error 5」;同目录下
# 复制进去的既有 exe 却能正常运行,说明不是目录权限问题,而是对新建 exe 的实时防护)。
# 此时用 -RustTargetDir 把后端产物外置到白名单目录即可正常构建。
#
# 注意:这里**只解析路径并显式传给 server-rs 的 cargo 命令(--target-dir)**,
# 不设置 CARGO_TARGET_DIR 环境变量——否则会一并作用于 src-tauri,使便携版产物
# 落到错误位置(build-portable.ps1 期望 src-tauri\target\release\kedai-portable.exe)。
$CargoTargetDir = if ($RustTargetDir) {
    if ([System.IO.Path]::IsPathRooted($RustTargetDir)) { $RustTargetDir }
    else { Join-Path $Root $RustTargetDir }
} elseif ($env:CARGO_TARGET_DIR) {
    if ([System.IO.Path]::IsPathRooted($env:CARGO_TARGET_DIR)) { $env:CARGO_TARGET_DIR }
    else { Join-Path $Root $env:CARGO_TARGET_DIR }
} else {
    "$Root\server-rs\target"
}
$null = New-Item -ItemType Directory -Force -Path $CargoTargetDir -ErrorAction SilentlyContinue
if ($CargoTargetDir -ne "$Root\server-rs\target") {
    Write-Host "[信息] 后端产物目录外置: $CargoTargetDir(仅 server-rs;src-tauri 仍用自身 target)" -ForegroundColor DarkGray
}

# 产物占用让位:若开发时用 cargo run / start.ps1 起过 kedai-server,运行中的 exe 会让
# cargo 重新链接报「failed to remove file ... os error 5」(见 Write-BuildStamp.ps1 说明)。
# 编译前统一做一次改名让位,无需杀进程;门禁(check-all)与下面的 [2/3] 编译都受益。
Clear-KedaiLockedServerArtifacts -TargetDir $CargoTargetDir

# 0) 门禁:先跑测试与静态检查,失败即中止(避免先花十分钟构建才发现测试红)。
#    -Quick 跳过 check-all 内部的前端 vite build(下方 [1/3] 会再构建一次,避免重复)。
#    逃生开关 -SkipChecks 仅限本地应急;交付前必须补跑一次完整的 tools/check-all.ps1。
if (-not $SkipChecks) {
    Write-Host "[0/3] 门禁检查(check-all.ps1 -Quick) ..." -ForegroundColor Green
    # 把后端产物目录一并传给门禁,使 check-all 的 cargo fmt/clippy/test 与本次构建
    # 使用同一 target 根(否则外置产物时门禁会走默认 server-rs\target 而失败)。
    $checkArgs = @('-NoProfile', '-ExecutionPolicy', 'Bypass',
        '-File', (Join-Path $Root 'tools\check-all.ps1'), '-Quick')
    if ($CargoTargetDir -ne "$Root\server-rs\target") {
        $checkArgs += @('-RustTargetDir', $CargoTargetDir)
    }
    & powershell @checkArgs
    if ($LASTEXITCODE -ne 0) {
        Write-Host "[FAIL] 门禁未通过,构建中止(仅本地应急可加 -SkipChecks)" -ForegroundColor Red
        exit 1
    }
    Write-Host "[OK] 门禁通过" -ForegroundColor Green
} else {
    Write-Host "[0/3] 已跳过门禁检查(-SkipChecks);交付前请补跑完整 check-all" -ForegroundColor Yellow
}

Assert-VersionConsistency

Write-Host "========== Kedai Build ==========" -ForegroundColor Cyan

# 1) 前端构建
if (-not $NoWeb) {
    Write-Host "[1/3] 构建前端 (web/dist) ..." -ForegroundColor Green
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

# 2) 前端新鲜度检测(兜底):kedai-server 用 include_dir! 在编译期嵌入 web/dist,
#    build.rs 已声明 rerun-if-changed 让 cargo 自动感知;此处再加 mtime 检测,
#    防止旧 exe 残留导致增量编译复用旧前端。仅当 exe 已存在且比 dist 旧时,
#    清掉 kedai-server 的编译指纹强制重编(不会触发 500 个 crate 全量重编)。
if (-not $NoWeb) {
    $distIndex = "$Root\web\dist\index.html"
    $exeCheck = if ($Dev) { "$CargoTargetDir\debug\kedai-server.exe" } else { "$CargoTargetDir\release\kedai-server.exe" }
    if ((Test-Path $distIndex) -and (Test-Path $exeCheck)) {
        $distTime = (Get-Item $distIndex).LastWriteTime
        $exeTime = (Get-Item $exeCheck).LastWriteTime
        if ($distTime -gt $exeTime) {
            Write-Host "[检测] 前端更新于 $distTime,晚于现有 exe($exeTime),清 kedai-server 指纹强制重编" -ForegroundColor Yellow
            Push-Location "$Root\server-rs"
            try {
                $ErrorActionPreference = "Continue"
                & $env:ComSpec /d /c "cargo clean -p kedai-server --target-dir `"$CargoTargetDir`" 2>&1"
            } finally {
                $ErrorActionPreference = "Stop"
                Pop-Location
            }
        }
    }
}

# 3) Rust 编译
Write-Host "[2/3] 编译 Rust 后端 ..." -ForegroundColor Green
Push-Location "$Root\server-rs"
try {
    # cargo 的编译进度写 stderr;PowerShell 5.1 会把 native stderr 包成 NativeCommandError
    # 显示红字(即使编译成功)。经 cmd 内联合并 stdout/stderr,PS 只看到普通字符串,
    # 不再产生 ErrorRecord;成败以退出码判断(不附加 exit 语句,见前端构建处说明)。
    $ErrorActionPreference = "Continue"
    if ($Dev) {
        & $env:ComSpec /d /c "cargo build --target-dir `"$CargoTargetDir`" 2>&1"
    } else {
        & $env:ComSpec /d /c "cargo build --release --target-dir `"$CargoTargetDir`" 2>&1"
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

$exe = if ($Dev) { "$CargoTargetDir\debug\kedai-server.exe" } else { "$CargoTargetDir\release\kedai-server.exe" }
if (-not (Test-Path $exe)) {
    throw "Rust 编译失败:未生成 $exe"
}
Write-Host "[OK] 后端编译完成: $exe" -ForegroundColor Green

# 附加) Tauri NSIS 安装包(可选;-Tauri 时)
if ($Tauri) {
    Write-Host "[附加] 打包 Tauri 桌面应用(NSIS) ..." -ForegroundColor Green
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
# - 生产构建(非 -Dev):先把 kedai-server.exe 复制到 dist\ 保留并写构建指纹 sidecar,
#   再删除整个 server-rs\target(含缓存与中间产物,回收数 GB 磁盘)。
# - -Tauri 且本次不构建便携版:额外删除整个 src-tauri\target(安装包已复制到 dist\);
#   本次要构建便携版时保留,交由 build-portable.ps1 复用缓存并统一清理。
# - -Dev(开发调试):保留,便于 cargo run 增量编译。
# 文件被占用时删除可能失败,仅警告不中止。
if (-not $Dev) {
    if (Test-Path $exe) {
        $distDir = Join-Path $Root "dist"
        New-Item -ItemType Directory -Force -Path $distDir | Out-Null
        $dest = Join-Path $distDir (Split-Path $exe -Leaf)
        try {
            Copy-Item $exe $dest -Force -ErrorAction Stop
        } catch [System.IO.IOException] {
            # 目标 exe 正在运行时 Windows 不允许覆盖,但允许改名:改名让位后新产物即可就位,
            # 旧进程继续跑旧代码不受影响,下次启动自动用新版
            $stale = "$dest.old"
            Move-Item $dest $stale -Force -ErrorAction Stop
            Copy-Item $exe $dest -Force -ErrorAction Stop
            Write-Host "[提示] 旧 $(Split-Path $exe -Leaf) 正在运行,已改名为 $(Split-Path $stale -Leaf) 让位;旧进程退出后可删除" -ForegroundColor Yellow
        }
        Write-Host "[保留] 已复制 $(Split-Path $exe -Leaf) 到 dist\" -ForegroundColor Green
        # 构建指纹 sidecar:start.ps1 / launcher 据此比对两版是否同步
        $stampPath = Write-KedaiBuildStamp -Root $Root -ExePath $dest
        Write-Host "[指纹] 已写出 $(Split-Path $stampPath -Leaf)" -ForegroundColor Green
    }
    # 清理产物释放磁盘(下次构建全量重编,是有意取舍)。
    # 路径必须与实际产物目录一致(2026-09-13 批次 1 修正):外置时硬编码 server-rs\target
    # 会清理不到。**但外置目录可能与他人共用**(本机 D:\kedai-build 下还挂着 Android 构建的
    # junction 目标 android-app-build)——2026-09-13 实测踩坑:整删外置目录把 Android 构建目录
    # 一并删除,导致 APK 构建在 `app:mergeUniversalReleaseJniLibsFolders` 报「无法创建目录」。
    # 故:默认路径整删(整个 target 都是 cargo 的);外置目录只清 cargo 自己的 profile 子目录。
    if ($CargoTargetDir -eq "$Root\server-rs\target") {
        if (Test-Path $CargoTargetDir) {
            try {
                Remove-Item -Recurse -Force $CargoTargetDir -ErrorAction Stop
                Write-Host "[清理] 已删除 $CargoTargetDir" -ForegroundColor Yellow
            } catch {
                Write-Host "[警告] 清理 $CargoTargetDir 失败: $($_.Exception.Message)" -ForegroundColor Yellow
            }
        }
    } else {
        foreach ($sub in @("debug", "release", "tmp", "CACHEDIR.TAG", ".rustc_info.json")) {
            $p = Join-Path $CargoTargetDir $sub
            if (Test-Path $p) {
                try {
                    Remove-Item -Recurse -Force $p -ErrorAction Stop
                    Write-Host "[清理] 已删除 $p" -ForegroundColor Yellow
                } catch {
                    Write-Host "[警告] 清理 $p 失败: $($_.Exception.Message)" -ForegroundColor Yellow
                }
            }
        }
        Write-Host "[提示] 外置产物目录只清 cargo 子目录,保留同目录下其它数据: $CargoTargetDir" -ForegroundColor DarkGray
    }
}
# 本次要构建便携版时保留 src-tauri\target,交由 build-portable.ps1 复用缓存并统一清理。
if ($Tauri -and -not $BuildPortable -and (Test-Path "$Root\src-tauri\target")) {
    try {
        Remove-Item -Recurse -Force "$Root\src-tauri\target" -ErrorAction Stop
        Write-Host "[清理] 已删除 $Root\src-tauri\target" -ForegroundColor Yellow
        # target 删除会让 Android 构建留下的 jniLibs .so 符号链接悬空(压缩/备份会报
        # 「系统找不到指定的路径」);派生文件,一并清理。
        Clear-KedaiDanglingJniLibs -Root $Root | Out-Null
    } catch {
        Write-Host "[警告] 清理 $Root\src-tauri\target 失败: $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

# 4) 便携版构建(默认行为):复用刚产出的 web/dist 与 src-tauri\target 编译缓存,
#    由 build-portable.ps1 组装 dist\Kedai-portable、写 sidecar 并统一清理 src-tauri\target。
#    被调脚本失败会以终止错误传播,此处无需再判退出码。
if ($BuildPortable) {
    Write-Host "[3/3] 构建便携版 dist\Kedai-portable\Kedai.exe(与测试版同一份前端) ..." -ForegroundColor Green
    & "$Root\tools\build-portable.ps1" -NoWeb
}

# 5) 图形启动器与快捷方式(幂等):项目根 Kedai.exe 是双击正式入口(过期询问重建/
#    端口探测/数据迁移/进程存活确认,见 launcher/src/main.rs)。Kedai.lnk 必须指向它,
#    而不是 dist 里的裸便携版 exe——后者双击没有任何新鲜度校验,是版本漂移的入口。
if (-not $Dev) {
    $launcherExe = Join-Path $Root "Kedai.exe"
    $launcherBuilt = Join-Path $Root "launcher\target\release\Kedai.exe"

    $launcherSrcNewer = $true
    if (Test-Path $launcherBuilt) {
        $srcLatest = Get-ChildItem (Join-Path $Root "launcher\src") -Recurse -File -ErrorAction SilentlyContinue |
            Measure-Object -Property LastWriteTime -Maximum |
            Select-Object -ExpandProperty Maximum
        $launcherSrcNewer = ($null -ne $srcLatest -and $srcLatest -gt (Get-Item $launcherBuilt).LastWriteTime) -or
            ((Get-Item (Join-Path $Root "launcher\Cargo.toml")).LastWriteTime -gt (Get-Item $launcherBuilt).LastWriteTime)
    }
    if ($launcherSrcNewer) {
        Write-Host "[附加] 编译图形启动器(launcher,零依赖秒级) ..." -ForegroundColor Green
        Push-Location $Root
        try {
            $ErrorActionPreference = "Continue"
            & $env:ComSpec /d /c "cargo build --release --manifest-path `"$Root\launcher\Cargo.toml`" 2>&1"
            $code = $LASTEXITCODE
            if ($code -ne 0) { throw "launcher 编译失败(exit=$code)" }
        } finally {
            $ErrorActionPreference = "Stop"
            Pop-Location
        }
    }
    if ((Test-Path $launcherBuilt) -and ((-not (Test-Path $launcherExe)) -or
        ((Get-Item $launcherBuilt).LastWriteTime -gt (Get-Item $launcherExe).LastWriteTime))) {
        try {
            Copy-Item $launcherBuilt $launcherExe -Force -ErrorAction Stop
            Write-Host "[附加] 已更新项目根 Kedai.exe(图形启动器)" -ForegroundColor Green
        } catch [System.IO.IOException] {
            Write-Host "[警告] 项目根 Kedai.exe 正在运行,本次未能更新启动器;关闭后重跑构建即可" -ForegroundColor Yellow
        }
    }

    # 快捷方式:仅修正目标与工作目录,图标等其余字段保持不动;lnk 缺失时顺手创建
    $lnkPath = Join-Path $Root "Kedai.lnk"
    try {
        $wshell = New-Object -ComObject WScript.Shell
        $sc = $wshell.CreateShortcut($lnkPath)
        if (($sc.TargetPath -ne $launcherExe) -or ($sc.WorkingDirectory -ne $Root)) {
            $sc.TargetPath = $launcherExe
            $sc.WorkingDirectory = $Root
            if ([string]::IsNullOrEmpty($sc.IconLocation)) {
                $portableForIcon = Join-Path $Root "dist\Kedai-portable\Kedai.exe"
                if (Test-Path $portableForIcon) { $sc.IconLocation = "$portableForIcon,0" }
            }
            $sc.Save()
            Write-Host "[附加] Kedai.lnk 已指向图形启动器(双击自带过期检测)" -ForegroundColor Green
        }
    } catch {
        Write-Host "[警告] 更新 Kedai.lnk 失败: $($_.Exception.Message)" -ForegroundColor Yellow
    }
}

Write-Host "==================================" -ForegroundColor Cyan
if ($Dev) {
    Write-Host "启动方式: $exe"
} else {
    Write-Host "测试版: $Root\dist\kedai-server.exe"
}
if ($Tauri) { Write-Host "桌面安装包: $Root\dist\" -ForegroundColor Green }

if ($BuildPortable) {
    Write-Host "便携版: $Root\dist\Kedai-portable\Kedai.exe" -ForegroundColor Green
    # 同步结论:两个 sidecar 的 dist_hash 必须一致;不一致说明构建链路出了裂缝,直接红字报警
    $hTest = Read-KedaiDistHash -ExePath (Join-Path $Root "dist\kedai-server.exe")
    $hPort = Read-KedaiDistHash -ExePath (Join-Path $Root "dist\Kedai-portable\Kedai.exe")
    if ($hTest -and $hPort -and ($hTest -eq $hPort)) {
        Write-Host "[同步] 测试版与便携版指纹一致(dist_hash=$($hTest.Substring(0,12))…)" -ForegroundColor Green
    } else {
        Write-Host "[严重] 两端指纹不一致或缺失(test=$hTest portable=$hPort),请立即重跑 .\build.ps1" -ForegroundColor Red
    }
} elseif ($TestOnly) {
    Write-Host "[警告] 本次为 -TestOnly 单端构建:便携版 dist\Kedai-portable\Kedai.exe 未更新。" -ForegroundColor Yellow
    Write-Host "       交付或日常使用 exe 版之前,请执行一次 .\build.ps1(默认双端同步)。" -ForegroundColor Yellow
}
