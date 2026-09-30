#requires -Version 7.0

param([string]$MakeNSISPath)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if (-not $IsWindows) { throw 'This NSIS checkbox UI smoke only runs on Windows.' }
if (-not [Environment]::Is64BitProcess) { throw 'Run this test from 64-bit PowerShell.' }

$repoRoot = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot '..')).Path
$generatedNsiPath = Join-Path $repoRoot 'target/release/nsis/x64/installer.nsi'
$generatedEnglishPath = Join-Path (Split-Path -Parent $generatedNsiPath) 'English.nsh'
$hookPath = [IO.Path]::GetFullPath((Join-Path $repoRoot 'apps/desktop/src-tauri/nsis/installer-hooks.nsh'))
foreach ($requiredFile in @($generatedNsiPath, $generatedEnglishPath, $hookPath)) {
  if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
    throw "Required NSIS build input is missing: $requiredFile. Run pnpm package first."
  }
}

if ([string]::IsNullOrWhiteSpace($MakeNSISPath)) {
  $onPath = Get-Command makensis.exe -ErrorAction SilentlyContinue
  if ($onPath) {
    $MakeNSISPath = $onPath.Source
  } else {
    $tauriBundledMakensis = Join-Path $env:LOCALAPPDATA 'tauri/NSIS/makensis.exe'
    if (Test-Path -LiteralPath $tauriBundledMakensis -PathType Leaf) {
      $MakeNSISPath = $tauriBundledMakensis
    } else {
      throw 'makensis.exe was not found on PATH or in the Tauri NSIS tool cache. Run pnpm package first.'
    }
  }
}
$MakeNSISPath = [IO.Path]::GetFullPath((Resolve-Path -LiteralPath $MakeNSISPath).Path)

$generatedNsi = [IO.File]::ReadAllText($generatedNsiPath)
$generatedEnglish = [IO.File]::ReadAllText($generatedEnglishPath)
$confirmShow = [regex]::Match($generatedNsi, '(?ms)^Function un\.ConfirmShow\b.*?^FunctionEnd\s*$')
$confirmLeave = [regex]::Match($generatedNsi, '(?ms)^Function un\.ConfirmLeave\b.*?^FunctionEnd\s*$')
$layoutDefine = [regex]::Match($generatedNsi, '(?m)^!define /ifndef WS_EX_LAYOUTRTL\s+[^\r\n]+').Value
$checkboxLabel = [regex]::Match($generatedEnglish, '(?m)^LangString deleteAppData \$\{LANG_ENGLISH\} .+$').Value
if (-not $confirmShow.Success -or -not $confirmLeave.Success -or
    [string]::IsNullOrWhiteSpace($layoutDefine) -or [string]::IsNullOrWhiteSpace($checkboxLabel)) {
  throw 'Could not extract the confirmation-page callbacks or checkbox label from the current Tauri-generated NSIS script.'
}

