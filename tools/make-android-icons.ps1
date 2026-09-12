# Kedai Android launcher icon generator (no external deps, uses .NET System.Drawing)
#
# 解决问题:原图标由 `tauri icon` 从一张本身偏下、非居中的源图直接生成,前景内容
# 占满 108dp 画布 65%~81%,越出 adaptive icon 的 66dp 安全区且整体偏左偏下
# (xxxhdpi 下边距仅剩 3px),被各厂商遮罩裁切后看起来「位置不对」;
# 且 mipmap-hdpi 的 legacy 图被错生成成 49x49(应为 72x72),round 形态还缺 XML。
#
# 本脚本做法:
#   1) 把源图按 alpha 包围盒裁剪为「内容居中的正方母版」,消除源图自带偏移;
#   2) 前景(foreground):按「内容任一点到画布中心的最大距离」自适应缩放,
#      保证落在 adaptive icon 的 66dp 中心安全圆内(默认 SafeRadiusRatio 0.60),
#      四周留透明。比按正方形边长缩放更稳:logo 顶角(如耳朵尖)不会越出圆形安全区;
#   3) 传统图标(ic_launcher / ic_launcher_round):按五档密度生成正确尺寸
#      (48/72/96/144/192),垫 Kedai 纸色圆角方形底 / 圆形底,logo 居中;
#   4) 额外输出两张遮罩预览图,便于人眼确认居中与否。
#
# 用法:
#   powershell -NoProfile -ExecutionPolicy Bypass -File tools\make-android-icons.ps1
#     -Source           源 logo PNG(默认 D:\kedai-android\artifacts\app-icon-1024.png,
#                       不存在则回退 src-tauri\icons\icon.png)
#     -OutRoot          项目根(默认本仓库根)
#     -SafeRadiusRatio  前景内容最大半径占画布半宽的比例(默认 0.60;
#                       adaptive icon 安全圆理论值 66/108≈0.611)
#     -PreviewDir       预览图输出目录(默认 D:\kedai-android\artifacts)

param(
    [string]$Source = "",
    [string]$OutRoot = "",
    [double]$SafeRadiusRatio = 0.60,
    [string]$PreviewDir = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

if (-not $OutRoot) { $OutRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path) }

if (-not $Source) {
    $candidates = @(
        "D:\kedai-android\artifacts\app-icon-1024.png",
        (Join-Path $OutRoot "src-tauri\icons\icon.png")
    )
    foreach ($c in $candidates) { if (Test-Path $c) { $Source = $c; break } }
}
if (-not $Source -or -not (Test-Path $Source)) { throw "找不到源 logo 图片。请用 -Source 指定。" }
if (-not $PreviewDir) { $PreviewDir = "D:\kedai-android\artifacts" }

$resDir = Join-Path $OutRoot "src-tauri\gen\android\app\src\main\res"
if (-not (Test-Path $resDir)) { throw "找不到 Android res 目录: $resDir" }

# Kedai 纸色(与 values/colors.xml、values/ic_launcher_background.xml 同值)
$paper = [System.Drawing.Color]::FromArgb(255, 251, 239, 241)

Write-Host "源图: $Source"
Write-Host "前景安全半径占比: $SafeRadiusRatio (adaptive icon 理论安全区 66/108≈0.611)"

# ---------- 工具函数 ----------

# 读取 alpha > 阈值 的像素包围盒(返回 $null 表示全透明)
function Get-AlphaBBox($bmp, [int]$threshold = 8) {
    $w = $bmp.Width; $h = $bmp.Height
    $rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $bytes = New-Object byte[] ($stride * $h)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    $bmp.UnlockBits($data)

    $minX = $w; $minY = $h; $maxX = -1; $maxY = -1
    for ($y = 0; $y -lt $h; $y++) {
        $row = $y * $stride
        for ($x = 0; $x -lt $w; $x++) {
            $a = $bytes[$row + $x * 4 + 3]
            if ($a -gt $threshold) {
                if ($x -lt $minX) { $minX = $x }
                if ($x -gt $maxX) { $maxX = $x }
                if ($y -lt $minY) { $minY = $y }
                if ($y -gt $maxY) { $maxY = $y }
            }
        }
    }
    if ($maxX -lt 0) { return $null }
    return @{ MinX = $minX; MinY = $minY; MaxX = $maxX; MaxY = $maxY; W = ($maxX - $minX + 1); H = ($maxY - $minY + 1) }
}

