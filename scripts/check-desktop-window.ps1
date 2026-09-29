param(
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot "..")),
    [int]$TimeoutSeconds = 20
)

$ErrorActionPreference = "Stop"

$binaryPath = Join-Path $RepoRoot "target\release\yukinal-desktop.exe"
if (-not (Test-Path -LiteralPath $binaryPath)) {
    $binaryPath = Join-Path $RepoRoot "target\debug\yukinal-desktop.exe"
}
if (-not (Test-Path -LiteralPath $binaryPath)) {
    throw "Desktop binary not found in target\release or target\debug under $RepoRoot."
}

$dataDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("yukinal-window-check-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
$webViewDirectory = Join-Path $dataDirectory "webview2"
$oldWebViewDirectory = $env:WEBVIEW2_USER_DATA_FOLDER
$previousDataDirectory = $env:YUKINAL_DATA_DIR
$env:YUKINAL_DATA_DIR = $dataDirectory
$env:WEBVIEW2_USER_DATA_FOLDER = $webViewDirectory
$process = $null

try {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes

    $process = Start-Process -FilePath $binaryPath -WorkingDirectory $RepoRoot -PassThru
    $deadline = (Get-Date).AddSeconds($TimeoutSeconds)

    do {
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Desktop process exited before creating a window (exit code $($process.ExitCode))."
        }
        if ($process.MainWindowHandle -ne 0) {
            break
        }
    } while ((Get-Date) -lt $deadline)

    if ($process.MainWindowHandle -eq 0) {
        throw "Desktop process stayed alive but created no top-level window (pid=$($process.Id), mainWindowHandle=$($process.MainWindowHandle))."
    }

    $automationRoot = [System.Windows.Automation.AutomationElement]::FromHandle($process.MainWindowHandle)
    $buttonCondition = [System.Windows.Automation.PropertyCondition]::new(
        [System.Windows.Automation.AutomationElement]::ControlTypeProperty,
        [System.Windows.Automation.ControlType]::Button
    )
    $expectedNavigation = @("服务器", "项目", "排查任务", "动态", "设置")
    $uiDeadline = (Get-Date).AddSeconds($TimeoutSeconds)
    $missingNavigation = $expectedNavigation
    do {
        $missingNavigation = @(
            foreach ($label in $expectedNavigation) {
                $labelCondition = [System.Windows.Automation.PropertyCondition]::new(
                    [System.Windows.Automation.AutomationElement]::NameProperty,
                    $label
                )
                $condition = [System.Windows.Automation.AndCondition]::new($buttonCondition, $labelCondition)
                if ($null -eq $automationRoot.FindFirst([System.Windows.Automation.TreeScope]::Descendants, $condition)) {
                    $label
                }
            }
        )
        if ($missingNavigation.Count -eq 0) { break }
        Start-Sleep -Milliseconds 250
        $process.Refresh()
        if ($process.HasExited) {
            throw "Desktop process exited while rendering the main navigation (exit code $($process.ExitCode))."
        }
    } while ((Get-Date) -lt $uiDeadline)

    if ($missingNavigation.Count -gt 0) {
        throw "Yukinal window opened, but its accessible primary navigation did not render. Missing buttons: $($missingNavigation -join ', ')."
    }
    if (-not (Test-Path -LiteralPath $webViewDirectory)) {
        throw "WebView2 did not create its isolated user-data directory: $webViewDirectory"
    }

    Write-Output "PASS: Yukinal native window and all $($expectedNavigation.Count) primary navigation buttons rendered (pid=$($process.Id), title='$($process.MainWindowTitle)')."
}
finally {
    if ($null -eq $previousDataDirectory) {
        Remove-Item Env:YUKINAL_DATA_DIR -ErrorAction SilentlyContinue
    }
    else {
        $env:YUKINAL_DATA_DIR = $previousDataDirectory
    }

    if ($null -eq $oldWebViewDirectory) {
        Remove-Item Env:WEBVIEW2_USER_DATA_FOLDER -ErrorAction SilentlyContinue
    }
    else {
        $env:WEBVIEW2_USER_DATA_FOLDER = $oldWebViewDirectory
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
