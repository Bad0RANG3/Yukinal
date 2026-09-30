#requires -Version 7.0

param(
  [string]$InstallerPath,
  [switch]$PlanOnly
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) { throw 'This lifecycle smoke only runs on Windows.' }
if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
  throw 'Run this test from 64-bit PowerShell on 64-bit Windows.'
}

function Stop-ExactProcess {
  param([Diagnostics.Process]$Process, [string]$ExpectedPath)
  if ($null -eq $Process) { return }
  $Process.Refresh()
  if ($Process.HasExited) { return }

  $current = Get-CimInstance Win32_Process -Filter "ProcessId = $($Process.Id)"
  $expected = [IO.Path]::GetFullPath($ExpectedPath)
  if (-not $current -or [IO.Path]::GetFullPath($current.ExecutablePath) -ne $expected) {
    throw "Refusing to stop a process that is not the exact test process: PID $($Process.Id)"
  }
  Stop-Process -Id $Process.Id -Force
  $Process.WaitForExit(5000) | Out-Null
}

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$tauriConfig = Get-Content -Raw -LiteralPath (Join-Path $repoRoot 'apps/desktop/src-tauri/tauri.conf.json') | ConvertFrom-Json
if ([string]::IsNullOrWhiteSpace([string]$tauriConfig.productName)) {
  throw 'Tauri productName is required to resolve the default NSIS install directory safely.'
}
$defaultInstallDir = [IO.Path]::GetFullPath((Join-Path $env:LOCALAPPDATA ([string]$tauriConfig.productName)))
$bundleDir = Join-Path $repoRoot 'target/release/bundle/nsis'
if ([string]::IsNullOrWhiteSpace($InstallerPath)) {
  $candidates = @(Get-ChildItem -LiteralPath $bundleDir -Filter '*setup.exe' -File)
  if ($candidates.Count -ne 1) {
    throw "Expected exactly one NSIS setup.exe in $bundleDir; found $($candidates.Count)."
  }
  $InstallerPath = $candidates[0].FullName
}
$InstallerPath = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $InstallerPath).Path)

