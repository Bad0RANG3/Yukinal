param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")),
    [string]$Output = "docs\assets\screenshots\yukinal-workspace.png",
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"
$binaryPath = Join-Path $RepoRoot "target\debug\yukinal-desktop.exe"
if (-not (Test-Path -LiteralPath $binaryPath)) {
    throw "Debug binary not found: $binaryPath. Run `cargo build -p yukinal-desktop` first."
}

$outputPath = Join-Path $RepoRoot $Output
$outputDirectory = Split-Path -Parent $outputPath
New-Item -ItemType Directory -Force -Path $outputDirectory | Out-Null

$dataDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("yukinal-readme-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Force -Path $dataDirectory | Out-Null
$previousDataDirectory = $env:YUKINAL_DATA_DIR
$env:YUKINAL_DATA_DIR = $dataDirectory
$process = $null

Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;

public static class YukinalWindowCapture
{
    [StructLayout(LayoutKind.Sequential)]
    public struct RECT
    {
        public int Left;
        public int Top;
        public int Right;
        public int Bottom;
    }

    [DllImport("user32.dll")]
    public static extern bool GetWindowRect(IntPtr hWnd, out RECT rect);

    [DllImport("user32.dll")]
    public static extern bool PrintWindow(IntPtr hWnd, IntPtr hdcBlt, uint flags);

    [DllImport("user32.dll")]
    public static extern bool SetForegroundWindow(IntPtr hWnd);

    [DllImport("user32.dll")]
    public static extern bool ShowWindow(IntPtr hWnd, int command);
}
"@

try {
    $process = Start-Process -FilePath $binaryPath -WorkingDirectory $RepoRoot -PassThru
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)
    do {
        Start-Sleep -Milliseconds 500
        $process.Refresh()
        if ($process.HasExited) {
            throw "Desktop process exited before creating a window (exit code $($process.ExitCode))."
        }
    } while ($process.MainWindowHandle -eq 0 -and (Get-Date) -lt $deadline)

    if ($process.MainWindowHandle -eq 0) {
        throw "Desktop process stayed alive but created no top-level window."
    }

    [YukinalWindowCapture]::ShowWindow($process.MainWindowHandle, 9) | Out-Null
    [YukinalWindowCapture]::SetForegroundWindow($process.MainWindowHandle) | Out-Null
    Start-Sleep -Seconds 3

    $rect = New-Object YukinalWindowCapture+RECT
    if (-not [YukinalWindowCapture]::GetWindowRect($process.MainWindowHandle, [ref]$rect)) {
        throw "Could not read the desktop window bounds."
    }
    $width = $rect.Right - $rect.Left
    $height = $rect.Bottom - $rect.Top
    if ($width -le 0 -or $height -le 0) {
        throw "Desktop window has invalid bounds: ${width}x${height}."
    }

    $bitmap = New-Object System.Drawing.Bitmap($width, $height)
    $graphics = [System.Drawing.Graphics]::FromImage($bitmap)
    $hdc = $graphics.GetHdc()
    try {
        if (-not [YukinalWindowCapture]::PrintWindow($process.MainWindowHandle, $hdc, 2)) {
            throw "PrintWindow failed."
        }
    }
    finally {
        $graphics.ReleaseHdc($hdc)
        $graphics.Dispose()
    }

    $bitmap.Save($outputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    Write-Output "PASS: captured ${width}x${height} to $outputPath"
}
finally {
    if ($null -eq $previousDataDirectory) {
        Remove-Item Env:YUKINAL_DATA_DIR -ErrorAction SilentlyContinue
    }
    else {
        $env:YUKINAL_DATA_DIR = $previousDataDirectory
    }

    if ($null -ne $process) {
        $process.Refresh()
        if (-not $process.HasExited) {
            $process.CloseMainWindow() | Out-Null
            if (-not $process.WaitForExit(3000)) {
                Stop-Process -Id $process.Id -Force
            }
        }
    }

    if (Test-Path -LiteralPath $dataDirectory) {
        Remove-Item -LiteralPath $dataDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}
