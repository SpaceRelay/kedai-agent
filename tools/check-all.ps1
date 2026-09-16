# check-all.ps1 — 本地 CI 一键检查:后端 fmt/clippy/test + 前端 typecheck/test/build
# 用法: npm run check  |  或 powershell -NoProfile -ExecutionPolicy Bypass -File tools/check-all.ps1
# 参数: -SkipRust 跳过后端编译; -SkipWeb 跳过前端编译; -Quick 只跑 test 不跑 build;
#       -AuditOnly 只跑审计段(两把 Cargo.lock + npm 生产依赖),跳过全部编译/测试/构建;
#       -StrictTypecheck 历史保留参数(2026-09-08 起 typecheck 已是硬门禁,此开关无差异)
#       -StrictAudit 历史保留参数(2026-09-13 起 cargo audit 已是硬门禁,此开关无差异)
#       -LooseAudit 把 cargo audit 降回警告档(仅在 advisory DB 不可用等特殊场景临时使用)
#       -Perf 追加性能门禁(需 Kedai 服务已在运行;见文件末尾「性能门禁」段)
#
# 审计段(独立安全门禁,与 -SkipRust/-SkipWeb 解耦;批次 6.1 升级):
#   - cargo audit 覆盖**两把锁**:server-rs/Cargo.lock + src-tauri/Cargo.lock(此前只扫前者)。
#     无法本地修复的 unmaintained 告警在 .cargo/audit.toml 逐条 ignore(附理由与复审时机);
#     真实漏洞/未忽略告警出现即 FAIL。
#   - npm audit --omit=dev 复核为 0 告警,由警告档提升为硬门禁;离线自动降 WARN。
param(
    [switch]$SkipRust,
    [switch]$SkipWeb,
    [switch]$Quick,
    [switch]$AuditOnly,
    [switch]$StrictTypecheck,
    [switch]$StrictAudit,
    [switch]$LooseAudit,
    # 性能门禁(2026-09-14 新增,默认关闭):需 Kedai 服务已在 $PerfBase 运行。
    # 打开后跑 tools/perf-baseline.mjs,按「p95 <= 基线 × 倍数 且 errors==0」判定。
    [switch]$Perf,
    [string]$PerfBase = 'http://127.0.0.1:3001',
    [double]$PerfFactor = 1.25,
    # 后端 cargo target 根(见 MAINTENANCE.md §10 条目 22:某些机器上安全软件拦截
    # server-rs\target 下新建 exe 的执行)。留空则用 CARGO_TARGET_DIR 环境变量或默认路径。
    [string]$RustTargetDir
)
$ErrorActionPreference = 'Continue'
$root = Split-Path -Parent $PSScriptRoot
$results = @()

# 占用让位助手(见 Write-BuildStamp.ps1):开发时 cargo run / start.ps1 起的 kedai-server
# 会锁住 target 下的产物,cargo 重新链接时报「failed to remove file ... os error 5」,
# 门禁会误报成权限/杀软问题并中止。编译前统一改名让位,无需杀进程。
. (Join-Path $root "tools\Write-BuildStamp.ps1")

# 解析后端产物目录:参数 > 环境变量 > 默认;统一经 $env:CARGO_TARGET_DIR 传给 cargo,
# 使 fmt/clippy/test 三处口径一致(显式导出后子进程与后续路径检查都据此走)。
if ($RustTargetDir) {
    $env:CARGO_TARGET_DIR = if ([System.IO.Path]::IsPathRooted($RustTargetDir)) { $RustTargetDir }
                            else { Join-Path $root $RustTargetDir }
}

# ---- 环境准备:MSVC vcvars64 + cargo 路径 ----
$vcvars = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
if (-not (Test-Path $vcvars)) {
    $vswhere = "${env:ProgramFiles(x86)}\Microsoft Visual Studio\Installer\vswhere.exe"
    if (Test-Path $vswhere) {
        $vsPath = & $vswhere -latest -products * -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath 2>$null
        if ($vsPath) { $vcvars = Join-Path $vsPath 'VC\Auxiliary\Build\vcvars64.bat' }
    }
}
$cargoBin = Join-Path $env:USERPROFILE '.cargo\bin'
$hasCargo = [bool](Get-Command cargo -ErrorAction SilentlyContinue) -or (Test-Path (Join-Path $cargoBin 'cargo.exe'))

