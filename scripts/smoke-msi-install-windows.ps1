#requires -Version 7.0

<#
.SYNOPSIS
  Install, smoke-test, and uninstall one exact Yukinal MSI on a disposable Windows x64 host.

.DESCRIPTION
  This intentionally requires an elevated PowerShell session and a host with no Node.js or
  existing Yukinal installation. For ephemeral CI images that include Node.js, the explicit
  -AllowInstalledNodeForPathIsolationSmoke switch permits the run only as a PATH-isolation
  test: the packaged application is launched with a restricted PATH by smoke-installed-windows.ps1.
  Such a run does not replace acceptance on a truly Node-free host. It never removes arbitrary
  files: MSI owns both install and uninstall. Logs and isolated smoke data are kept below target/release.
#>

param(
  [string]$MsiPath = (Join-Path $PSScriptRoot '..\target\release\bundle\msi\Yukinal_1.0.0_x64_en-US.msi'),
  [switch]$PlanOnly,
  [switch]$AllowInstalledNodeForPathIsolationSmoke
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) { throw 'This MSI lifecycle test only runs on Windows.' }

$repoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path
$msiItem = Get-Item -LiteralPath $MsiPath -ErrorAction Stop
if ($msiItem.PSIsContainer) { throw "MSI path is not a file: $MsiPath" }
$msiPath = $msiItem.FullName
$smokeScript = Join-Path $PSScriptRoot 'smoke-installed-windows.ps1'
if (-not (Test-Path -LiteralPath $smokeScript -PathType Leaf)) {
  throw "Required packaged startup smoke script is missing: $smokeScript"
}

$installer = New-Object -ComObject WindowsInstaller.Installer
$database = $installer.OpenDatabase($msiPath, 0)
$properties = @{}
$propertyView = $database.OpenView('SELECT `Property`, `Value` FROM `Property`')
$propertyView.Execute()
while ($record = $propertyView.Fetch()) {
  $properties[$record.StringData(1)] = $record.StringData(2)
}
$directoryView = $database.OpenView('SELECT `Directory_Parent`, `DefaultDir` FROM `Directory` WHERE `Directory` = ''INSTALLDIR''')
$directoryView.Execute()
$installDirectoryRecord = $directoryView.Fetch()
if (-not $installDirectoryRecord) { throw 'The MSI has no INSTALLDIR directory entry.' }
$installDirectoryParent = $installDirectoryRecord.StringData(1)
$installDirectoryLeaf = ($installDirectoryRecord.StringData(2) -split '\|')[-1]

$productName = $properties['ProductName']
$productVersion = $properties['ProductVersion']
$productCode = $properties['ProductCode']
$allUsers = $properties['ALLUSERS']
if ($productName -ne 'Yukinal') { throw "Refusing unexpected MSI product name: $productName" }
if ($productCode -notmatch '^\{[0-9A-Fa-f-]{36}\}$') { throw "Invalid MSI ProductCode: $productCode" }
if ($allUsers -ne '1' -or $installDirectoryParent -ne 'ProgramFiles64Folder') {
  throw 'The MSI is not the expected 64-bit per-machine Yukinal package.'
}
if (-not [Environment]::Is64BitOperatingSystem -or -not [Environment]::Is64BitProcess) {
  throw 'Run this test from 64-bit PowerShell on 64-bit Windows.'
}

$installDir = Join-Path $env:ProgramFiles $installDirectoryLeaf
$appDataRoot = Join-Path $env:LOCALAPPDATA 'dev.yukinal.workspace'
$appPath = Join-Path $installDir 'yukinal-desktop.exe'
$runtimePath = Join-Path $installDir 'runtime/node.exe'
$productRegistryPath = "HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\$productCode"
$uninstallRoots = @(
  'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\*',
  'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\*'
)
$installedPrograms = foreach ($root in $uninstallRoots) {
  Get-ItemProperty -Path $root -ErrorAction SilentlyContinue
}
$matchingProduct = @($installedPrograms | Where-Object { $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -eq $productName })
$nodePrograms = @($installedPrograms | Where-Object { $_.PSObject.Properties['DisplayName'] -and $_.DisplayName -match '^Node\.js(?:\s|$)' })
$nodeCommand = Get-Command node.exe -ErrorAction SilentlyContinue | Select-Object -First 1
$nodePathDirectories = @(
  [Environment]::GetEnvironmentVariable('Path', 'Machine'),
  [Environment]::GetEnvironmentVariable('Path', 'User'),
  $env:Path
) | Where-Object { $_ } | ForEach-Object { $_ -split ';' } | ForEach-Object { $_.Trim().Trim('"') } | Where-Object { $_ }
$nodePathHits = @($nodePathDirectories | Where-Object { Test-Path -LiteralPath (Join-Path $_ 'node.exe') -PathType Leaf })
$nodeIsPresent = [bool]($nodeCommand -or $nodePrograms.Count -gt 0 -or $nodePathHits.Count -gt 0)
$principal = New-Object Security.Principal.WindowsPrincipal([Security.Principal.WindowsIdentity]::GetCurrent())
$isElevated = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$runningYukinal = @(Get-CimInstance Win32_Process -Filter "Name = 'yukinal-desktop.exe'" -ErrorAction SilentlyContinue)
$blockers = [Collections.Generic.List[string]]::new()

