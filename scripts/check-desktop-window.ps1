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
$binaryPath = [IO.Path]::GetFullPath($binaryPath)
$runningYukinal = @(Get-CimInstance Win32_Process -Filter "Name = 'yukinal-desktop.exe'" -ErrorAction SilentlyContinue)
if ($runningYukinal.Count -gt 0) {
    throw 'Refusing to run the packaged UI smoke while another Yukinal process is using the user profile.'
}

$tauriConfig = Get-Content -Raw -LiteralPath (Join-Path $RepoRoot "apps\desktop\src-tauri\tauri.conf.json") | ConvertFrom-Json
$defaultAppDataDirectory = Join-Path $env:LOCALAPPDATA ([string]$tauriConfig.identifier)
$defaultAppDataExistedBefore = Test-Path -LiteralPath $defaultAppDataDirectory
$dataDirectory = Join-Path ([System.IO.Path]::GetTempPath()) ("yukinal-window-check-" + [Guid]::NewGuid().ToString("N"))
New-Item -ItemType Directory -Path $dataDirectory -Force | Out-Null
$webViewDirectory = Join-Path $dataDirectory "webview2"
$stdoutLog = Join-Path $dataDirectory "app.stdout.log"
$stderrLog = Join-Path $dataDirectory "app.stderr.log"
$oldWebViewDirectory = $env:WEBVIEW2_USER_DATA_FOLDER
$previousDataDirectory = $env:YUKINAL_DATA_DIR
$env:YUKINAL_DATA_DIR = $dataDirectory
$env:WEBVIEW2_USER_DATA_FOLDER = $webViewDirectory
$process = $null
$processStillRunning = $false
$failure = $null
$defaultAppDataCleanupFailure = $null

try {
    Add-Type -AssemblyName UIAutomationClient
    Add-Type -AssemblyName UIAutomationTypes

    $process = Start-Process -FilePath $binaryPath -WorkingDirectory $RepoRoot -PassThru `
        -RedirectStandardOutput $stdoutLog -RedirectStandardError $stderrLog
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
    if (-not $defaultAppDataExistedBefore -and (Test-Path -LiteralPath $defaultAppDataDirectory)) {
        $profileContents = @(Get-ChildItem -LiteralPath $defaultAppDataDirectory -Force)
        if ($profileContents.Count -gt 0) {
            throw "Packaged UI smoke wrote into Tauri's default LocalAppData profile despite WebView2 isolation: $defaultAppDataDirectory`nUnexpected entries: $($profileContents.Name -join ', ')"
        }
    }
}
catch {
    $failure = $_.Exception.Message
    if (Test-Path -LiteralPath $stderrLog) {
        $startupLog = (Get-Content -LiteralPath $stderrLog -Tail 40) -join [Environment]::NewLine
        if (-not [string]::IsNullOrWhiteSpace($startupLog)) {
            $failure = "$failure`nApplication stderr:`n$startupLog"
        }
    }
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
                    $process.WaitForExit(5000) | Out-Null
                }
            }
            else {
                Stop-Process -Id $process.Id -Force
                $process.WaitForExit(5000) | Out-Null
            }
        }
        $process.Refresh()
        $processStillRunning = -not $process.HasExited
        if ($processStillRunning) {
            $defaultAppDataCleanupFailure = "Refusing to clean smoke data because the packaged UI process is still running (pid=$($process.Id))."
        }
    }

    # Tauri creates this empty WebView2 root unconditionally on Windows even when the
    # actual WebView2 profile is redirected. Remove only the directory this smoke created,
    # and only if it is still empty after the app has exited. Never prune user data.
    if (-not $defaultAppDataExistedBefore -and (Test-Path -LiteralPath $defaultAppDataDirectory -PathType Container)) {
        try {
            $otherRunningYukinal = @(Get-CimInstance Win32_Process -Filter "Name = 'yukinal-desktop.exe'" -ErrorAction SilentlyContinue |
                Where-Object { $null -eq $process -or $_.ProcessId -ne $process.Id })
            if ($processStillRunning -or $otherRunningYukinal.Count -gt 0) {
                $defaultAppDataCleanupFailure = "Refusing to clean the Tauri profile while a Yukinal process may be using it; the directory was preserved: $defaultAppDataDirectory"
            } else {
                $profileContents = @(Get-ChildItem -LiteralPath $defaultAppDataDirectory -Force)
                if ($profileContents.Count -eq 0) {
                    Remove-Item -LiteralPath $defaultAppDataDirectory
                } else {
                    $defaultAppDataCleanupFailure = "Tauri's default LocalAppData profile is not empty after the isolated smoke; it was preserved for inspection: $defaultAppDataDirectory`nEntries: $($profileContents.Name -join ', ')"
                }
            }
        }
        catch {
            $defaultAppDataCleanupFailure = "Unable to inspect or remove the exact empty Tauri profile directory $defaultAppDataDirectory`: $($_.Exception.Message)"
        }
    }

    if (-not $processStillRunning -and (Test-Path -LiteralPath $dataDirectory)) {
        Remove-Item -LiteralPath $dataDirectory -Recurse -Force -ErrorAction SilentlyContinue
    }
}

if ($defaultAppDataCleanupFailure) {
    if ($failure) { $failure = "$failure`n$defaultAppDataCleanupFailure" }
    else { $failure = $defaultAppDataCleanupFailure }
}
if ($failure) { throw $failure }

Write-Output "PASS: Yukinal native window and all $($expectedNavigation.Count) primary navigation buttons rendered (pid=$($process.Id), title='$($process.MainWindowTitle)')."