# 把 vcvars64 设置的环境变量一次性导入当前 PowerShell 进程
# (不再每层 cmd /c 嵌套——PS5.1 原生命令引号传递不可靠,曾导致 clippy 误用 Git 的 link.exe)
function Import-VcVars([string]$VcvarsPath) {
    $setOut = cmd /c "`"$VcvarsPath`" >nul 2>&1 && set"
    foreach ($line in $setOut) {
        $i = $line.IndexOf('=')
        if ($i -gt 0) {
            Set-Item -Path ("Env:\" + $line.Substring(0, $i)) -Value $line.Substring($i + 1)
        }
    }
}

function Invoke-Stage([string]$Name, [scriptblock]$Body) {
    Write-Host "`n===== $Name =====" -ForegroundColor Cyan
    $sw = [System.Diagnostics.Stopwatch]::StartNew()
    & $Body
    $code = $LASTEXITCODE
    $sw.Stop()
    if ($code -ne 0) {
        Write-Host "[FAIL] $Name (exit=$code, 耗时 $([math]::Round($sw.Elapsed.TotalSeconds,1))s)" -ForegroundColor Red
        $script:results += [pscustomobject]@{ Stage = $Name; Result = 'FAIL'; Seconds = [math]::Round($sw.Elapsed.TotalSeconds,1) }
        Write-Host "`n===== 汇总: $Name 失败,后续阶段跳过 =====" -ForegroundColor Red
        $results | Format-Table -AutoSize
        exit 1
    }
    Write-Host "[ OK ] $Name (耗时 $([math]::Round($sw.Elapsed.TotalSeconds,1))s)" -ForegroundColor Green
    $script:results += [pscustomobject]@{ Stage = $Name; Result = 'OK'; Seconds = [math]::Round($sw.Elapsed.TotalSeconds,1) }
}

if (-not $SkipRust -and -not $AuditOnly) {
    if (-not $hasCargo) { Write-Host '未找到 cargo(也不在 ~\.cargo\bin),跳过后端;请先安装 Rust 工具链' -ForegroundColor Yellow }
    elseif (-not (Test-Path $vcvars)) { Write-Host "未找到 vcvars64.bat,跳过后端;请安装 VS BuildTools(MSVC)" -ForegroundColor Yellow }
    else {
        Import-VcVars $vcvars
        if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { $env:PATH = "$cargoBin;$env:PATH" }
        Push-Location "$root\server-rs"
        try {
            # 后端产物目录:默认 server-rs\target;若外部已设 CARGO_TARGET_DIR 则用其值
            # (某些机器上安全软件会拦截 server-rs\target 下**新建 exe** 的执行,报
            #  os error 5,需把产物外置。详见 MAINTENANCE.md §10 条目 22)。
            # 这里显式把变量导出给本轮 cargo 调用,并让 grep/路径类检查一致。
            $cargoTargetArgs = if ($env:CARGO_TARGET_DIR) { @('--target-dir', $env:CARGO_TARGET_DIR) } else { @() }
            if ($env:CARGO_TARGET_DIR) { Write-Host "[信息] 后端产物目录外置: $env:CARGO_TARGET_DIR" -ForegroundColor DarkGray }
            # 编译前让位:运行中的 kedai-server 会锁住产物,导致 cargo 链接失败(os error 5)
            $serverTargetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $root "server-rs\target" }
            Clear-KedaiLockedServerArtifacts -TargetDir $serverTargetDir
            Invoke-Stage 'cargo fmt --check'        { cargo fmt --check }
            Invoke-Stage 'cargo clippy'             { cargo clippy --all-targets @cargoTargetArgs -- -D warnings }
            # -j 2:本机并行链接曾撞 LNK1318/os error 1455(页面文件不足),限并发换稳定
            Invoke-Stage 'cargo test --workspace'   { cargo test --workspace -j 2 @cargoTargetArgs }
            # cargo audit 已迁出本块,升级为下方独立「审计段」:不再受 -SkipRust 影响,
            # 并覆盖 server-rs + src-tauri 两把锁。
        } finally { Pop-Location }
    }
}