if ($allUsers -ne '1') { $blockers.Add('MSI is not per-machine.') }
if (-not $isElevated) { $blockers.Add('PowerShell is not elevated; run it as Administrator on a disposable test host.') }
$nodeEvidence = $null
if ($nodeIsPresent) {
  $nodeEvidence = if ($nodeCommand) { $nodeCommand.Source } elseif ($nodePrograms.Count -gt 0) { $nodePrograms[0].DisplayName } else { Join-Path $nodePathHits[0] 'node.exe' }
  if (-not $AllowInstalledNodeForPathIsolationSmoke) {
    $blockers.Add("Node.js is present on the host ($nodeEvidence); use a clean image with no Node.js installed.")
  }
}
if ($matchingProduct.Count -gt 0 -or (Test-Path -LiteralPath $productRegistryPath) -or (Test-Path -LiteralPath $installDir)) {
  $blockers.Add('Yukinal is already installed or its machine-wide install directory already exists.')
}
if (Test-Path -LiteralPath $appDataRoot) {
  $blockers.Add("Yukinal user-data root already exists; use a fresh disposable Windows profile to avoid touching existing data: $appDataRoot")
}
if ($runningYukinal.Count -gt 0) { $blockers.Add('A Yukinal process is already running; close it before testing.') }

$plan = [pscustomobject]@{
  Product = $productName
  Version = $productVersion
  ProductCode = $productCode
  MsiPath = $msiPath
  MsiSha256 = (Get-FileHash -LiteralPath $msiPath -Algorithm SHA256).Hash
  InstallDirectory = $installDir
  Elevated = $isElevated
  NodeFoundOnHost = $nodeIsPresent
  NodeEvidence = $nodeEvidence
  AllowInstalledNodeForPathIsolationSmoke = [bool]$AllowInstalledNodeForPathIsolationSmoke
  ExistingYukinalInstall = [bool]($matchingProduct.Count -gt 0 -or (Test-Path -LiteralPath $productRegistryPath) -or (Test-Path -LiteralPath $installDir))
  ExistingUserData = [bool](Test-Path -LiteralPath $appDataRoot)
  PlanOnly = [bool]$PlanOnly
  Blockers = @($blockers)
}

if ($PlanOnly) {
  $plan | Format-List
  return
}
if ($blockers.Count -gt 0) {
  $plan | Format-List
  throw "MSI lifecycle preflight refused to make changes: $($blockers -join ' ')"
}

$testRoot = Join-Path $repoRoot 'target/release/msi-install-smoke'
New-Item -ItemType Directory -Path $testRoot -Force | Out-Null
$installerSmokeRoot = Join-Path $repoRoot 'target/release/installer-smoke'
New-Item -ItemType Directory -Path $installerSmokeRoot -Force | Out-Null
$runId = [guid]::NewGuid().ToString('N')
$installLog = Join-Path $testRoot "install-$runId.log"
$uninstallLog = Join-Path $testRoot "uninstall-$runId.log"
$smokeStdout = Join-Path $testRoot "smoke-$runId.stdout.log"
$smokeStderr = Join-Path $testRoot "smoke-$runId.stderr.log"
$preservationCanary = Join-Path $appDataRoot "msi-uninstall-preserves-$runId.marker"
$appDataRootCreated = $false
$preservationCanaryCreated = $false
$userDataPreserved = $false
$installExitCode = $null
$smokeExitCode = $null
$uninstallExitCode = $null
$smokePassed = $false
$uninstallPassed = $false
$failure = $null
$cleanupFailure = $null