$runId = [guid]::NewGuid().ToString('N')
$testRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target/release/nsis-install-smoke'))
$runRoot = [IO.Path]::GetFullPath((Join-Path $testRoot $runId))
$installDir = [IO.Path]::GetFullPath((Join-Path $runRoot 'install'))
$installPrefix = $testRoot.TrimEnd('\') + '\'
if (-not $installDir.StartsWith($installPrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to use an NSIS test install directory outside $testRoot"
}

$appDataRoot = Join-Path $env:APPDATA 'dev.yukinal.workspace'
$localDataRoot = Join-Path $env:LOCALAPPDATA 'dev.yukinal.workspace'
$appDataCanary = Join-Path $appDataRoot "nsis-uninstall-preserves-$runId.marker"
$localDataCanary = Join-Path $localDataRoot "nsis-uninstall-preserves-$runId.marker"
$productRegistryPath = 'HKCU:\Software\Yukinal contributors\Yukinal'
$uninstallRegistryPath = 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\Yukinal'
$runningYukinal = @(Get-CimInstance Win32_Process -Filter "Name = 'yukinal-desktop.exe'")
$nodeOnHost = [bool](Get-Command node.exe -ErrorAction SilentlyContinue)

$blockers = [System.Collections.Generic.List[string]]::new()
if (Test-Path -LiteralPath $appDataRoot) { $blockers.Add("Roaming Yukinal data already exists: $appDataRoot") }
if (Test-Path -LiteralPath $localDataRoot) { $blockers.Add("Local Yukinal data already exists: $localDataRoot") }
if (Test-Path -LiteralPath $productRegistryPath) { $blockers.Add("Yukinal install registry data already exists: $productRegistryPath") }
if (Test-Path -LiteralPath $uninstallRegistryPath) { $blockers.Add("Yukinal uninstall registration already exists: $uninstallRegistryPath") }
if (Test-Path -LiteralPath $installDir) { $blockers.Add("Test install directory already exists: $installDir") }
if (-not [string]::Equals($defaultInstallDir, $installDir, [StringComparison]::OrdinalIgnoreCase) -and
    (Test-Path -LiteralPath $defaultInstallDir)) {
  $blockers.Add("Default NSIS install directory already exists: $defaultInstallDir")
}
if ($runningYukinal.Count -gt 0) { $blockers.Add('A Yukinal process is already running; close it before testing.') }

$plan = [pscustomobject]@{
  Product = 'Yukinal'
  Installer = $InstallerPath
  TestRoot = $runRoot
  InstallDirectory = $installDir
  DefaultInstallDirectory = $defaultInstallDir
  RoamingDataRoot = $appDataRoot
  LocalDataRoot = $localDataRoot
  NodeFoundOnHost = $nodeOnHost
  PlanOnly = [bool]$PlanOnly
  Blockers = @($blockers)
}

if ($PlanOnly) {
  $plan | Format-List
  if ($blockers.Count -gt 0) { exit 2 }
  exit 0
}
if ($blockers.Count -gt 0) {
  $plan | Format-List
  throw 'NSIS lifecycle smoke refused to modify this Windows profile.'
}

New-Item -ItemType Directory -Path $runRoot -Force | Out-Null
$appDataRootCreated = $false
$localDataRootCreated = $false
$appDataCanaryCreated = $false
$localDataCanaryCreated = $false
$installProcess = $null
$updateProcess = $null
$uninstallProcess = $null
$smokeExitCode = $null
$updateExitCode = $null
$uninstallExitCode = $null
$updateCanariesPreserved = $false
$userDataPreserved = $false
$failure = $null
$cleanupFailures = [System.Collections.Generic.List[string]]::new()

try {
  # NSIS requires /D= to be the final, unquoted command-line parameter.
  $installProcess = Start-Process -FilePath $InstallerPath -ArgumentList @('/S', "/D=$installDir") -PassThru -WindowStyle Hidden
  if (-not $installProcess.WaitForExit(180000)) {
    throw 'NSIS installer did not finish within 180 seconds.'
  }
  $installProcess.Refresh()
  if ($installProcess.ExitCode -ne 0) { throw "NSIS installer exited with code $($installProcess.ExitCode)." }

  $productKey = Get-Item -LiteralPath $productRegistryPath -ErrorAction SilentlyContinue
  if ($null -eq $productKey) { throw 'NSIS installer did not register its install directory.' }
  $registeredInstallDir = [IO.Path]::GetFullPath([string]$productKey.GetValue(''))
  if (-not [string]::Equals($registeredInstallDir, $installDir, [StringComparison]::OrdinalIgnoreCase)) {
    throw "NSIS installer escaped the isolated test directory: $registeredInstallDir"
  }

  foreach ($required in @(
    (Join-Path $installDir 'yukinal-desktop.exe'),
    (Join-Path $installDir 'runtime/node.exe'),
    (Join-Path $installDir 'agent/index.js'),
    (Join-Path $installDir 'NOTICE')
  )) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
      throw "NSIS installation is missing a required packaged file: $required"
    }
  }

  New-Item -ItemType Directory -Path $appDataRoot | Out-Null
  $appDataRootCreated = $true
  New-Item -ItemType Directory -Path $localDataRoot | Out-Null
  $localDataRootCreated = $true
  [IO.File]::WriteAllText($appDataCanary, "nsis-$runId")
  $appDataCanaryCreated = $true
  [IO.File]::WriteAllText($localDataCanary, "nsis-$runId")
  $localDataCanaryCreated = $true

  # Exercise Tauri's real update-mode path against this isolated install. Removing one
  # packaged, non-executable file proves the updater rewrites the existing installation;
  # both profile canaries must survive the in-place update as well as final uninstall.
  $updateProbePath = Join-Path $installDir 'NOTICE'
  $updateProbeHash = (Get-FileHash -LiteralPath $updateProbePath -Algorithm SHA256).Hash
  Remove-Item -LiteralPath $updateProbePath -Force
  $updateProcess = Start-Process -FilePath $InstallerPath -ArgumentList @('/S', '/UPDATE', "/D=$installDir") -PassThru -WindowStyle Hidden
  if (-not $updateProcess.WaitForExit(180000)) {
    throw 'NSIS update-mode reinstall did not finish within 180 seconds.'
  }
  $updateProcess.Refresh()
  $updateExitCode = $updateProcess.ExitCode
  if ($updateExitCode -ne 0) { throw "NSIS update-mode reinstall exited with code $updateExitCode." }
  if (-not (Test-Path -LiteralPath $updateProbePath -PathType Leaf)) {
    throw 'NSIS update-mode reinstall did not restore the missing packaged NOTICE file.'
  }
  $restoredUpdateProbeHash = (Get-FileHash -LiteralPath $updateProbePath -Algorithm SHA256).Hash
  if ($restoredUpdateProbeHash -ne $updateProbeHash) {
    throw 'NSIS update-mode reinstall restored NOTICE with unexpected content.'
  }

  $productKey = Get-Item -LiteralPath $productRegistryPath -ErrorAction SilentlyContinue
  if ($null -eq $productKey) { throw 'NSIS update-mode reinstall removed the install-directory registration.' }
  $registeredInstallDir = [IO.Path]::GetFullPath([string]$productKey.GetValue(''))
  if (-not [string]::Equals($registeredInstallDir, $installDir, [StringComparison]::OrdinalIgnoreCase)) {
    throw "NSIS update-mode reinstall changed the isolated install directory: $registeredInstallDir"
  }
  $updateCanariesPreserved =
    ([IO.File]::ReadAllText($appDataCanary) -eq "nsis-$runId") -and
    ([IO.File]::ReadAllText($localDataCanary) -eq "nsis-$runId")
  if (-not $updateCanariesPreserved) {
    throw 'NSIS update-mode reinstall deleted or changed a Roaming/LocalAppData preservation canary.'
  }

  $smokeRoot = Join-Path $repoRoot 'target/release/installer-smoke'
  New-Item -ItemType Directory -Path $smokeRoot -Force | Out-Null
  $smokeLog = Join-Path $runRoot 'installed-app-smoke.log'
  $powershell = Join-Path $PSHOME 'pwsh.exe'
  $smokeOutput = & $powershell -NoLogo -NoProfile -File (Join-Path $PSScriptRoot 'smoke-installed-windows.ps1') -InstallDir $installDir 2>&1
  $smokeExitCode = $LASTEXITCODE
  [IO.File]::WriteAllLines($smokeLog, [string[]]@($smokeOutput | ForEach-Object { $_.ToString() }))
  if ($smokeExitCode -ne 0) { throw "Installed NSIS app startup/shutdown smoke failed with exit $smokeExitCode. See $smokeLog" }
  if (([IO.File]::ReadAllText($appDataCanary) -ne "nsis-$runId") -or
      ([IO.File]::ReadAllText($localDataCanary) -ne "nsis-$runId")) {
    throw 'Installed NSIS app modified a user-data canary before uninstall.'
  }

  $uninstallerPath = Join-Path $installDir 'uninstall.exe'
  if (-not (Test-Path -LiteralPath $uninstallerPath -PathType Leaf)) { throw 'Installed NSIS uninstaller is missing.' }
  # NSIS requires _?= to be the final, unquoted uninstaller parameter.
  $uninstallProcess = Start-Process -FilePath $uninstallerPath -ArgumentList @('/S', "_?=$installDir") -PassThru -WindowStyle Hidden
  if (-not $uninstallProcess.WaitForExit(180000)) {
    throw 'NSIS uninstaller did not finish within 180 seconds.'
  }
  $uninstallProcess.Refresh()
  $uninstallExitCode = $uninstallProcess.ExitCode
  if ($uninstallExitCode -ne 0) { throw "NSIS uninstaller exited with code $uninstallExitCode." }

  $userDataPreserved =
    (Test-Path -LiteralPath $appDataCanary -PathType Leaf) -and
    (Test-Path -LiteralPath $localDataCanary -PathType Leaf) -and
    ([IO.File]::ReadAllText($appDataCanary) -eq "nsis-$runId") -and
    ([IO.File]::ReadAllText($localDataCanary) -eq "nsis-$runId")
  if (-not $userDataPreserved) {
    throw 'Silent NSIS uninstall deleted or changed a Roaming/LocalAppData preservation canary.'
  }
  if (Test-Path -LiteralPath $installDir) {
    $remainingInstallEntries = @(Get-ChildItem -LiteralPath $installDir -Force)
    $unexpectedInstallEntries = @($remainingInstallEntries | Where-Object { $_.FullName -ne $uninstallerPath })
    if ($unexpectedInstallEntries.Count -gt 0) {
      throw "NSIS uninstall left test installation content behind: $($unexpectedInstallEntries.Name -join ', ')"
    }
    # With _?= NSIS runs this executable in place and may be unable to delete itself.
    # It has exited now, so remove only that exact packaged test uninstaller.
    if (Test-Path -LiteralPath $uninstallerPath -PathType Leaf) {
      if (-not $uninstallProcess.HasExited) { throw 'Refusing to remove an NSIS uninstaller that is still running.' }
      Remove-Item -LiteralPath $uninstallerPath -Force
    }
    # NSIS can leave its now-empty root directory after deleting the running uninstaller.
    Remove-Item -LiteralPath $installDir -Force
  }
  if (Test-Path -LiteralPath $uninstallRegistryPath) { throw 'NSIS uninstall left the Yukinal uninstall registration behind.' }
} catch {
  $failure = $_.Exception.Message
} finally {
  try { Stop-ExactProcess -Process $uninstallProcess -ExpectedPath (Join-Path $installDir 'uninstall.exe') }
  catch { $cleanupFailures.Add("Unable to stop exact NSIS uninstaller process: $($_.Exception.Message)") }
  try { Stop-ExactProcess -Process $updateProcess -ExpectedPath $InstallerPath }
  catch { $cleanupFailures.Add("Unable to stop exact NSIS update process: $($_.Exception.Message)") }
  try { Stop-ExactProcess -Process $installProcess -ExpectedPath $InstallerPath }
  catch { $cleanupFailures.Add("Unable to stop exact NSIS installer process: $($_.Exception.Message)") }

  foreach ($canary in @(
    @{ Path = $appDataCanary; Created = $appDataCanaryCreated },
    @{ Path = $localDataCanary; Created = $localDataCanaryCreated }
  )) {
    try {
      if ($canary.Created -and (Test-Path -LiteralPath $canary.Path -PathType Leaf)) {
        Remove-Item -LiteralPath $canary.Path -Force
      }
    } catch { $cleanupFailures.Add("Unable to remove exact canary $($canary.Path): $($_.Exception.Message)") }
  }

  foreach ($root in @(
    @{ Path = $appDataRoot; Created = $appDataRootCreated },
    @{ Path = $localDataRoot; Created = $localDataRootCreated }
  )) {
    try {
      if ($root.Created -and (Test-Path -LiteralPath $root.Path -PathType Container) -and
          @(Get-ChildItem -LiteralPath $root.Path -Force).Count -eq 0) {
        Remove-Item -LiteralPath $root.Path
      }
    } catch { $cleanupFailures.Add("Unable to remove empty test data directory $($root.Path): $($_.Exception.Message)") }
  }

  # This key was absent before the test; remove only the default install-path value if it
  # still points at this exact test installation. Any unexpected registry state is retained.
  try {
    if (Test-Path -LiteralPath $productRegistryPath) {
      $productKey = Get-Item -LiteralPath $productRegistryPath
      $registeredInstallDir = [string]$productKey.GetValue('')
      if ($registeredInstallDir -eq $installDir) {
        & reg.exe delete 'HKCU\Software\Yukinal contributors\Yukinal' /ve /f | Out-Null
        if ($LASTEXITCODE -ne 0) { throw "reg.exe exited with code $LASTEXITCODE" }
        $productKey = Get-Item -LiteralPath $productRegistryPath
        if ($productKey.GetValueNames().Count -eq 0 -and $productKey.GetSubKeyNames().Count -eq 0) {
          Remove-Item -LiteralPath $productRegistryPath
        }
      } else {
        $cleanupFailures.Add("Retained unexpected Yukinal registry value at $productRegistryPath")
      }
    }
  } catch { $cleanupFailures.Add("Unable to clean exact test registry value: $($_.Exception.Message)") }

  try {
    if (Test-Path -LiteralPath $installDir) {
      $resolvedInstallDir = [IO.Path]::GetFullPath($installDir)
      if ($resolvedInstallDir.StartsWith($installPrefix, [StringComparison]::OrdinalIgnoreCase)) {
        Remove-Item -LiteralPath $resolvedInstallDir -Recurse -Force
      } else {
        throw "Refusing to clean install directory outside test root: $resolvedInstallDir"
      }
    }
  } catch { $cleanupFailures.Add("Unable to clean exact test install directory: $($_.Exception.Message)") }
}

[pscustomobject]@{
  Installer = $InstallerPath
  InstallDirectory = $installDir
  NodeFoundOnHost = $nodeOnHost
  InstalledAppSmokeExitCode = $smokeExitCode
  UpdateModeReinstallExitCode = $updateExitCode
  UserDataPreservedAcrossUpdate = $updateCanariesPreserved
  UninstallExitCode = $uninstallExitCode
  RoamingDataPreserved = [bool]($appDataCanaryCreated -and $userDataPreserved)
  LocalDataPreserved = [bool]($localDataCanaryCreated -and $userDataPreserved)
  Failure = $failure
  CleanupFailures = @($cleanupFailures)
  Logs = $runRoot
} | Format-List

if ($cleanupFailures.Count -gt 0) { throw ($cleanupFailures -join ' ') }
if ($failure) { throw $failure }
if ($updateExitCode -ne 0 -or -not $updateCanariesPreserved -or $uninstallExitCode -ne 0 -or -not $userDataPreserved) {
  throw 'NSIS install/update/uninstall lifecycle smoke failed.'
}

Write-Output 'Windows NSIS install/update/startup/shutdown/uninstall/data-preservation smoke: green.'
