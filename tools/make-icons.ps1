# Kedai brand icon generator v2 (no external deps, uses .NET System.Drawing)
# Steps: draw source (contain-fit) -> per-pixel: white-bg removal + rounded-corner mask -> resize -> emit .ico + .png
# Usage: powershell -NoProfile -ExecutionPolicy Bypass -File make-icons.ps1
#   -Source  input PNG (default: desktop logo photo)
#   -OutRoot project root (default: this repo root)

param(
    [string]$Source = "",
    [string]$OutRoot = ""
)

$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

if (-not $Source) {
    Write-Host "用法: powershell -NoProfile -ExecutionPolicy Bypass -File make-icons.ps1 -Source <logo.png>"
    Write-Host "  -Source  源 logo 图片(必填,PNG)"
    Write-Host "  -OutRoot 项目根目录(默认:本仓库根)"
    exit 1
}

if (-not $OutRoot) { $OutRoot = Split-Path -Parent (Split-Path -Parent $MyInvocation.MyCommand.Path) }

$srcDir = Join-Path $OutRoot "src-tauri\icons"
$pubDir = Join-Path $OutRoot "web\public"
New-Item -ItemType Directory -Force -Path $srcDir | Out-Null
New-Item -ItemType Directory -Force -Path $pubDir | Out-Null

if (-not (Test-Path $Source)) { throw "Source image not found: $Source" }

$srcBmp = New-Object System.Drawing.Bitmap($Source)
$SW = $srcBmp.Width
$SH = $srcBmp.Height
$size = [Math]::Max($SW, $SH)
$bgLow = 230    # alpha fully removed below this min-channel value
$bgHigh = 246   # alpha fully kept above this min-channel value
$aa = 2.0       # anti-aliasing falloff pixels

