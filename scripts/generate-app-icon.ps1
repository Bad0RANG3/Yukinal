# Generates the Yukinal application mark as a 1024x1024 PNG.
#
# Reproducible on purpose: `tauri icon` needs a source image, and a committed
# PowerShell script beats an opaque binary that nobody can regenerate.
# Usage:  powershell -NoProfile -ExecutionPolicy Bypass -File scripts/generate-app-icon.ps1
# Then:   pnpm --filter @yukinal/desktop icon

param(
    [int]$Size = 1024,
    [string]$Out = "apps/desktop/design/app-icon.png"
)

Add-Type -AssemblyName System.Drawing

function New-RoundedRectPath {
    param([single]$X, [single]$Y, [single]$W, [single]$H, [single]$Radius)
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = $Radius * 2
    $path.AddArc($X, $Y, $d, $d, 180, 90)
    $path.AddArc($X + $W - $d, $Y, $d, $d, 270, 90)
    $path.AddArc($X + $W - $d, $Y + $H - $d, $d, $d, 0, 90)
    $path.AddArc($X, $Y + $H - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

$bitmap = New-Object System.Drawing.Bitmap $Size, $Size
$graphics = [System.Drawing.Graphics]::FromImage($bitmap)
$graphics.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$graphics.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
$graphics.Clear([System.Drawing.Color]::Transparent)

# --- plate: dark, so the mark survives both light and dark taskbars (legibility)
$plateRect = New-RoundedRectPath ($Size * 0.06) ($Size * 0.06) ($Size * 0.88) ($Size * 0.88) ($Size * 0.20)
$plateBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 9, 9, 11))
$graphics.FillPath($plateBrush, $plateRect)
$plateBrush.Dispose()
$plateRect.Dispose()

$centerX = $Size / 2.0
$centerY = $Size / 2.0

# --- ring: an ellipse drawn under a rotation transform, i.e. an inclined plane
$savedState = $graphics.Save()
$graphics.TranslateTransform([single]$centerX, [single]$centerY)
$graphics.RotateTransform(-28)
$ringWidth = $Size * 0.74
$ringHeight = $Size * 0.40
$ringPen = New-Object System.Drawing.Pen([System.Drawing.Color]::FromArgb(255, 56, 189, 248), ($Size * 0.045))
$graphics.DrawEllipse($ringPen, [single](-$ringWidth / 2.0), [single](-$ringHeight / 2.0), [single]$ringWidth, [single]$ringHeight)
$ringPen.Dispose()

# --- marker: the node travelling along the ring
$dotDiameter = $Size * 0.10
$dotBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 244, 244, 245))
$graphics.FillEllipse($dotBrush, [single]($ringWidth / 2.0 - $dotDiameter / 2.0), [single](-$dotDiameter / 2.0), [single]$dotDiameter, [single]$dotDiameter)
$dotBrush.Dispose()
$graphics.Restore($savedState)

# --- core server node
$coreDiameter = $Size * 0.26
$coreBrush = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 250, 250, 250))
$graphics.FillEllipse($coreBrush, [single]($centerX - $coreDiameter / 2.0), [single]($centerY - $coreDiameter / 2.0), [single]$coreDiameter, [single]$coreDiameter)
$coreBrush.Dispose()

$graphics.Dispose()

$directory = Split-Path -Parent $Out
if ($directory -and -not (Test-Path $directory)) {
    New-Item -ItemType Directory -Path $directory -Force | Out-Null
}
$bitmap.Save((Resolve-Path -Path "." ).Path + "\" + $Out.Replace('/', '\'), [System.Drawing.Imaging.ImageFormat]::Png)
$bitmap.Dispose()

Write-Host "wrote $Out ($Size x $Size)"
