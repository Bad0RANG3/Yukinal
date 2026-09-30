#requires -Version 7.0

<#
.SYNOPSIS
  Launch an installed or MSI-extracted Yukinal build with Node absent from PATH and verify sidecar shutdown.

.DESCRIPTION
  This is a Windows release-acceptance probe, not part of `pnpm check`: it exercises the
  packaged Tauri binary, its real resource directory, and the Windows process lifecycle.
  The test database and WebView2 user-data folder are routed under target/release so the
  user's normal Yukinal profile is untouched.

.PARAMETER ForceTerminateAfterReady
  Force-terminate only the exact packaged application process started by this test after the
  Agent handshake succeeds, then verify that closing the parent's pipes stops the sidecar.
#>

param(
  [ValidateRange(1, 60)][int]$ReadyTimeoutSeconds = 25,
  [ValidateRange(1, 60)][int]$CloseTimeoutSeconds = 15,
  [string]$InstallDir,
  [switch]$ForceTerminateAfterReady
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) { throw 'This smoke test only runs on Windows.' }

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$testRoot = Join-Path $repoRoot 'target/release/installer-smoke'
if ([string]::IsNullOrWhiteSpace($InstallDir)) {
  $InstallDir = Join-Path $env:LOCALAPPDATA 'Yukinal'
}
$installDir = [IO.Path]::GetFullPath($InstallDir)
$appPath = Join-Path $installDir 'yukinal-desktop.exe'
$runtimePath = Join-Path $installDir 'runtime/node.exe'
$sidecarCommandPattern = '[\\/]agent[\\/]index\.js(?=$|[\s"''])'

foreach ($required in @($appPath, $runtimePath, (Join-Path $installDir 'agent/index.js'))) {
  if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
    throw "Required installed resource is missing: $required"
  }
}
if (-not (Test-Path -LiteralPath $testRoot -PathType Container)) {
  throw "Expected test artifact directory is missing: $testRoot"
}

$appPath = [IO.Path]::GetFullPath($appPath)
$runtimePath = [IO.Path]::GetFullPath($runtimePath)
$existing = Get-CimInstance Win32_Process -Filter "Name = 'yukinal-desktop.exe'" |
  Where-Object { $_.ExecutablePath -eq $appPath }
if ($existing) { throw "Refusing to interfere with an already running installation (PID $($existing.ProcessId))." }

$runId = [guid]::NewGuid().ToString('N')
$dataDir = Join-Path $testRoot "package-data-$runId"
$webViewDataDir = Join-Path $dataDir 'webview2'
$stdoutPath = Join-Path $testRoot "package-$runId.stdout.log"
$stderrPath = Join-Path $testRoot "package-$runId.stderr.log"
$oldPath = $env:PATH
$oldDataDir = [Environment]::GetEnvironmentVariable('YUKINAL_DATA_DIR', 'Process')
$oldWebViewDataDir = [Environment]::GetEnvironmentVariable('WEBVIEW2_USER_DATA_FOLDER', 'Process')
$overrideNames = @('YUKINAL_NODE', 'YUKINAL_AGENT_COMMAND', 'YUKINAL_AGENT_ARGS', 'YUKINAL_AGENT_ENTRY')
$oldOverrides = @{}
foreach ($name in $overrideNames) {
  $oldOverrides[$name] = [Environment]::GetEnvironmentVariable($name, 'Process')
}

$app = $null
$appExited = $false
$closeAccepted = $false
$shutdownRequested = $false
$shutdownMode = if ($ForceTerminateAfterReady) { 'forced-parent-termination' } else { 'graceful-window-close' }
$sidecarStarted = $false
$sidecarHashMatches = $false
$agentReady = $false
$sidecarStopped = $false
$nodeOnRestrictedPath = $false
$webViewDataIsolationVerified = $false
$sidecarPath = $null
$failure = $null