# ---- build master (size x size): contain-fit source + bg removal + rounded corners ----
$master = New-Object System.Drawing.Bitmap($size, $size, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($master)
$g.Clear([System.Drawing.Color]::Transparent)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
# contain-fit: scale preserving aspect ratio, centered
$scale = [Math]::Min(1.0 * $size / $SW, 1.0 * $size / $SH)
$dw = [int]($SW * $scale); $dh = [int]($SH * $scale)
$dx = [int](($size - $dw) / 2); $dy = [int](($size - $dh) / 2)
$g.DrawImage($srcBmp, (New-Object System.Drawing.Rectangle($dx, $dy, $dw, $dh)))
$g.Dispose()
$srcBmp.Dispose()

# per-pixel pass
$rect = New-Object System.Drawing.Rectangle(0, 0, $size, $size)
$bmpData = $master.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadWrite, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$stride = $bmpData.Stride
$bytes = New-Object byte[] ($stride * $size)
[System.Runtime.InteropServices.Marshal]::Copy($bmpData.Scan0, $bytes, 0, $bytes.Length)

$r = [double]($size * 0.20)   # corner radius
$r2 = $r * $r
$minX = $r + $aa; $maxX = $size - 1 - $r - $aa
$minY = $r + $aa; $maxY = $size - 1 - $r - $aa

for ($y = 0; $y -lt $size; $y++) {
    $row = $y * $stride
    for ($x = 0; $x -lt $size; $x++) {
        $i = $row + $x * 4
        $b = $bytes[$i]; $gv = $bytes[$i+1]; $rv = $bytes[$i+2]; $a = $bytes[$i+3]
        $minC = [Math]::Min($b, [Math]::Min($gv, $rv))

        # 1) white background removal
        $bgA = 255
        if ($minC -ge $bgHigh) { $bgA = 0 }
        elseif ($minC -gt $bgLow) { $bgA = [int](255 * ($minC - $bgLow) / ($bgHigh - $bgLow)) }

        # 2) rounded corner mask
        $cornerA = 255
        if ($x -le $minX -and $y -le $minY) {
            $dxc = $x - $r; $dyc = $y - $r; $d = [Math]::Sqrt($dxc*$dxc + $dyc*$dyc)
            if ($d -gt $r) { $cornerA = if ($d -ge $r + $aa) { 0 } else { [int](255 * ($r + $aa - $d) / $aa) } }
        } elseif ($x -ge $maxX -and $y -le $minY) {
            $dxc = $x - ($size - 1 - $r); $dyc = $y - $r; $d = [Math]::Sqrt($dxc*$dxc + $dyc*$dyc)
            if ($d -gt $r) { $cornerA = if ($d -ge $r + $aa) { 0 } else { [int](255 * ($r + $aa - $d) / $aa) } }
        } elseif ($x -le $minX -and $y -ge $maxY) {
            $dxc = $x - $r; $dyc = $y - ($size - 1 - $r); $d = [Math]::Sqrt($dxc*$dxc + $dyc*$dyc)
            if ($d -gt $r) { $cornerA = if ($d -ge $r + $aa) { 0 } else { [int](255 * ($r + $aa - $d) / $aa) } }
        } elseif ($x -ge $maxX -and $y -ge $maxY) {
            $dxc = $x - ($size - 1 - $r); $dyc = $y - ($size - 1 - $r); $d = [Math]::Sqrt($dxc*$dxc + $dyc*$dyc)
            if ($d -gt $r) { $cornerA = if ($d -ge $r + $aa) { 0 } else { [int](255 * ($r + $aa - $d) / $aa) } }
        }

        $final = [Math]::Min([Math]::Min($a, $bgA), $cornerA)
        if ($final -ne $a) { $bytes[$i+3] = [byte]$final }
    }
}
[System.Runtime.InteropServices.Marshal]::Copy($bytes, 0, $bmpData.Scan0, $bytes.Length)
$master.UnlockBits($bmpData)

# ---- high-quality resize ----
function Resize-Image($bmp, [int]$s) {
    $out = New-Object System.Drawing.Bitmap($s, $s, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g2 = [System.Drawing.Graphics]::FromImage($out)
    $g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g2.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
    $g2.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g2.Clear([System.Drawing.Color]::Transparent)
    $g2.DrawImage($bmp, (New-Object System.Drawing.Rectangle(0, 0, $s, $s)))
    $g2.Dispose()
    return $out
}

foreach ($s in @(512, 256, 128, 64, 48, 32, 16)) {
    $img = Resize-Image $master $s
    $img.Save((Join-Path $srcDir ("icon_{0}x{0}.png" -f $s)), [System.Drawing.Imaging.ImageFormat]::Png)
    $img.Dispose()
}
(Resize-Image $master 512).Save((Join-Path $srcDir "icon.png"), [System.Drawing.Imaging.ImageFormat]::Png)
(Resize-Image $master 256).Save((Join-Path $srcDir "128x128@2x.png"), [System.Drawing.Imaging.ImageFormat]::Png)
(Resize-Image $master 128).Save((Join-Path $srcDir "128x128.png"), [System.Drawing.Imaging.ImageFormat]::Png)
(Resize-Image $master 32).Save((Join-Path $srcDir "32x32.png"), [System.Drawing.Imaging.ImageFormat]::Png)
(Resize-Image $master 64).Save((Join-Path $pubDir "favicon.png"), [System.Drawing.Imaging.ImageFormat]::Png)
(Resize-Image $master 128).Save((Join-Path $pubDir "logo.png"), [System.Drawing.Imaging.ImageFormat]::Png)
Write-Host "PNGs done"

function New-IcoFile([string]$pngPath, [string]$icoPath) {
    $pngBytes = [System.IO.File]::ReadAllBytes($pngPath)
    $ms = New-Object System.IO.MemoryStream
    $bw = New-Object System.IO.BinaryWriter($ms)
    $bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]1)
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([byte]0); $bw.Write([byte]0)
    $bw.Write([uint16]1); $bw.Write([uint16]32)
    $bw.Write([uint32]$pngBytes.Length)
    $bw.Write([uint32]22)
    $bw.Write($pngBytes)
    $bw.Flush()
    [System.IO.File]::WriteAllBytes($icoPath, $ms.ToArray())
    $bw.Dispose(); $ms.Dispose()
}
New-IcoFile (Join-Path $srcDir "icon_256x256.png") (Join-Path $srcDir "icon.ico")
New-IcoFile (Join-Path $srcDir "icon_256x256.png") (Join-Path $pubDir "favicon.ico")
$master.Dispose()
Write-Host "ALL ICONS DONE"