# ---- 审计段(独立安全门禁,与 -SkipRust/-SkipWeb 解耦)----
# 2026-09-13 批次 6.1:cargo audit 由「只扫 server-rs」升级为双锁覆盖
# (server-rs/Cargo.lock + src-tauri/Cargo.lock,后者含 Tauri 桌面壳完整依赖树)。
# 无法本地修复的 unmaintained 告警列在 .cargo/audit.toml(逐条附理由与复审时机);
# 真实漏洞或未忽略告警出现时该锁 FAIL。
if ($AuditOnly) { Write-Host "`n##### 审计档:-AuditOnly(跳过编译/测试/构建)#####" -ForegroundColor DarkCyan }

# 对单把锁跑 cargo audit;返回 'ok' | 'warn'(无法拉 DB/离线,跳过) | 'fail'。
# 判定 fail-closed:仅当明确是 advisory DB / 索引拉取失败时才降 WARN。
function Invoke-CargoAuditLock {
    param([string]$Label, [string]$LockPath, [bool]$Loose)
    Write-Host "`n--- cargo audit: $Label ($LockPath) ---" -ForegroundColor Cyan
    # --no-yanked:yank 状态不是漏洞信号,且冷索引下该检查会大量超时刷屏(不影响漏洞判定)。
    $out = (cargo audit -f $LockPath --no-yanked 2>&1 | Out-String)
    if ($LASTEXITCODE -eq 0) {
        Write-Host "[ OK ] $Label 无已知漏洞" -ForegroundColor Green
        # exit 0 仍可能带 allowed warning(如 glib 的 unsound,无 CVE、无修复版本,
        # 不属 unmaintained 故未列入 .cargo/audit.toml)。打印出来保持透明,不拦截。
        if ($out -match 'allowed warning') {
            Write-Host (($out.Trim() -split "`n" | Where-Object { $_ -match 'Crate:|Version:|Warning:|Title:|ID:' }) -join "`n") -ForegroundColor DarkYellow
            Write-Host "[提示] $Label 存在未拦截的信息类告警(见上;非 CVE,评估后如需处理请登记 .cargo/audit.toml 或升级依赖)" -ForegroundColor DarkYellow
        }
        return 'ok'
    }
    if ($Loose) {
        Write-Host (($out.Trim() -split "`n" | Select-Object -Last 8) -join "`n") -ForegroundColor Yellow
        Write-Host "[WARN] $Label 审计未通过(警告档,-LooseAudit)" -ForegroundColor Yellow
        return 'warn'
    }
    if ($out -match '(failed to fetch|failed to load|failed to update|unable to fetch|could not be fetched|could not be loaded|resolve host|timed out|network|TLS|connection refused|ENOTFOUND)') {
        Write-Host "[WARN] $Label 无法拉取 advisory DB(离线/镜像不可达?),跳过该锁硬门禁" -ForegroundColor Yellow
        Write-Host (($out.Trim() -split "`n" | Select-Object -Last 3) -join ' ') -ForegroundColor DarkGray
        return 'warn'
    }
    Write-Host $out
    Write-Host "[FAIL] $Label 发现依赖漏洞或未忽略告警(硬门禁;评估修复后重跑,或 -LooseAudit 临时降档)" -ForegroundColor Red
    return 'fail'
}