try {
  # WebView2 otherwise creates EBWebView under the normal LocalAppData profile, even when
  # Yukinal's own database directory is redirected for this smoke.
  New-Item -ItemType Directory -Path $dataDir -Force | Out-Null
  $env:PATH = "$env:SystemRoot\System32;$env:SystemRoot"
  $env:YUKINAL_DATA_DIR = $dataDir
  $env:WEBVIEW2_USER_DATA_FOLDER = $webViewDataDir
  foreach ($name in $overrideNames) { Remove-Item "Env:\$name" -ErrorAction SilentlyContinue }

  $nodeOnRestrictedPath = [bool](Get-Command node.exe -ErrorAction SilentlyContinue)
  if ($nodeOnRestrictedPath) {
    throw 'The restricted PATH unexpectedly resolves node.exe.'
  }

  $app = Start-Process -FilePath $appPath `
    -WorkingDirectory $installDir `
    -PassThru `
    -WindowStyle Hidden `
    -RedirectStandardOutput $stdoutPath `
    -RedirectStandardError $stderrPath

  $sidecar = $null
  for ($attempt = 0; $attempt -lt 20; $attempt++) {
    Start-Sleep -Seconds 1
    $app.Refresh()
    $sidecar = Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)" |
      Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match $sidecarCommandPattern } |
      Select-Object -First 1
    if ($sidecar) { break }
    if ($app.HasExited) { break }
  }

  if ($sidecar) {
    $sidecarStarted = $true
    $sidecarPath = $sidecar.ExecutablePath
    $expectedHash = (Get-FileHash -LiteralPath $runtimePath -Algorithm SHA256).Hash
    $actualHash = (Get-FileHash -LiteralPath $sidecarPath -Algorithm SHA256).Hash
    $sidecarHashMatches = $actualHash -eq $expectedHash

    for ($attempt = 0; $attempt -lt $ReadyTimeoutSeconds; $attempt++) {
      $app.Refresh()
      $startupLog = if (Test-Path -LiteralPath $stderrPath) { Get-Content -LiteralPath $stderrPath -Raw } else { '' }
      if ($startupLog -match '\[yukinal\] autostart ok pid=') { $agentReady = $true; break }
      if ($startupLog -match '\[yukinal\] autostart failed:') { break }
      if ($app.HasExited) { break }
      Start-Sleep -Seconds 1
    }
  }

  $app.Refresh()
  if (-not $app.HasExited) {
    if ($ForceTerminateAfterReady) {
      $currentApp = Get-CimInstance Win32_Process -Filter "ProcessId = $($app.Id)"
      if (-not $currentApp -or $currentApp.ExecutablePath -ne $appPath) {
        throw 'Refusing to force-terminate a process that is not the exact packaged application started by this test.'
      }
      Stop-Process -Id $app.Id -Force
      $shutdownRequested = $true
    } else {
      $closeAccepted = $app.CloseMainWindow()
      $shutdownRequested = $closeAccepted
    }
    if ($shutdownRequested) { $appExited = $app.WaitForExit($CloseTimeoutSeconds * 1000) }
  } else {
    $appExited = $true
  }

  if ($appExited) {
    for ($attempt = 0; $attempt -lt 10; $attempt++) {
      $remaining = Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)" |
        Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match $sidecarCommandPattern } |
        Select-Object -First 1
      if (-not $remaining) { $sidecarStopped = $true; break }
      Start-Sleep -Seconds 1
    }
  }
  $webViewDataIsolationVerified = Test-Path -LiteralPath $webViewDataDir -PathType Container
} catch {
  $failure = $_.Exception.Message
} finally {
  if ($app) {
    $app.Refresh()
    if (-not $app.HasExited) {
      $current = Get-CimInstance Win32_Process -Filter "ProcessId = $($app.Id)"
      if ($current -and $current.ExecutablePath -eq $appPath) {
        Stop-Process -Id $app.Id -Force -ErrorAction SilentlyContinue
      }
      $app.WaitForExit(5000) | Out-Null
    }

    # A failed smoke must not leave the exact sidecar it launched behind.
    $children = Get-CimInstance Win32_Process -Filter "ParentProcessId = $($app.Id)" |
      Where-Object { $_.Name -eq 'node.exe' -and $_.CommandLine -match $sidecarCommandPattern }
    foreach ($child in $children) {
      if ($child.ExecutablePath -and (Get-FileHash -LiteralPath $child.ExecutablePath -Algorithm SHA256).Hash -eq (Get-FileHash -LiteralPath $runtimePath -Algorithm SHA256).Hash) {
        Stop-Process -Id $child.ProcessId -Force -ErrorAction SilentlyContinue
      }
    }
  }

  $env:PATH = $oldPath
  if ($null -eq $oldDataDir) { Remove-Item Env:\YUKINAL_DATA_DIR -ErrorAction SilentlyContinue }
  else { [Environment]::SetEnvironmentVariable('YUKINAL_DATA_DIR', $oldDataDir, 'Process') }
  if ($null -eq $oldWebViewDataDir) { Remove-Item Env:\WEBVIEW2_USER_DATA_FOLDER -ErrorAction SilentlyContinue }
  else { [Environment]::SetEnvironmentVariable('WEBVIEW2_USER_DATA_FOLDER', $oldWebViewDataDir, 'Process') }
  foreach ($name in $overrideNames) {
    if ($null -eq $oldOverrides[$name]) { Remove-Item "Env:\$name" -ErrorAction SilentlyContinue }
    else { [Environment]::SetEnvironmentVariable($name, $oldOverrides[$name], 'Process') }
  }
}

[pscustomobject]@{
  NodeOnRestrictedPath = $nodeOnRestrictedPath
  WebView2DataIsolationVerified = $webViewDataIsolationVerified
  WebView2UserDataFolder = $webViewDataDir
  BundledSidecarStarted = $sidecarStarted
  RuntimeHashMatches = $sidecarHashMatches
  AgentHandshakeReady = $agentReady
  SidecarExecutable = $sidecarPath
  ShutdownMode = $shutdownMode
  CloseRequestAccepted = $closeAccepted
  ShutdownRequested = $shutdownRequested
  CloseTimeoutSeconds = $CloseTimeoutSeconds
  AppExitedWithinTimeout = $appExited
  SidecarStoppedAfterExit = $sidecarStopped
  StdoutLog = $stdoutPath
  StderrLog = $stderrPath
} | Format-List

if ($failure) { throw $failure }
if (-not $sidecarStarted -or -not $sidecarHashMatches -or -not $agentReady -or -not $shutdownRequested -or -not $appExited -or -not $sidecarStopped -or -not $webViewDataIsolationVerified) {
  if (Test-Path -LiteralPath $stderrPath) {
    Get-Content -LiteralPath $stderrPath -Tail 60
  }
  throw 'Windows packaged startup/shutdown smoke failed.'
}

Write-Output "Windows packaged $shutdownMode smoke: green."