try {
  New-Item -ItemType Directory -Path $appDataRoot | Out-Null
  $appDataRootCreated = $true
  New-Item -ItemType File -Path $preservationCanary | Out-Null
  $preservationCanaryCreated = $true

  $installArguments = @('/i', "`"$msiPath`"", '/qn', '/norestart', '/L*v', "`"$installLog`"")
  $installProcess = Start-Process -FilePath msiexec.exe -ArgumentList $installArguments -Wait -PassThru -WindowStyle Hidden
  $installExitCode = $installProcess.ExitCode
  if ($installExitCode -notin @(0, 3010)) { throw "MSI install failed with exit code $installExitCode. See $installLog" }

  if (-not (Test-Path -LiteralPath $productRegistryPath)) {
    throw "MSI returned success but its ProductCode is not registered: $productCode"
  }
  foreach ($required in @($appPath, $runtimePath, (Join-Path $installDir 'runtime/LICENSE'), (Join-Path $installDir 'agent/index.js'))) {
    if (-not (Test-Path -LiteralPath $required -PathType Leaf)) {
      throw "Installed MSI resource is missing: $required"
    }
  }

  $pwshPath = (Get-Command pwsh.exe -ErrorAction Stop).Source
  $smokeArguments = @('-NoProfile', '-File', "`"$smokeScript`"", '-InstallDir', "`"$installDir`"")
  $smokeProcess = Start-Process -FilePath $pwshPath -ArgumentList $smokeArguments -Wait -PassThru -WindowStyle Hidden -RedirectStandardOutput $smokeStdout -RedirectStandardError $smokeStderr
  $smokeExitCode = $smokeProcess.ExitCode
  if ($smokeExitCode -ne 0) { throw "Installed MSI startup/shutdown smoke failed with exit code $smokeExitCode. See $smokeStdout and $smokeStderr" }
  $smokePassed = $true
} catch {
  $failure = $_.Exception.Message
} finally {
  if (Test-Path -LiteralPath $productRegistryPath) {
    try {
      $uninstallArguments = @('/x', $productCode, '/qn', '/norestart', '/L*v', "`"$uninstallLog`"")
      $uninstallProcess = Start-Process -FilePath msiexec.exe -ArgumentList $uninstallArguments -Wait -PassThru -WindowStyle Hidden
      $uninstallExitCode = $uninstallProcess.ExitCode
      if ($uninstallExitCode -notin @(0, 3010)) { throw "MSI uninstall failed with exit code $uninstallExitCode. See $uninstallLog" }
      if ((Test-Path -LiteralPath $productRegistryPath) -or (Test-Path -LiteralPath $installDir)) {
        throw "MSI uninstall returned success but registration or install directory remains: $installDir"
      }
      $uninstallPassed = $true
    } catch {
      $cleanupFailure = $_.Exception.Message
    }
  } elseif (Test-Path -LiteralPath $installDir) {
    $cleanupFailure = "MSI left an unregistered install directory. It was preserved for manual inspection: $installDir"
  }

  if ($uninstallPassed) {
    $userDataPreserved = Test-Path -LiteralPath $preservationCanary -PathType Leaf
    if (-not $userDataPreserved) {
      $cleanupFailure = "MSI uninstall did not preserve the test user-data canary: $preservationCanary"
    }
  }

  try {
    if ($preservationCanaryCreated -and (Test-Path -LiteralPath $preservationCanary -PathType Leaf)) {
      Remove-Item -LiteralPath $preservationCanary
    }
    if ($appDataRootCreated -and (Test-Path -LiteralPath $appDataRoot -PathType Container)) {
      $remainingAppData = @(Get-ChildItem -LiteralPath $appDataRoot -Force)
      if ($remainingAppData.Count -eq 0) { Remove-Item -LiteralPath $appDataRoot }
    }
  } catch {
    $dataCleanupFailure = "Unable to clean the exact MSI preservation canary: $($_.Exception.Message)"
    if ($cleanupFailure) { $cleanupFailure = "$cleanupFailure $dataCleanupFailure" }
    else { $cleanupFailure = $dataCleanupFailure }
  }
}

[pscustomobject]@{
  Product = $productName
  Version = $productVersion
  ProductCode = $productCode
  InstallExitCode = $installExitCode
  InstalledPayloadSmokePassed = $smokePassed
  SmokeExitCode = $smokeExitCode
  UninstallExitCode = $uninstallExitCode
  UninstallPassed = $uninstallPassed
  UserDataPreserved = $userDataPreserved
  InstallLog = $installLog
  SmokeStdout = $smokeStdout
  SmokeStderr = $smokeStderr
  UninstallLog = $uninstallLog
} | Format-List

if ($failure -or $cleanupFailure) {
  $messages = @()
  if ($failure) { $messages += $failure }
  if ($cleanupFailure) { $messages += "Cleanup status: $cleanupFailure" }
  throw ($messages -join ' ')
}
if (-not $smokePassed -or -not $uninstallPassed -or -not $userDataPreserved) {
  throw 'MSI install, packaged startup/shutdown, uninstall, and user-data preservation did not all pass.'
}

if ($AllowInstalledNodeForPathIsolationSmoke -and $nodeIsPresent) {
  Write-Output 'Windows MSI install/startup/shutdown/uninstall lifecycle: green (host Node excluded from application PATH).'
} else {
  Write-Output 'Windows MSI install/startup/shutdown/uninstall lifecycle: green.'
}