Write-Host "`n===== cargo audit(硬门禁,双 Cargo.lock)=====" -ForegroundColor Cyan
# cargo-audit 可能未加入 PATH(-AuditOnly 时不会走上面的 Import-VcVars 分支),按需补 ~\.cargo\bin
if (-not (Get-Command cargo -ErrorAction SilentlyContinue) -and (Test-Path (Join-Path $cargoBin 'cargo.exe'))) { $env:PATH = "$cargoBin;$env:PATH" }
if (Get-Command cargo-audit -ErrorAction SilentlyContinue) {
    $auditFail = $false
    $auditLocks = @(
        @{ Label = 'server-rs'; Lock = 'server-rs/Cargo.lock' },
        @{ Label = 'src-tauri'; Lock = 'src-tauri/Cargo.lock' }
    )
    # 从仓库根运行:cargo-audit 由当前目录向上查找 .cargo/audit.toml,确保两把锁共用同一 ignore 配置。
    Push-Location $root
    try {
        foreach ($lk in $auditLocks) {
            if (-not (Test-Path (Join-Path $root $lk.Lock))) {
                Write-Host "[WARN] 未找到 $($lk.Lock),跳过该锁" -ForegroundColor Yellow
                continue
            }
            if ((Invoke-CargoAuditLock $lk.Label $lk.Lock $LooseAudit.IsPresent) -eq 'fail') { $auditFail = $true }
        }
    } finally { Pop-Location }
    if ($auditFail) {
        Write-Host "`n===== 汇总: cargo audit 失败,后续阶段跳过 =====" -ForegroundColor Red
        exit 1
    }
} else {
    Write-Host '未安装 cargo-audit,跳过(安装:cargo install cargo-audit --locked)' -ForegroundColor Yellow
}

# npm 生产依赖审计:批次 1 接入(警告档),批次 6.1 复核 --omit=dev 为 0 告警后**提升为硬门禁**。
# 离线(拉不到 registry)时降 WARN,避免断网误拦;真漏洞输出无「离线」特征,仍按 FAIL 处理。
Write-Host "`n===== npm audit(生产依赖,硬门禁)=====" -ForegroundColor Cyan
$npmAuditOut = (npm audit --omit=dev 2>&1 | Out-String)
if ($LASTEXITCODE -eq 0) {
    Write-Host '[ OK ] npm audit 无生产依赖漏洞' -ForegroundColor Green
} elseif ($npmAuditOut -match 'found\s+(\d+)\s+vulnerabilit') {
    Write-Host ($npmAuditOut.Trim() -split "`n" | Select-Object -Last 8 | Out-String).Trim() -ForegroundColor Yellow
    Write-Host '[FAIL] npm audit 报告生产依赖漏洞(硬门禁;评估修复,勿裸升依赖)' -ForegroundColor Red
    exit 1
} elseif ($npmAuditOut -match 'ENOTFOUND|ECONNREFUSED|ETIMEDOUT|EAI_AGAIN|network|registry|audit endpoint|request to') {
    Write-Host '[WARN] npm audit 无法访问 registry(离线?),跳过硬门禁' -ForegroundColor Yellow
} else {
    Write-Host ($npmAuditOut.Trim() -split "`n" | Select-Object -Last 8 | Out-String).Trim() -ForegroundColor Yellow
    Write-Host '[FAIL] npm audit 异常退出(非漏洞输出,也非离线;请人工核查)' -ForegroundColor Red
    exit 1
}

# ===== 文档一致性门禁(纯 Node 零依赖,与 -SkipWeb 解耦)=====
# 2026-09-16 文档整合:68 份散落 md → 六类体系(契约/功能/计划/展望/经验/遗留),
# 原文档归档到 docs/archive/2026-09-16-consolidation/。此前 docs/ 没有任何机器校验,
# 本阶段把「根下只放六类活文档 + 索引双向一致 + 链接不失效 + 归档映射覆盖」变成硬门禁。
# 规则清单见 tools/check-docs.mjs 头部注释(D1~D6)。
if (-not $AuditOnly) {
    Push-Location $root
    try {
        Invoke-Stage 'docs: check-docs' { node tools/check-docs.mjs }
    } finally { Pop-Location }
}