# 内容中 alpha>阈值 的像素到画布中心的最大距离(母版像素单位)
function Get-MaxContentRadius($bmp, [int]$threshold = 8) {
    $w = $bmp.Width; $h = $bmp.Height
    $rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
    $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $stride = $data.Stride
    $bytes = New-Object byte[] ($stride * $h)
    [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
    $bmp.UnlockBits($data)

    $cx = ($w - 1) / 2.0; $cy = ($h - 1) / 2.0
    $maxR = 0.0
    for ($y = 0; $y -lt $h; $y++) {
        $row = $y * $stride
        for ($x = 0; $x -lt $w; $x++) {
            if ($bytes[$row + $x * 4 + 3] -gt $threshold) {
                $dx = $x - $cx; $dy = $y - $cy
                $r = [Math]::Sqrt($dx * $dx + $dy * $dy)
                if ($r -gt $maxR) { $maxR = $r }
            }
        }
    }
    return $maxR
}

function New-HiResGraphics($bmp) {
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
    $g.CompositingMode = [System.Drawing.Drawing2D.CompositingMode]::SourceOver
    return $g
}

# 内容居中的正方母版:裁剪到 alpha 包围盒,再以最长边为边长居中补透明
function New-SquareMaster($src) {
    $bbox = Get-AlphaBBox $src
    if ($null -eq $bbox) { throw "源图全透明,无法生成图标。" }
    Write-Host ("  源图内容包围盒: x[{0},{1}] y[{2},{3}] ({4}x{5})" -f $bbox.MinX, $bbox.MaxX, $bbox.MinY, $bbox.MaxY, $bbox.W, $bbox.H)

    $side = [Math]::Max($bbox.W, $bbox.H)
    $master = New-Object System.Drawing.Bitmap($side, $side, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = New-HiResGraphics $master
    $g.Clear([System.Drawing.Color]::Transparent)
    # 把内容包围盒放到正方画布中央
    $dx = [int](($side - $bbox.W) / 2)
    $dy = [int](($side - $bbox.H) / 2)
    $srcRect = New-Object System.Drawing.Rectangle($bbox.MinX, $bbox.MinY, $bbox.W, $bbox.H)
    $dstRect = New-Object System.Drawing.Rectangle($dx, $dy, $bbox.W, $bbox.H)
    $g.DrawImage($src, $dstRect, $srcRect, [System.Drawing.GraphicsUnit]::Pixel)
    $g.Dispose()
    return $master
}

# 圆角方形路径(半径按画布比例)
function New-RoundedRectPath([int]$size, [double]$radiusPct) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $r = [single]([Math]::Max(1, [Math]::Round($size * $radiusPct)))
    $d = $r * 2
    $max = [single]($size - 1)
    $path.AddArc(0, 0, $d, $d, 180, 90)
    $path.AddArc($max - $d, 0, $d, $d, 270, 90)
    $path.AddArc($max - $d, $max - $d, $d, $d, 0, 90)
    $path.AddArc(0, $max - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

# 画一个底(圆角方形 或 圆形),颜色给定
function New-Background([int]$size, [string]$shape, [System.Drawing.Color]$color, [double]$radiusPct = 0.2) {
    $bmp = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = New-HiResGraphics $bmp
    $g.Clear([System.Drawing.Color]::Transparent)
    $brush = New-Object System.Drawing.SolidBrush($color)
    if ($shape -eq "circle") {
        $g.FillEllipse($brush, 0, 0, $size - 1, $size - 1)
    } else {
        $path = New-RoundedRectPath $size $radiusPct
        $g.FillPath($brush, $path)
        $path.Dispose()
    }
    $brush.Dispose()
    $g.Dispose()
    return $bmp
}

# 在画布中央放置母版,使内容最远点到中心的距离 = size*radiusRatio/2(保证落在安全圆内)
function Add-CenteredLogo($canvas, $master, [double]$radiusRatio) {
    $size = $canvas.Width
    $masterMaxR = Get-MaxContentRadius $master
    if ($masterMaxR -le 0) { throw "母版内容为空,无法放置。" }
    $targetMaxR = $size * $radiusRatio / 2.0
    $scale = $targetMaxR / $masterMaxR
    $side = [int][Math]::Round($master.Width * $scale)
    $dx = [int](($size - $side) / 2)
    $dy = [int](($size - $side) / 2)
    $g = New-HiResGraphics $canvas
    $g.DrawImage($master, (New-Object System.Drawing.Rectangle($dx, $dy, $side, $side)))
    $g.Dispose()
}

function Save-Png($bmp, [string]$path) {
    $dir = Split-Path -Parent $path
    New-Item -ItemType Directory -Force -Path $dir | Out-Null
    $bmp.Save($path, [System.Drawing.Imaging.ImageFormat]::Png)
}

# 把合成图按遮罩形状裁掉外部(模拟 launcher 遮罩后的样子,供预览)
function New-MaskedPreview($canvas, [string]$shape, [double]$radiusPct = 0.25) {
    $size = $canvas.Width
    $masked = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = New-HiResGraphics $masked
    $g.Clear([System.Drawing.Color]::Transparent)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    if ($shape -eq "circle") {
        $path.AddEllipse(0, 0, $size - 1, $size - 1)
    } else {
        $path.Dispose()
        $path = New-RoundedRectPath $size $radiusPct
    }
    $g.SetClip($path)
    $g.DrawImage($canvas, 0, 0)
    $g.ResetClip()
    $path.Dispose()
    $g.Dispose()
    return $masked
}

# ---------- 主流程 ----------

$srcBmp = New-Object System.Drawing.Bitmap($Source)
$master = New-SquareMaster $srcBmp
$srcBmp.Dispose()
Write-Host ("  居中正方母版: {0}x{0}" -f $master.Width)

# 五档密度:名称 / 传统图标边长 / 前景边长(108dp 系)
$densities = @(
    @{ Name = "mdpi";    Legacy = 48;  Fg = 108 },
    @{ Name = "hdpi";    Legacy = 72;  Fg = 162 },
    @{ Name = "xhdpi";   Legacy = 96;  Fg = 216 },
    @{ Name = "xxhdpi";  Legacy = 144; Fg = 324 },
    @{ Name = "xxxhdpi"; Legacy = 192; Fg = 432 }
)

foreach ($d in $densities) {
    $dir = Join-Path $resDir ("mipmap-" + $d.Name)

    # 1) 自适应前景:透明底 + 居中内容
    $fg = New-Object System.Drawing.Bitmap($d.Fg, $d.Fg, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $gfg = New-HiResGraphics $fg
    $gfg.Clear([System.Drawing.Color]::Transparent)
    $gfg.Dispose()
    Add-CenteredLogo $fg $master $SafeRadiusRatio
    Save-Png $fg (Join-Path $dir "ic_launcher_foreground.png")

    # 2) 传统图标:纸色圆角方形底 + 居中 logo(圆角半径 20%,同桌面图标风格)
    #    安全比例略宽(0.72):方块底的四角本就是可见区域,不受圆安全区约束
    $legacy = New-Background $d.Legacy "rounded" $paper 0.20
    Add-CenteredLogo $legacy $master 0.72
    Save-Png $legacy (Join-Path $dir "ic_launcher.png")

    # 3) 传统圆形图标:纸色圆形底 + 居中 logo(收敛以适配圆形)
    $round = New-Background $d.Legacy "circle" $paper
    Add-CenteredLogo $round $master 0.60
    Save-Png $round (Join-Path $dir "ic_launcher_round.png")

    Write-Host ("  {0,-8} legacy {1}x{1}  fg {2}x{2}" -f $d.Name, $d.Legacy, $d.Fg)
    $fg.Dispose(); $legacy.Dispose(); $round.Dispose()
}

# ---------- 遮罩预览(模拟 launcher 裁切后的视觉) ----------
$pvSize = 512
$composed = New-Background $pvSize "rounded" $paper 0.0   # 先用方形纸底,再交给遮罩裁形
Add-CenteredLogo $composed $master $SafeRadiusRatio
$pvCircle = New-MaskedPreview $composed "circle"
Save-Png $pvCircle (Join-Path $PreviewDir "icon-preview-circle.png")
$pvCircle.Dispose()
$pvSquircle = New-MaskedPreview $composed "rounded" 0.25
Save-Png $pvSquircle (Join-Path $PreviewDir "icon-preview-squircle.png")
$pvSquircle.Dispose()
$composed.Dispose()

# 对照:未裁切的自适应前景(透明底)
$fgPreview = New-Object System.Drawing.Bitmap($pvSize, $pvSize, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$gfp = New-HiResGraphics $fgPreview
$gfp.Clear([System.Drawing.Color]::Transparent)
$gfp.Dispose()
Add-CenteredLogo $fgPreview $master $SafeRadiusRatio
Save-Png $fgPreview (Join-Path $PreviewDir "icon-preview-foreground.png")
$fgPreview.Dispose()

$master.Dispose()
Write-Host "预览图输出:"
Write-Host ("  {0}" -f (Join-Path $PreviewDir "icon-preview-circle.png"))
Write-Host ("  {0}" -f (Join-Path $PreviewDir "icon-preview-squircle.png"))
Write-Host ("  {0}" -f (Join-Path $PreviewDir "icon-preview-foreground.png"))
Write-Host "ANDROID ICONS DONE"
