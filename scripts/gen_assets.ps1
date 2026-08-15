# Nexora brand asset generator (docs/logo.svg, docs/wordmark.svg, docs/nexora.ico)
# + Windows-relevant Tauri app icons, made from the approved PNG artwork.
# Uses .NET System.Drawing only (no external tools / no new dependencies).
$ErrorActionPreference = "Stop"
Add-Type -AssemblyName System.Drawing

function Get-ContentBounds($bmp, [ref]$bx, [ref]$by, [ref]$bw, [ref]$bh) {
  $w = $bmp.Width; $h = $bmp.Height
  $rect = New-Object System.Drawing.Rectangle(0, 0, $w, $h)
  $data = $bmp.LockBits($rect, [System.Drawing.Imaging.ImageLockMode]::ReadOnly, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $stride = $data.Stride
  $bytes = New-Object byte[] ($stride * $h)
  [System.Runtime.InteropServices.Marshal]::Copy($data.Scan0, $bytes, 0, $bytes.Length)
  $bmp.UnlockBits($data)
  $minX = $w; $minY = $h; $maxX = -1; $maxY = -1
  for ($y = 0; $y -lt $h; $y++) {
    for ($x = 0; $x -lt $w; $x++) {
      $i = $y * $stride + $x * 4
      if ($bytes[$i + 3] -gt 8) {
        if ($x -lt $minX) { $minX = $x }
        if ($x -gt $maxX) { $maxX = $x }
        if ($y -lt $minY) { $minY = $y }
        if ($y -gt $maxY) { $maxY = $y }
      }
    }
  }
  $bx.Value = $minX; $by.Value = $minY
  $bw.Value = $maxX - $minX + 1; $bh.Value = $maxY - $minY + 1
}

function Get-PngBytes($bmp) {
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $bytes = $ms.ToArray()
  $ms.Dispose()
  return ,$bytes
}

function Resize-ToMax($bmp, $maxSide) {
  $w = $bmp.Width; $h = $bmp.Height
  $scale = [Math]::Min(1.0, $maxSide / [Math]::Max($w, $h))
  $nw = [Math]::Max(1, [int][Math]::Round($w * $scale))
  $nh = [Math]::Max(1, [int][Math]::Round($h * $scale))
  $nb = New-Object System.Drawing.Bitmap($nw, $nh, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($nb)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
  $g.Clear([System.Drawing.Color]::Transparent)
  $g.DrawImage($bmp, (New-Object System.Drawing.Rectangle(0, 0, $nw, $nh)), 0, 0, $w, $h, [System.Drawing.GraphicsUnit]::Pixel)
  $g.Dispose()
  return $nb
}

function New-BrandSvg($bmp, $path) {
  $bytes = Get-PngBytes $bmp
  $b64 = [Convert]::ToBase64String($bytes)
  $svg = '<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 {0} {1}" width="{0}" height="{1}"><image width="{0}" height="{1}" href="data:image/png;base64,{2}" /></svg>' -f $bmp.Width, $bmp.Height, $b64
  [System.IO.File]::WriteAllText($path, $svg, (New-Object System.Text.UTF8Encoding($false)))
  Write-Output ("wrote {0} ({1} x {2}, {3} bytes)" -f $path, $bmp.Width, $bmp.Height, (Get-Item $path).Length)
}

function New-Ico($contentBmp, $path, $sizes) {
  $side = 1024
  $canvas = New-Object System.Drawing.Bitmap($side, $side, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($canvas)
  $g.Clear([System.Drawing.Color]::Transparent)
  $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
  $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
  $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
  $g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
  $cx = [int][Math]::Floor(($side - $contentBmp.Width) / 2)
  $cy = [int][Math]::Floor(($side - $contentBmp.Height) / 2)
  $g.DrawImage($contentBmp, $cx, $cy, $contentBmp.Width, $contentBmp.Height)
  $g.Dispose()

  $images = @()
  foreach ($s in $sizes) {
    $sized = Resize-ToMax $canvas $s
    $images += ,@($s, (Get-PngBytes $sized))
    $sized.Dispose()
  }
  $ms = New-Object System.IO.MemoryStream
  $bw2 = New-Object System.IO.BinaryWriter($ms)
  $bw2.Write([UInt16]0); $bw2.Write([UInt16]1); $bw2.Write([UInt16]$images.Count)
  $offset = 6 + 16 * $images.Count
  foreach ($im in $images) {
    $wbyte = if ($im[0] -ge 256) { [Byte]0 } else { [Byte]$im[0] }
    $hbyte = if ($im[0] -ge 256) { [Byte]0 } else { [Byte]$im[0] }
    $bw2.Write($wbyte); $bw2.Write($hbyte)
    $bw2.Write([Byte]0); $bw2.Write([Byte]0)
    $bw2.Write([UInt16]1); $bw2.Write([UInt16]32)
    $bw2.Write([UInt32]$im[1].Length); $bw2.Write([UInt32]$offset)
    $offset += $im[1].Length
  }
  foreach ($im in $images) { $bw2.Write($im[1]) }
  $bw2.Flush()
  [System.IO.File]::WriteAllBytes($path, $ms.ToArray())
  $bw2.Dispose(); $ms.Dispose(); $canvas.Dispose()
  Write-Output ("wrote {0} ({1} bytes)" -f $path, (Get-Item $path).Length)
}

# ---- Load approved PNGs ----
$logoImg = [System.Drawing.Image]::FromFile("docs/logo.png")
$logo = New-Object System.Drawing.Bitmap($logoImg)
$wmImg = [System.Drawing.Image]::FromFile("docs/wordmark.png")
$wm = New-Object System.Drawing.Bitmap($wmImg)

# ---- Content bounds (crop transparent padding) ----
$bx = 0; $by = 0; $bw = 0; $bh = 0
Get-ContentBounds $logo ([ref]$bx) ([ref]$by) ([ref]$bw) ([ref]$bh)
Write-Output ("logo content bbox: {0},{1} {2}x{3}" -f $bx, $by, $bw, $bh)
$logoContent = $logo.Clone((New-Object System.Drawing.Rectangle($bx, $by, $bw, $bh)), $logo.PixelFormat)

$wx = 0; $wy = 0; $ww = 0; $wh = 0
Get-ContentBounds $wm ([ref]$wx) ([ref]$wy) ([ref]$ww) ([ref]$wh)
Write-Output ("wordmark content bbox: {0},{1} {2}x{3}" -f $wx, $wy, $ww, $wh)
$wmContent = $wm.Clone((New-Object System.Drawing.Rectangle($wx, $wy, $ww, $wh)), $wm.PixelFormat)

# ---- SVGs (faithful embedded raster, artwork bounds) ----
$logoSvg = Resize-ToMax $logoContent 512
New-BrandSvg $logoSvg "docs/logo.svg"
$wmSvg = Resize-ToMax $wmContent 640
New-BrandSvg $wmSvg "docs/wordmark.svg"

# ---- Application icon (logo mark only, no distortion) ----
$icoSizes = @(16, 24, 32, 48, 64, 128, 256)
New-Ico $logoContent "docs/nexora.ico" $icoSizes

# ---- Windows-relevant Tauri app icons (same mark, square) ----
$iconDir = "src-tauri/icons"
$sq = New-Object System.Drawing.Bitmap(1024, 1024, [System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($sq)
$g.Clear([System.Drawing.Color]::Transparent)
$g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::HighQuality
$g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
$g.CompositingQuality = [System.Drawing.Drawing2D.CompositingQuality]::HighQuality
$g.DrawImage($logoContent, ([int][Math]::Floor((1024 - $logoContent.Width) / 2)), ([int][Math]::Floor((1024 - $logoContent.Height) / 2)), $logoContent.Width, $logoContent.Height)
$g.Dispose()
$t = Resize-ToMax $sq 32;  $t.Save("$iconDir/32x32.png", [System.Drawing.Imaging.ImageFormat]::Png); $t.Dispose()
$t = Resize-ToMax $sq 64;  $t.Save("$iconDir/64x64.png", [System.Drawing.Imaging.ImageFormat]::Png); $t.Dispose()
$t = Resize-ToMax $sq 128; $t.Save("$iconDir/128x128.png", [System.Drawing.Imaging.ImageFormat]::Png); $t.Dispose()
$t = Resize-ToMax $sq 256; $t.Save("$iconDir/128x128@2x.png", [System.Drawing.Imaging.ImageFormat]::Png); $t.Dispose()
$t = Resize-ToMax $sq 512; $t.Save("$iconDir/icon.png", [System.Drawing.Imaging.ImageFormat]::Png); $t.Dispose()
$sq.Dispose()
New-Ico $logoContent "$iconDir/icon.ico" $icoSizes

$logoSvg.Dispose(); $wmSvg.Dispose()
$logoContent.Dispose(); $wmContent.Dispose()
$logo.Dispose(); $wm.Dispose()
$logoImg.Dispose(); $wmImg.Dispose()
Write-Output "DONE"