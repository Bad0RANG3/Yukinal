param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")),
    [int]$TimeoutSeconds = 10
)

$binaryPath = Join-Path $RepoRoot "target\debug\yukinal-desktop.exe"
if (-not (Test-Path -LiteralPath $binaryPath)) {
    throw "Debug binary not found: $binaryPath"
}

$dataDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("yukinal-window-check-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
$previousDataDirectory = $env:YUKINAL_DATA_DIR
$env:YUKINAL_DATA_DIR = $dataDirectory
$process = $null

try {
    $process = Start-Process -FilePath $binaryPath -WorkingDirectory $RepoRoot -PassThru
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)

    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Desktop process exited before creating a window (exit code $($process.ExitCode))."
        }
        if ($process.MainWindowHandle -ne 0) {
            Write-Output "PASS: Yukinal window created (pid=$($process.Id), hwnd=$($process.MainWindowHandle), title='$($process.MainWindowTitle)')."
            return
        }
    } while ((Get-Date) -lt $deadline)

    throw "Desktop process stayed alive but created no top-level window (pid=$($process.Id), mainWindowHandle=$($process.MainWindowHandle))."
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
            if ($process.MainWindowHandle -ne 0) {
                $process.CloseMainWindow() | Out-Null
                if (-not $process.WaitForExit(2000)) {
                    Stop-Process -Id $process.Id -Force
                }
            }
            else {
                Stop-Process -Id $process.Id -Force
            }
        }
    }

    if (Test-Path -LiteralPath $dataDirectory) {
        Remove-Item -LiteralPath $dataDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}
