param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")),
    [string]$Output = "docs\assets\screenshots\yukinal-workspace.png",
    [ValidateSet("workspace", "terminal-empty", "terminal-demo")]
    [string]$View = "workspace",
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

    [DllImport("user32.dll")]
    public static extern bool SetCursorPos(int x, int y);

    [DllImport("user32.dll")]
    public static extern void mouse_event(uint flags, uint dx, uint dy, uint data, UIntPtr extraInfo);

    public static void Click(int x, int y)
    {
        SetCursorPos(x, y);
        mouse_event(0x0002, 0, 0, 0, UIntPtr.Zero);
        mouse_event(0x0004, 0, 0, 0, UIntPtr.Zero);
    }
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
        throw "Could not read the desktop window bounds before navigation."
    }

    if ($View -eq "terminal-empty") {
        # The temporary data directory contains no server. Close the onboarding
        # guide, open the Terminal tab, and capture its honest empty state.
        [YukinalWindowCapture]::Click($rect.Left + 877, $rect.Top + 216)
        Start-Sleep -Milliseconds 700
        [YukinalWindowCapture]::Click($rect.Left + 482, $rect.Top + 140)
        Start-Sleep -Seconds 1
    }

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

    if ($View -eq "terminal-demo") {
        # Keep the screenshot grounded in the real layout, then add sanitized
        # sample output so the README communicates what an active terminal and
        # an Agent answer look like without connecting to a real host or model.
        $demo = [System.Drawing.Graphics]::FromImage($bitmap)
        $demo.TextRenderingHint = [System.Drawing.Text.TextRenderingHint]::AntiAliasGridFit

        $terminalBackground = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#08090a"))
        $terminalBorder = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#34383e"), 1)
        $terminalText = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#d9d9d9"))
        $terminalPrompt = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#c4f5a6"))
        $terminalMuted = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#8f979f"))
        $terminalOk = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#a9e6bd"))
        $terminalFont = [System.Drawing.Font]::new("Cascadia Mono", 8.1, [System.Drawing.FontStyle]::Regular)
        $terminalBold = [System.Drawing.Font]::new("Cascadia Mono", 8.1, [System.Drawing.FontStyle]::Bold)

        $demo.FillRectangle($terminalBackground, 382, 184, 568, 357)
        $demo.DrawRectangle($terminalBorder, 381, 183, 569, 359)

        $terminalLines = @(
            @{ Text = '$ sudo nginx -t'; Brush = $terminalPrompt; Font = $terminalBold },
            @{ Text = 'nginx: the configuration file /etc/nginx/nginx.conf syntax is ok'; Brush = $terminalText; Font = $terminalFont },
            @{ Text = 'nginx: configuration file /etc/nginx/nginx.conf test is successful'; Brush = $terminalOk; Font = $terminalFont },
            @{ Text = '$ sudo systemctl reload nginx'; Brush = $terminalPrompt; Font = $terminalBold },
            @{ Text = '● nginx.service - A high performance web server'; Brush = $terminalText; Font = $terminalFont },
            @{ Text = '   Active: active (running) · reload completed in 42 ms'; Brush = $terminalOk; Font = $terminalFont },
            @{ Text = '$ journalctl -u nginx -n 4 --no-pager'; Brush = $terminalPrompt; Font = $terminalBold },
            @{ Text = 'Sep 16 04:31:08 host nginx[2418]: signal process started'; Brush = $terminalMuted; Font = $terminalFont },
            @{ Text = 'Sep 16 04:31:08 host nginx[2418]: configuration reload complete'; Brush = $terminalMuted; Font = $terminalFont },
            @{ Text = '$ curl -fsS http://127.0.0.1/health'; Brush = $terminalPrompt; Font = $terminalBold },
            @{ Text = '{"status":"ok","version":"1.0.0"}'; Brush = $terminalOk; Font = $terminalFont },
            @{ Text = '$ docker ps --format "table {{.Names}}\\t{{.Status}}"'; Brush = $terminalPrompt; Font = $terminalBold },
            @{ Text = 'NAME              STATUS'; Brush = $terminalText; Font = $terminalFont },
            @{ Text = 'api               Up 3 days (healthy)'; Brush = $terminalOk; Font = $terminalFont },
            @{ Text = 'worker            Up 3 days (healthy)'; Brush = $terminalOk; Font = $terminalFont },
            @{ Text = '$ _'; Brush = $terminalPrompt; Font = $terminalBold }
        )
        $terminalY = 198
        foreach ($line in $terminalLines) {
            $demo.DrawString($line.Text, $line.Font, $line.Brush, 397, $terminalY)
            $terminalY += 20
        }

        $panelBackground = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#0d0d0d"))
        $cardBackground = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#171717"))
        $cardBorder = [System.Drawing.Pen]::new([System.Drawing.ColorTranslator]::FromHtml("#343434"), 1)
        $uiText = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#e1e1e1"))
        $uiMuted = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#a0a0a0"))
        $uiAccent = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#b8f3cf"))
        $uiFont = [System.Drawing.Font]::new("Microsoft YaHei UI", 9, [System.Drawing.FontStyle]::Regular)
        $uiSmall = [System.Drawing.Font]::new("Microsoft YaHei UI", 8, [System.Drawing.FontStyle]::Regular)
        $uiBold = [System.Drawing.Font]::new("Microsoft YaHei UI", 9, [System.Drawing.FontStyle]::Bold)

        # Replace the empty Agent body, keeping the real Agent header, context
        # row and composer chrome visible around the example conversation.
        $demo.FillRectangle($panelBackground, 1005, 176, 418, 594)

        $demo.FillRectangle($cardBackground, 1020, 188, 390, 58)
        $demo.DrawRectangle($cardBorder, 1020, 188, 390, 58)
        $demo.DrawString('你', $uiBold, $uiAccent, 1034, 198)
        $demo.DrawString('帮我看一下刚才的 Nginx 配置检查和 reload 日志。', $uiFont, $uiText, 1060, 197)
        $demo.DrawString('有没有需要处理的异常？', $uiSmall, $uiMuted, 1060, 219)

        $demo.FillRectangle($cardBackground, 1020, 258, 390, 214)
        $demo.DrawRectangle($cardBorder, 1020, 258, 390, 214)
        $demo.DrawString('Agent', $uiBold, $uiAccent, 1034, 271)
        $demo.DrawString('我看到了两条关键信息：', $uiFont, $uiText, 1034, 301)
        $demo.DrawString('1. nginx -t 通过，配置语法没有问题。', $uiSmall, $uiText, 1034, 327)
        $demo.DrawString('2. reload 成功，health endpoint 返回 200。', $uiSmall, $uiText, 1034, 348)
        $demo.DrawString('没有看到 5xx、worker 重启或配置回滚。', $uiSmall, $uiText, 1034, 377)
        $demo.DrawString('建议先观察 error.log 5 分钟，再把同样改动', $uiSmall, $uiMuted, 1034, 407)
        $demo.DrawString('放到 staging 做一次回归。', $uiSmall, $uiMuted, 1034, 428)

        $demo.FillRectangle($cardBackground, 1020, 484, 390, 82)
        $demo.DrawRectangle($cardBorder, 1020, 484, 390, 82)
        $demo.DrawString('已读取', $uiSmall, $uiAccent, 1034, 497)
        $demo.DrawString('server.info', $uiSmall, $uiText, 1034, 520)
        $demo.DrawString('docker.ps', $uiSmall, $uiText, 1144, 520)
        $demo.DrawString('只读工具 · 已记录到活动', $uiSmall, $uiMuted, 1034, 544)

        $demo.FillRectangle($cardBackground, 1020, 582, 390, 52)
        $demo.DrawRectangle($cardBorder, 1020, 582, 390, 52)
        $demo.DrawString('只读模式 · 操作前询问', $uiSmall, $uiMuted, 1034, 600)
        $demo.DrawString('没有执行写入或重启。', $uiSmall, $uiAccent, 1034, 619)

        $composerBackground = [System.Drawing.SolidBrush]::new([System.Drawing.ColorTranslator]::FromHtml("#1c1c1c"))
        $demo.FillRectangle($composerBackground, 1001, 811, 422, 95)
        $demo.DrawRectangle($cardBorder, 1001, 811, 422, 95)
        $demo.DrawString('继续追问 Agent…', $uiFont, $uiMuted, 1030, 829)
        $demo.DrawString('目标环境：当前服务器', $uiSmall, $uiMuted, 1118, 878)
        $demo.DrawString('请求批准', $uiSmall, $uiAccent, 1270, 878)

        $demo.Dispose()
        foreach ($resource in @(
            $terminalBackground, $terminalBorder, $terminalText, $terminalPrompt, $terminalMuted, $terminalOk,
            $terminalFont, $terminalBold, $panelBackground, $cardBackground, $cardBorder, $uiText, $uiMuted,
            $uiAccent, $uiFont, $uiSmall, $uiBold, $composerBackground
        )) {
            $resource.Dispose()
        }
    }

    $bitmap.Save($outputPath, [System.Drawing.Imaging.ImageFormat]::Png)
    $bitmap.Dispose()
    Write-Output "PASS: captured ${width}x${height} ($View) to $outputPath"
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
