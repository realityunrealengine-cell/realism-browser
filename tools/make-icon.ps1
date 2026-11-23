# Generates assets/realism.ico (PNG-compressed multi-size icon) with System.Drawing.
param([string]$Out = "$PSScriptRoot\..\assets\realism.ico")
Add-Type -AssemblyName System.Drawing
$sizes = 16, 24, 32, 48, 64, 128, 256
$pngs = @()
foreach ($s in $sizes) {
  $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
  $g = [System.Drawing.Graphics]::FromImage($bmp)
  $g.SmoothingMode = 'AntiAlias'; $g.InterpolationMode = 'HighQualityBicubic'; $g.TextRenderingHint = 'AntiAliasGridFit'
  $g.Clear([System.Drawing.Color]::Transparent)
  $r = [single]($s * 0.24)
  $path = New-Object System.Drawing.Drawing2D.GraphicsPath
  $w = [single]($s - 1)
  $path.AddArc(0, 0, 2*$r, 2*$r, 180, 90); $path.AddArc($w - 2*$r, 0, 2*$r, 2*$r, 270, 90)
  $path.AddArc($w - 2*$r, $w - 2*$r, 2*$r, 2*$r, 0, 90); $path.AddArc(0, $w - 2*$r, 2*$r, 2*$r, 90, 90); $path.CloseFigure()
  $rect = New-Object System.Drawing.RectangleF 0, 0, $s, $s
  $brush = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(255,139,124,255)), ([System.Drawing.Color]::FromArgb(255,61,217,197)), 45
  $g.FillPath($brush, $path)
  # inner dark disc
  $m = [single]($s * 0.17)
  $dark = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(235, 18, 20, 26))
  $g.FillEllipse($dark, $m, $m, $s - 2*$m, $s - 2*$m)
  # stylised R
  $fs = [single]($s * 0.50)
  $font = New-Object System.Drawing.Font 'Segoe UI', $fs, ([System.Drawing.FontStyle]::Bold), ([System.Drawing.GraphicsUnit]::Pixel)
  $sf = New-Object System.Drawing.StringFormat; $sf.Alignment = 'Center'; $sf.LineAlignment = 'Center'
  $tb = New-Object System.Drawing.Drawing2D.LinearGradientBrush $rect, ([System.Drawing.Color]::FromArgb(255,190,180,255)), ([System.Drawing.Color]::FromArgb(255,120,240,225)), 45
  $g.DrawString('R', $font, $tb, (New-Object System.Drawing.RectangleF 0, ([single]($s*0.02)), $s, $s), $sf)
  $g.Dispose()
  $ms = New-Object System.IO.MemoryStream
  $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
  $pngs += ,($ms.ToArray())
  $bmp.Dispose()
}
$fsOut = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter $fsOut
$bw.Write([uint16]0); $bw.Write([uint16]1); $bw.Write([uint16]$sizes.Count)
$offset = 6 + 16 * $sizes.Count
for ($i = 0; $i -lt $sizes.Count; $i++) {
  $s = $sizes[$i]; $d = if ($s -ge 256) { 0 } else { $s }
  $bw.Write([byte]$d); $bw.Write([byte]$d); $bw.Write([byte]0); $bw.Write([byte]0)
  $bw.Write([uint16]1); $bw.Write([uint16]32); $bw.Write([uint32]$pngs[$i].Length); $bw.Write([uint32]$offset)
  $offset += $pngs[$i].Length
}
foreach ($p in $pngs) { $bw.Write($p) }
$bw.Close()
# Also export a 256px PNG for docs/installer
Write-Output "icon written: $Out"