if (-not $SkipWeb -and -not $AuditOnly) {
    Push-Location $root
    try {
        # 双 Cargo.lock 漂移:src-tauri 以 path 内嵌 server-rs,构建便携版时 cargo 忽略
        # server-rs/Cargo.lock 并重新解析依赖树 —— 同一后端源码在两版产物中可能编译出
        # 不同版本依赖(静默,跨 lock 不报 links 冲突)。2026-09-13 批次 1 起 0 漂移为基线,
        # 新增漂移即 FAIL;不可避免的历史例外登记在 tools/lock-sync-baseline.json。
        Invoke-Stage 'deps: check-lock-sync'    { node tools/check-lock-sync.mjs }
        # 契约快照:手写 TS 类型与 Rust 后端字段集合比对(漂移即 FAIL,纯 Node 零依赖)
        Invoke-Stage 'contract: check-contract' { node tools/check-contract.mjs }
        # 架构护栏:前端 store 循环依赖(A) + 组件直改 state(B) + 后端分层规则 C/D/E/G
        # + EJS 能力面冻结(H) + 代际归属 I + 跨代依赖方向 J(2026-09-14 新增)。
        # 全部规则均为硬门禁:违规即 exit 1;C/D/E 已于 2026-09-13 清零,
        # G/H 为 ratchet(只降不升),I/J 以 tools/arch-layers.json 为 SSOT。
        Invoke-Stage 'arch: check-arch'         { node tools/check-arch.mjs }
        # 测试数自动统计:与 MAINTENANCE.md 记录比对。
        # 2026-09-14 起为**硬门禁**(此前为警告档):MAINTENANCE.md 是「数字唯一真值源」,
        # 历史教训是同一数字在 5 份文档并存(923/697、975/733、977/745、971/763、1017/768),
        # 根因就是多处手抄且无人守护。新增测试后请跑 `npm run count:tests` 并同步该文档。
        Invoke-Stage 'count: tests' { node tools/count-tests.mjs --check }
        # 前端类型逃逸 ratchet:as never / as unknown as / 非空断言 / any 只降不升
        # (纯 Node 零依赖,与 check-arch/check-contract 同风格;基线见脚本内 BASELINE)
        Invoke-Stage 'web: type-ratchet'        { node tools/check-frontend-lint.mjs }
        # 前端代码质量(ESLint flat config + eslint-plugin-vue):0 error 硬门禁。
        # 与上面 type-ratchet 目的不同(ratchet 管类型逃逸、此管代码质量),两者并存。
        # 规则集首轮克制:存量问题降级为 warn(见 web/eslint.config.js),故 0 error 可过;
        # prettier --check 未接入——现有代码为手写紧凑风格,全量格式化差异面 ~80%,
        # 待独立「全量格式化」专项落地后再接(见 docs/功能-变更史.md)。
        Invoke-Stage 'web: eslint'              { npm run lint -w web }
        # 2026-09-08 附录 D 168 个存量错误已清偿归零,typecheck 恢复硬门禁;
        # -StrictTypecheck 参数保留兼容(已无分支差异)
        Invoke-Stage 'web: vue-tsc --noEmit'    { npm run typecheck -w web }
        Invoke-Stage 'web: vitest run'          { npm test -w web }
        if (-not $Quick) {
            Invoke-Stage 'web: vite build'      { npm run build -w web }
        }
    } finally { Pop-Location }
}

# ===== 性能门禁(可选,默认关闭;见 docs/功能-变更史.md 与 tools/perf-baseline.json)=====
# 需要**已在运行**的 Kedai 服务(脚本自动从 /api/bootstrap 取 token),故默认不跑,
# 避免把「没起服务」误报成门禁失败。用 -Perf 显式开启:
#   powershell -File tools/check-all.ps1 -Perf
# 服务不可用时脚本自身 exit(1),门禁显式 FAIL(fail-closed,不做自动探活降级——
# 与审计段「离线才降 WARN」的例外语义区分开:性能门禁是本地可复现的,不该静默跳过)。
if ($Perf) {
    Invoke-Stage 'perf: p95 gate' {
        node tools/perf-baseline.mjs --base $PerfBase --max-p95-factor $PerfFactor
    }
}

Write-Host "`n===== 全部通过 =====" -ForegroundColor Green
$results | Format-Table -AutoSize
exit 0
