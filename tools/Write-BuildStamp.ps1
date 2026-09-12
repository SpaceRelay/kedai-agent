# 构建指纹 sidecar 工具:供 build.ps1 与 tools\build-portable.ps1 dot-source 共用。
# 两端产物(kedai-server.exe / Kedai.exe)各自的 sidecar 由同一函数写出,
# 保证 dist_hash 算法一致、可直接比对;启动脚本(start.ps1 / launcher)据此
# 判断两版是否同步,见 MAINTENANCE.md「发布纪律」。

# 对 web\dist 全部文件(相对路径 + 单文件 SHA256)再做一次聚合 SHA256。
# 排序按小写完整路径,保证同一份 dist 在任何机器上算出同一个指纹。
function Get-KedaiDistHash {
    param([Parameter(Mandatory = $true)][string]$Root)

    $dist = Join-Path $Root "web\dist"
    $files = Get-ChildItem $dist -Recurse -File -ErrorAction SilentlyContinue |
        Sort-Object { $_.FullName.ToLowerInvariant() }
    if (-not $files) { throw "web\dist 为空或不存在,无法计算构建指纹" }

    $ms = New-Object System.IO.MemoryStream
    try {
        $writer = New-Object System.IO.StreamWriter($ms)
        foreach ($f in $files) {
            $rel = $f.FullName.Substring($dist.Length).Replace('\', '/')
            $h = (Get-FileHash -Algorithm SHA256 -Path $f.FullName).Hash
            $writer.Write("$rel`:$h`n")
        }
        $writer.Flush()
        $ms.Position = 0
        return (Get-FileHash -Algorithm SHA256 -InputStream $ms).Hash.ToLowerInvariant()
    } finally {
        $ms.Dispose()
    }
}

# 在 exe 旁边写 <exe>.build.json:{version, build_time, dist_hash}(扁平紧凑 JSON,
# launcher 用纯字符串提取解析,不要改成缩进格式)。返回 sidecar 完整路径。
function Write-KedaiBuildStamp {
    param(
        [Parameter(Mandatory = $true)][string]$Root,
        [Parameter(Mandatory = $true)][string]$ExePath
    )

    $version = (Get-Content (Join-Path $Root "package.json") -Raw -Encoding UTF8 | ConvertFrom-Json).version
    $stamp = [ordered]@{
        version    = $version
        build_time = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
        dist_hash  = Get-KedaiDistHash -Root $Root
    }
    $sidecar = "$ExePath.build.json"
    # PS 5.1 的 UTF8 带 BOM;ConvertFrom-Json 与 launcher 的子串提取均不受影响
    $stamp | ConvertTo-Json -Compress | Set-Content -Path $sidecar -Encoding UTF8
    return $sidecar
}

# ---------------------------------------------------------------- 悬空 jniLibs 链接清理
# 背景:tauri android build 会在 gen\android\app\src\main\jniLibs\<abi>\ 下创建指向
# src-tauri\target\<triple>\release\libkedai_desktop_lib.so 的符号链接(避免复制 30MB
# 大文件);而构建收尾会删除 src-tauri\target 回收磁盘,链接随之悬空。此后用资源管理器
# 或 7-Zip 压缩 / 备份整个仓库时,工具跟随不到链接目标,会报「系统找不到指定的路径」
# 并中断,压缩产物不完整。
# 故删除 target 后同步清掉悬空链接:它们本就是派生文件(见 gen/android/app/.gitignore
# 的 /src/main/jniLibs/**/*.so),下次 Android 构建会自动重建,删除不影响任何产物。
# 判据说明:悬空链接上 Test-Path 仍返回 True(PS 5.1 不穿透判定),必须校验「链接目标」
# 是否存在;链接类型为 SymbolicLink 才处理,真实文件一律不动。
function Clear-KedaiDanglingJniLibs {
    param([Parameter(Mandatory = $true)][string]$Root)

    $libRoot = Join-Path $Root "src-tauri\gen\android\app\src\main\jniLibs"
    if (-not (Test-Path $libRoot)) { return 0 }

    $removed = 0
    foreach ($f in (Get-ChildItem $libRoot -Recurse -Force -File -ErrorAction SilentlyContinue)) {
        if ($f.LinkType -ne "SymbolicLink") { continue }
        $alive = $false
        foreach ($t in @($f.Target)) {
            if (-not [string]::IsNullOrEmpty($t) -and (Test-Path -LiteralPath $t)) {
                $alive = $true
                break
            }
        }
        if ($alive) { continue }
        try {
            Remove-Item -LiteralPath $f.FullName -Force -ErrorAction Stop
            $removed++
            Write-Host "[清理] 移除悬空链接 $($f.FullName)" -ForegroundColor Yellow
        } catch {
            Write-Host "[警告] 移除悬空链接失败: $($f.FullName) - $($_.Exception.Message)" -ForegroundColor Yellow
        }
    }
    if ($removed -gt 0) {
        Write-Host "[清理] 共移除 $removed 个悬空 jniLibs 链接(防止压缩/备份报「找不到指定的路径」)" -ForegroundColor Yellow
    }
    return $removed
}

# 读取 sidecar 的 dist_hash;文件缺失或字段缺失返回 $null(调用方按「无法判定」处理,
# 绝不允许因为 sidecar 缺失而误报同步)。
function Read-KedaiDistHash {
    param([Parameter(Mandatory = $true)][string]$ExePath)

    $sidecar = "$ExePath.build.json"
    if (-not (Test-Path $sidecar)) { return $null }
    try {
        $json = Get-Content $sidecar -Raw -Encoding UTF8 | ConvertFrom-Json
    } catch {
        return $null
    }
    if ([string]::IsNullOrEmpty($json.dist_hash)) { return $null }
    return [string]$json.dist_hash
}