$runId = [guid]::NewGuid().ToString('N')
$smokeRoot = [IO.Path]::GetFullPath((Join-Path $repoRoot 'target/release/nsis-checkbox-ui-smoke'))
$runRoot = [IO.Path]::GetFullPath((Join-Path $smokeRoot $runId))
$runPrefix = $smokeRoot.TrimEnd('\') + '\'
if (-not $runRoot.StartsWith($runPrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to use an NSIS checkbox smoke directory outside $smokeRoot"
}
if (Test-Path -LiteralPath $runRoot) { throw "NSIS checkbox smoke directory already exists: $runRoot" }

$installerPath = Join-Path $runRoot 'checkbox-ui-smoke.exe'
$installDir = Join-Path $runRoot 'install'
$uninstallerPath = Join-Path $installDir 'uninstall.exe'
$sourcePath = Join-Path $runRoot 'checkbox-ui-smoke.nsi'
$compilerLog = Join-Path $runRoot 'makensis.log'
$runLog = Join-Path $runRoot 'run.log'
$expectedContent = "checked-ui-checkbox-$runId"
$roamingCanary = Join-Path $installDir 'roaming/dev.yukinal.workspace/preservation.marker'
$localCanary = Join-Path $installDir 'local/dev.yukinal.workspace/preservation.marker'
$failure = $null
$script:uninstallerProcess = $null
$script:controls = @()
$logLines = [System.Collections.Generic.List[string]]::new()

New-Item -ItemType Directory -Path $runRoot | Out-Null

$nativeWindowSource = @'
using System;
using System.Collections.Generic;
using System.Runtime.InteropServices;
using System.Text;

public sealed class YukinalNativeControl {
    public IntPtr TopLevel { get; set; }
    public IntPtr Handle { get; set; }
    public string ClassName { get; set; }
    public string Text { get; set; }
}

public static class YukinalNativeWindow {
    private delegate bool EnumWindowCallback(IntPtr handle, IntPtr state);

    [DllImport("user32.dll")] private static extern bool EnumWindows(EnumWindowCallback callback, IntPtr state);
    [DllImport("user32.dll")] private static extern bool EnumChildWindows(IntPtr parent, EnumWindowCallback callback, IntPtr state);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetWindowText(IntPtr handle, StringBuilder text, int maxCount);
    [DllImport("user32.dll", CharSet = CharSet.Unicode)] private static extern int GetClassName(IntPtr handle, StringBuilder text, int maxCount);
    [DllImport("user32.dll")] private static extern uint GetWindowThreadProcessId(IntPtr handle, out int processId);
    [DllImport("user32.dll")] public static extern IntPtr SendMessage(IntPtr handle, uint message, IntPtr wParam, IntPtr lParam);
    [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr handle, int command);
    [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr handle);

    private static string ReadText(IntPtr handle) {
        var buffer = new StringBuilder(2048);
        GetWindowText(handle, buffer, buffer.Capacity);
        return buffer.ToString();
    }

    private static string ReadClass(IntPtr handle) {
        var buffer = new StringBuilder(256);
        GetClassName(handle, buffer, buffer.Capacity);
        return buffer.ToString();
    }

    public static YukinalNativeControl[] GetProcessControls(int processId) {
        var controls = new List<YukinalNativeControl>();
        EnumWindows((top, unused) => {
            GetWindowThreadProcessId(top, out int ownerProcessId);
            if (ownerProcessId != processId) return true;
            EnumChildWindows(top, (child, ignored) => {
                controls.Add(new YukinalNativeControl {
                    TopLevel = top,
                    Handle = child,
                    ClassName = ReadClass(child),
                    Text = ReadText(child)
                });
                return true;
            }, IntPtr.Zero);
            return true;
        }, IntPtr.Zero);
        return controls.ToArray();
    }
}
'@

$nsisTemplate = @'
Unicode true
Name "Yukinal NSIS checkbox UI smoke"
OutFile "@@INSTALLER_PATH@@"
InstallDir "@@INSTALL_DIR@@"
RequestExecutionLevel user

!include "MUI2.nsh"
!include "FileFunc.nsh"
!include "LogicLib.nsh"
!include "nsDialogs.nsh"
!include "WinMessages.nsh"
!define BUNDLEID "dev.yukinal.workspace"
!include "@@HOOK_PATH@@"

@@LAYOUT_DEFINE@@
Var DeleteAppDataCheckbox
Var DeleteAppDataCheckboxState
Var UpdateMode

!define MUI_PAGE_CUSTOMFUNCTION_SHOW un.ConfirmShow
@@CONFIRM_SHOW@@
!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave
@@CONFIRM_LEAVE@@
!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES
!insertmacro MUI_LANGUAGE "English"
@@DELETE_DATA_LABEL@@

Section "Install fixture"
  CreateDirectory "$INSTDIR\roaming\${BUNDLEID}"
  CreateDirectory "$INSTDIR\local\${BUNDLEID}"
  FileOpen $0 "$INSTDIR\roaming\${BUNDLEID}\preservation.marker" w
  FileWrite $0 "@@CANARY_CONTENT@@"
  FileClose $0
  FileOpen $0 "$INSTDIR\local\${BUNDLEID}\preservation.marker" w
  FileWrite $0 "@@CANARY_CONTENT@@"
  FileClose $0
  WriteUninstaller "$INSTDIR\uninstall.exe"
SectionEnd

Function un.onInit
  StrCpy $INSTDIR "@@INSTALL_DIR@@"
  StrCpy $DeleteAppDataCheckboxState 0
  StrCpy $UpdateMode 0
  ${GetOptions} $CMDLINE "/UPDATE" $0
  ${IfNot} ${Errors}
    StrCpy $UpdateMode 1
  ${EndIf}
  ; Test-only switch simulates a selected consent bit when NSIS skips the UI.
  ${GetOptions} $CMDLINE "/TESTCHECKED" $0
  ${IfNot} ${Errors}
    StrCpy $DeleteAppDataCheckboxState 1
  ${EndIf}
FunctionEnd

Section Uninstall
  !insertmacro NSIS_HOOK_PREUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
  ${AndIf} $UpdateMode <> 1
    RmDir /r "$INSTDIR\roaming\${BUNDLEID}"
    RmDir /r "$INSTDIR\local\${BUNDLEID}"
  ${EndIf}
  SetAutoClose true
SectionEnd
'@

$nsisSource = $nsisTemplate.Replace('@@INSTALLER_PATH@@', $installerPath)
$nsisSource = $nsisSource.Replace('@@INSTALL_DIR@@', $installDir)
$nsisSource = $nsisSource.Replace('@@HOOK_PATH@@', $hookPath)
$nsisSource = $nsisSource.Replace('@@LAYOUT_DEFINE@@', $layoutDefine)
$nsisSource = $nsisSource.Replace('@@DELETE_DATA_LABEL@@', $checkboxLabel)
$nsisSource = $nsisSource.Replace('@@CONFIRM_SHOW@@', $confirmShow.Value)
$nsisSource = $nsisSource.Replace('@@CONFIRM_LEAVE@@', $confirmLeave.Value)
$nsisSource = $nsisSource.Replace('@@CANARY_CONTENT@@', $expectedContent)
[IO.File]::WriteAllText($sourcePath, $nsisSource, [Text.UTF8Encoding]::new($false))

Add-Type -TypeDefinition $nativeWindowSource -Language CSharp

function Get-ControlsForProcess {
  param([int]$ProcessId)
  return @([YukinalNativeWindow]::GetProcessControls($ProcessId))
}

function Format-Controls {
  param([object[]]$Controls)
  return ($Controls | ForEach-Object { "$($_.ClassName): '$($_.Text)' HWND=$($_.Handle) TOP=$($_.TopLevel)" }) -join "`r`n"
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

function Install-UiFixture {
  $process = Start-Process -FilePath $installerPath -ArgumentList @('/S') -PassThru -WindowStyle Hidden
  if (-not $process.WaitForExit(60000)) { throw 'NSIS UI fixture installer did not finish within 60 seconds.' }
  $process.Refresh()
  if ($process.ExitCode -ne 0) { throw "NSIS UI fixture installer exited with code $($process.ExitCode)." }
  if (-not (Test-Path -LiteralPath $uninstallerPath -PathType Leaf)) { throw 'NSIS UI fixture did not produce its test uninstaller.' }
}

function Invoke-UiUninstallCase {
  param(
    [string]$CaseName,
    [bool]$SelectDeleteCheckbox,
    [bool]$UpdateMode
  )

  $arguments = [System.Collections.Generic.List[string]]::new()
  if ($UpdateMode) { $arguments.Add('/UPDATE') }
  # NSIS requires _?= to be the final, unquoted command-line parameter.
  $arguments.Add("_?=$installDir")
  $script:uninstallerProcess = Start-Process -FilePath $uninstallerPath -ArgumentList $arguments.ToArray() -PassThru -WindowStyle Hidden
  $processId = $script:uninstallerProcess.Id
  $checkbox = $null
  $script:controls = @()
  $deadline = [DateTime]::UtcNow.AddSeconds(20)
  while ([DateTime]::UtcNow -lt $deadline) {
    $script:uninstallerProcess.Refresh()
    if ($script:uninstallerProcess.HasExited) {
      throw "NSIS UI test uninstaller '$CaseName' exited before showing its confirmation page (exit $($script:uninstallerProcess.ExitCode))."
    }
    $script:controls = Get-ControlsForProcess -ProcessId $processId
    $checkbox = $script:controls | Where-Object { $_.ClassName -eq 'Button' -and $_.Text.Trim() -eq 'Delete the application data' } | Select-Object -First 1
    if ($checkbox) { break }
    Start-Sleep -Milliseconds 100
  }
  if (-not $checkbox) { throw "Could not find Tauri's delete-data checkbox in '$CaseName'. Controls:`r`n$(Format-Controls $script:controls)" }

  [YukinalNativeWindow]::ShowWindow($checkbox.TopLevel, 7) | Out-Null # SW_SHOWMINNOACTIVE
  $uncheckedState = [YukinalNativeWindow]::SendMessage($checkbox.Handle, 0x00F0, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64() # BM_GETCHECK
  if ($uncheckedState -ne 0) { throw "Delete-data checkbox must default to unchecked in '$CaseName'; native state was $uncheckedState." }
  $logLines.Add("${CaseName}: confirmation checkbox default unchecked.")

  if ($SelectDeleteCheckbox) {
    [YukinalNativeWindow]::SendMessage($checkbox.Handle, 0x00F1, [IntPtr]::new(1), [IntPtr]::Zero) | Out-Null # BM_SETCHECK
    $checkedState = [YukinalNativeWindow]::SendMessage($checkbox.Handle, 0x00F0, [IntPtr]::Zero, [IntPtr]::Zero).ToInt64()
    if ($checkedState -ne 1) { throw "Could not select the delete-data checkbox in '$CaseName'; native state was $checkedState." }
    $logLines.Add("${CaseName}: selected the delete-data checkbox.")
  }

  $script:controls = Get-ControlsForProcess -ProcessId $processId
  $uninstallButton = $script:controls | Where-Object {
    $_.ClassName -eq 'Button' -and ($_.Text -replace '&', '').Trim() -match '(?i)^uninstall$'
  } | Select-Object -First 1
  if (-not $uninstallButton) { throw "Could not find the Uninstall button in '$CaseName'. Controls:`r`n$(Format-Controls $script:controls)" }
  [YukinalNativeWindow]::SendMessage($uninstallButton.Handle, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null # BM_CLICK

  if ($SelectDeleteCheckbox) {
    $messageText = 'Yukinal keeps your user data when uninstalling.'
    $messageControl = $null
    $deadline = [DateTime]::UtcNow.AddSeconds(20)
    while ([DateTime]::UtcNow -lt $deadline) {
      $script:uninstallerProcess.Refresh()
      if ($script:uninstallerProcess.HasExited) { break }
      $script:controls = Get-ControlsForProcess -ProcessId $processId
      $messageControl = $script:controls | Where-Object {
        $_.ClassName -eq 'Static' -and $_.Text.Contains($messageText, [StringComparison]::OrdinalIgnoreCase)
      } | Select-Object -First 1
      if ($messageControl) { break }
      Start-Sleep -Milliseconds 100
    }
    if ($messageControl) {
      $okButton = $script:controls | Where-Object {
        $_.TopLevel -eq $messageControl.TopLevel -and $_.ClassName -eq 'Button' -and
        ($_.Text -replace '&', '').Trim() -match '^(?i:OK|确定)$'
      } | Select-Object -First 1
      if (-not $okButton) { throw "Could not find the preservation notice's OK button in '$CaseName'. Controls:`r`n$(Format-Controls $script:controls)" }
      [YukinalNativeWindow]::SendMessage($okButton.Handle, 0x00F5, [IntPtr]::Zero, [IntPtr]::Zero) | Out-Null # BM_CLICK
      $logLines.Add("${CaseName}: checked-data preservation notice shown and dismissed.")
    }
  }

  if (-not $script:uninstallerProcess.WaitForExit(60000)) { throw "NSIS UI test uninstaller '$CaseName' did not finish within 60 seconds." }
  $script:uninstallerProcess.Refresh()
  if ($script:uninstallerProcess.ExitCode -ne 0) { throw "NSIS UI test uninstaller '$CaseName' exited with code $($script:uninstallerProcess.ExitCode)." }

  foreach ($canary in @($roamingCanary, $localCanary)) {
    if (-not (Test-Path -LiteralPath $canary -PathType Leaf)) { throw "NSIS deleted or changed a user-data canary in '$CaseName': $canary" }
    if ([IO.File]::ReadAllText($canary) -ne $expectedContent) { throw "NSIS deleted or changed a user-data canary in '$CaseName': $canary" }
  }
  if ($SelectDeleteCheckbox -and -not $messageControl) {
    throw "The checked-data preservation notice did not appear in '$CaseName'. Controls:`r`n$(Format-Controls $script:controls)"
  }
  $logLines.Add("${CaseName}: both isolated user-data canaries preserved.")
  $script:uninstallerProcess = $null
}

function Invoke-SilentUninstallCase {
  $script:uninstallerProcess = Start-Process -FilePath $uninstallerPath -ArgumentList @('/S', '/TESTCHECKED', "_?=$installDir") -PassThru -WindowStyle Hidden
  if (-not $script:uninstallerProcess.WaitForExit(60000)) { throw 'NSIS silent checked-state test did not finish within 60 seconds.' }
  $script:uninstallerProcess.Refresh()
  if ($script:uninstallerProcess.ExitCode -ne 0) { throw "NSIS silent checked-state test exited with code $($script:uninstallerProcess.ExitCode)." }

  foreach ($canary in @($roamingCanary, $localCanary)) {
    if (-not (Test-Path -LiteralPath $canary -PathType Leaf)) { throw "NSIS deleted or changed a user-data canary in 'silent-checked': $canary" }
    if ([IO.File]::ReadAllText($canary) -ne $expectedContent) { throw "NSIS deleted or changed a user-data canary in 'silent-checked': $canary" }
  }
  $logLines.Add('silent-checked: both isolated user-data canaries preserved after /S completed.')
  $script:uninstallerProcess = $null
}

try {
  $compilerOutput = & $MakeNSISPath /V2 $sourcePath 2>&1
  [IO.File]::WriteAllLines($compilerLog, [string[]]@($compilerOutput | ForEach-Object { $_.ToString() }))
  if ($LASTEXITCODE -ne 0) { throw "makensis.exe failed with exit code $LASTEXITCODE. See $compilerLog" }
  if (-not (Test-Path -LiteralPath $installerPath -PathType Leaf)) { throw 'makensis.exe reported success but did not produce the UI smoke executable.' }

  foreach ($case in @(
    @{ Name = 'unchecked'; SelectDeleteCheckbox = $false; UpdateMode = $false },
    @{ Name = 'checked'; SelectDeleteCheckbox = $true; UpdateMode = $false },
    @{ Name = 'update-checked'; SelectDeleteCheckbox = $true; UpdateMode = $true }
  )) {
    Install-UiFixture
    Invoke-UiUninstallCase -CaseName $case.Name -SelectDeleteCheckbox $case.SelectDeleteCheckbox -UpdateMode $case.UpdateMode
  }
  Install-UiFixture
  Invoke-SilentUninstallCase
  [IO.File]::WriteAllLines($runLog, [string[]]$logLines)
} catch {
  $failure = $_.Exception.Message
  $logLines.Add("FAIL: $failure")
  if ($script:controls) { $logLines.Add("Native controls at failure:`r`n$(Format-Controls $script:controls)") }
  if (-not (Test-Path -LiteralPath $runLog)) { [IO.File]::WriteAllLines($runLog, [string[]]$logLines) }
  else { [IO.File]::AppendAllLines($runLog, [string[]]$logLines) }
}

try { Stop-ExactProcess -Process $script:uninstallerProcess -ExpectedPath $uninstallerPath }
catch {
  if ($failure) { $failure += "; cleanup also failed: $($_.Exception.Message)" }
  else { $failure = "Cleanup failed: $($_.Exception.Message)" }
}

if ($failure) {
  Write-Output "NSIS checkbox UI smoke failed. Diagnostics preserved at: $runRoot"
  throw $failure
}

$resolvedRunRoot = [IO.Path]::GetFullPath($runRoot)
if (-not $resolvedRunRoot.StartsWith($runPrefix, [StringComparison]::OrdinalIgnoreCase)) {
  throw "Refusing to clean an NSIS checkbox smoke directory outside $smokeRoot"
}
Remove-Item -LiteralPath $resolvedRunRoot -Recurse -Force
Write-Output 'Windows NSIS checkbox UI smoke: green (unchecked, checked, update, and silent checked-state paths preserve data; all data isolated).'
