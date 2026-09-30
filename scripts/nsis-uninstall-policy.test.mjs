import assert from "node:assert/strict";
import { spawnSync } from "node:child_process";
import { mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import test from "node:test";

const root = fileURLToPath(new URL("..", import.meta.url));
const configPath = path.join(root, "apps", "desktop", "src-tauri", "tauri.conf.json");

test("NSIS uninstall always preserves app data under the product contract", () => {
  const config = JSON.parse(readFileSync(configPath, "utf8"));
  const configuredHook = config.bundle?.windows?.nsis?.installerHooks;
  assert.equal(
    typeof configuredHook,
    "string",
    "Windows NSIS must configure the user-data preservation hook",
  );
  const hookPath = path.resolve(path.dirname(configPath), configuredHook);
  const hookSource = readFileSync(hookPath, "utf8");
  const macro = hookSource.slice(hookSource.indexOf("!macro NSIS_HOOK_PREUNINSTALL"), hookSource.indexOf("!macroend"));
  assert.match(macro, /\$DeleteAppDataCheckboxState\s*=\s*1/);
  assert.match(macro, /IfSilent\s+yukinal_keep_data_silently/);
  assert.match(macro, /MessageBox\s+MB_OK\|MB_ICONINFORMATION\s+"[^"]*"/);
  assert.match(macro, /yukinal_keep_data_silently:/);
  assert.match(macro, /StrCpy\s+\$DeleteAppDataCheckboxState\s+0/);
  assert.doesNotMatch(macro, /RmDir\s+\/r/i, "the preservation hook must never recursively delete user data");

  const smokeScript = readFileSync(path.join(root, "scripts", "smoke-nsis-checkbox-ui-windows.ps1"), "utf8");
  assert.match(smokeScript, /default unchecked/);
  assert.match(smokeScript, /selected the delete-data checkbox/);
  assert.match(smokeScript, /NSIS deleted or changed a user-data canary/);
  assert.match(smokeScript, /Yukinal keeps your user data when uninstalling/);
  assert.match(smokeScript, /!include "@@HOOK_PATH@@"/);
  assert.match(smokeScript, /!insertmacro NSIS_HOOK_PREUNINSTALL/);
  assert.match(smokeScript, /UpdateMode/);
});

test("generated NSIS check rejects unguarded deletion or erased checkbox consent", () => {
  const checkerPath = path.join(root, "scripts", "check-nsis-uninstall.mjs");
  const temporaryDirectory = mkdtempSync(path.join(tmpdir(), "yukinal-nsis-uninstall-policy-"));
  const fixturePath = path.join(temporaryDirectory, "installer.nsi");

  const runCheck = ({
    localDeleteInsideGuard = true,
    captureCheckboxState = true,
    extraUnconditionalDataDelete = false,
    duplicateCheckboxReset = false,
  } = {}) => {
    const localDelete = '  RmDir /r "$LOCALAPPDATA\\${BUNDLEID}"';
    const statements = [
      '!include "installer-hooks.nsh"',
      "!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave",
      "!insertmacro MUI_UNPAGE_CONFIRM",
      "Function un.ConfirmLeave",
      ...(captureCheckboxState
        ? ["  SendMessage $DeleteAppDataCheckbox ${BM_GETCHECK} 0 0 $DeleteAppDataCheckboxState"]
        : []),
      "FunctionEnd",
      "Section Uninstall",
      "  !insertmacro NSIS_HOOK_PREUNINSTALL",
      ...(duplicateCheckboxReset ? ["  StrCpy $DeleteAppDataCheckboxState 0"] : []),
      "  ${If} $DeleteAppDataCheckboxState = 1",
      "  ${AndIf} $UpdateMode <> 1",
      '    RmDir /r "$APPDATA\\${BUNDLEID}"',
      ...(localDeleteInsideGuard
        ? [localDelete, "  ${EndIf}", ...(extraUnconditionalDataDelete ? [localDelete] : [])]
        : ["  ${EndIf}", localDelete]),
      "SectionEnd",
    ].join("\n");
    writeFileSync(fixturePath, statements);
    return spawnSync(process.execPath, [checkerPath, fixturePath], { encoding: "utf8" });
  };

  try {
    const safeResult = runCheck();
    assert.equal(safeResult.status, 0, safeResult.stderr || safeResult.stdout);

    const unsafeResult = runCheck({ localDeleteInsideGuard: false });
    assert.notEqual(unsafeResult.status, 0, "unconditional LocalAppData deletion must fail the generated-script check");
    assert.match(unsafeResult.stderr, /LocalAppData removal is outside the user-consent guard/);

    const missingCheckboxCapture = runCheck({ captureCheckboxState: false });
    assert.notEqual(
      missingCheckboxCapture.status,
      0,
      "the generated-script check must reject an uninstall page that never captures the selected deletion checkbox",
    );
    assert.match(missingCheckboxCapture.stderr, /confirm page must capture the delete-checkbox state/);

    const extraDeleteOutsideGuard = runCheck({ extraUnconditionalDataDelete: true });
    assert.notEqual(
      extraDeleteOutsideGuard.status,
      0,
      "the generated-script check must reject any additional recursive user-data deletion outside the guard",
    );
    assert.match(extraDeleteOutsideGuard.stderr, /expected exactly two guarded user-data removals/);

    const duplicateReset = runCheck({ duplicateCheckboxReset: true });
    assert.notEqual(duplicateReset.status, 0, "extra clearing of checkbox state must fail the generated-script check");
    assert.match(duplicateReset.stderr, /selected checkbox state may only be reset by the verified preservation hook/);
  } finally {
    rmSync(temporaryDirectory, { recursive: true, force: true });
  }
});

test("Windows packaged smoke isolates and restores the WebView2 user-data folder", () => {
  const smokeScript = readFileSync(path.join(root, "scripts", "smoke-installed-windows.ps1"), "utf8");

  assert.match(smokeScript, /\$webViewDataDir\s*=\s*Join-Path \$dataDir 'webview2'/);
  assert.match(smokeScript, /\$env:WEBVIEW2_USER_DATA_FOLDER\s*=\s*\$webViewDataDir/);
  assert.match(smokeScript, /\$oldWebViewDataDir\s*=\s*\[Environment\]::GetEnvironmentVariable\('WEBVIEW2_USER_DATA_FOLDER', 'Process'\)/);
  assert.match(smokeScript, /\[Environment\]::SetEnvironmentVariable\('WEBVIEW2_USER_DATA_FOLDER', \$oldWebViewDataDir, 'Process'\)/);
  assert.match(smokeScript, /\$webViewDataIsolationVerified\s*=\s*Test-Path -LiteralPath \$webViewDataDir/);
  assert.match(smokeScript, /-or -not \$webViewDataIsolationVerified/);
});

test("native window smoke isolates app data and removes only its empty Tauri profile root", () => {
  const smokeScript = readFileSync(path.join(root, "scripts", "check-desktop-window.ps1"), "utf8");

  assert.match(smokeScript, /\$dataDirectory\s*=\s*Join-Path \(\[System\.IO\.Path\]::GetTempPath\(\)\)/);
  assert.match(smokeScript, /Refusing to run the packaged UI smoke while another Yukinal process is using the user profile/);
  assert.match(smokeScript, /\$env:YUKINAL_DATA_DIR\s*=\s*\$dataDirectory/);
  assert.match(smokeScript, /\$webViewDirectory\s*=\s*Join-Path \$dataDirectory "webview2"/);
  assert.match(smokeScript, /\$env:WEBVIEW2_USER_DATA_FOLDER\s*=\s*\$webViewDirectory/);
  assert.match(smokeScript, /Test-Path -LiteralPath \$webViewDirectory/);
  assert.match(smokeScript, /\$defaultAppDataExistedBefore\s*=\s*Test-Path -LiteralPath \$defaultAppDataDirectory/);
  assert.match(smokeScript, /Tauri's default LocalAppData profile/);
  assert.match(smokeScript, /\$profileContents\.Count -eq 0/);
  assert.match(smokeScript, /Remove-Item -LiteralPath \$defaultAppDataDirectory/);
  assert.match(smokeScript, /-not \$defaultAppDataExistedBefore -and \(Test-Path -LiteralPath \$defaultAppDataDirectory/);
  assert.match(smokeScript, /\$processStillRunning -or \$otherRunningYukinal\.Count -gt 0/);
  assert.match(smokeScript, /\$env:YUKINAL_DATA_DIR\s*=\s*\$previousDataDirectory/);
  assert.match(smokeScript, /\$env:WEBVIEW2_USER_DATA_FOLDER\s*=\s*\$oldWebViewDirectory/);
});

test("NSIS lifecycle smoke keeps installs isolated and refuses an existing default install", () => {
  const smokeScript = readFileSync(path.join(root, "scripts", "smoke-nsis-install-windows.ps1"), "utf8");

  assert.match(smokeScript, /\$defaultInstallDir\s*=.*\$env:LOCALAPPDATA.*productName/is);
  assert.match(smokeScript, /Test-Path -LiteralPath \$defaultInstallDir/);
  assert.match(smokeScript, /-ArgumentList\s+@\(\s*'\/S'\s*,\s*"\/D=\$installDir"\s*\)/);
  assert.match(
    smokeScript,
    /-ArgumentList\s+@\(\s*'\/S'\s*,\s*'\/UPDATE'\s*,\s*"\/D=\$installDir"\s*\)/,
    "the lifecycle smoke must run an in-place update using the generated NSIS update path",
  );
  assert.match(smokeScript, /Remove-Item -LiteralPath \$updateProbePath -Force[\s\S]*?Test-Path -LiteralPath \$updateProbePath -PathType Leaf/);
  assert.match(smokeScript, /UserDataPreservedAcrossUpdate\s*=\s*\$updateCanariesPreserved/);
  assert.match(smokeScript, /-ArgumentList\s+@\(\s*'\/S'\s*,\s*"_\?=\$installDir"\s*\)/);
  assert.match(smokeScript, /\[string\]::Equals\(\$registeredInstallDir, \$installDir,/i);
  assert.match(smokeScript, /\$remainingInstallEntries\s*=\s*@\(Get-ChildItem -LiteralPath \$installDir -Force\)/);
  assert.match(smokeScript, /\$unexpectedInstallEntries\s*=.*\$_\.FullName -ne \$uninstallerPath/s);
  assert.match(smokeScript, /Remove-Item -LiteralPath \$uninstallerPath -Force/);
});

test("NSIS checkbox UI smoke extracts generated callbacks and confines canaries to its isolated fixture", () => {
  const smokeScript = readFileSync(path.join(root, "scripts", "smoke-nsis-checkbox-ui-windows.ps1"), "utf8");

  assert.match(smokeScript, /target\/release\/nsis\/x64\/installer\.nsi/);
  assert.match(smokeScript, /Function un\\\.ConfirmShow/);
  assert.match(smokeScript, /Function un\\\.ConfirmLeave/);
  assert.match(smokeScript, /LangString deleteAppData/);
  assert.match(smokeScript, /0x00F1[\s\S]*BM_SETCHECK/);
  assert.match(smokeScript, /0x00F0[\s\S]*BM_GETCHECK/);
  assert.match(smokeScript, /0x00F5[\s\S]*BM_CLICK/);
  assert.match(smokeScript, /NSIS deleted or changed a user-data canary/);
  assert.match(smokeScript, /Yukinal keeps your user data when uninstalling/);
  assert.match(smokeScript, /\$roamingCanary\s*=\s*Join-Path \$installDir/);
  assert.match(smokeScript, /\$localCanary\s*=\s*Join-Path \$installDir/);
  assert.match(smokeScript, /_\?=\$installDir/);
});

test("NSIS checkbox UI smoke preserves canaries during silent uninstall with checked state", () => {
  const smokeScript = readFileSync(path.join(root, "scripts", "smoke-nsis-checkbox-ui-windows.ps1"), "utf8");

  assert.match(smokeScript, /GetOptions\}\s+\$CMDLINE "\/TESTCHECKED"[\s\S]*StrCpy \$DeleteAppDataCheckboxState 1/);
  assert.match(smokeScript, /function Invoke-SilentUninstallCase/);
  assert.match(smokeScript, /-ArgumentList\s+@\(\s*'\/S'\s*,\s*'\/TESTCHECKED'\s*,\s*"_\?=\$installDir"\s*\)/);
  assert.match(smokeScript, /^\s*Invoke-SilentUninstallCase\s*$/m);
  assert.match(smokeScript, /silent-checked:[\s\S]*both isolated user-data canaries preserved/);
});
