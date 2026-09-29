#!/usr/bin/env node

import assert from "node:assert/strict";
import { existsSync, readFileSync } from "node:fs";
import path from "node:path";
import { fileURLToPath } from "node:url";

const root = fileURLToPath(new URL("..", import.meta.url));
const nsisScriptPath = path.resolve(root, process.argv[2] ?? "target/release/nsis/x64/installer.nsi");
if (!existsSync(nsisScriptPath)) {
  console.error(`✗ generated NSIS script is missing: ${nsisScriptPath}`);
  process.exit(1);
}

const source = readFileSync(nsisScriptPath, "utf8");
const configPath = path.join(root, "apps/desktop/src-tauri/tauri.conf.json");
const config = JSON.parse(readFileSync(configPath, "utf8"));
const hookPath = config.bundle?.windows?.nsis?.installerHooks;
const hookFileName = typeof hookPath === "string" ? path.basename(hookPath) : "";
const hookSource = hookFileName.length > 0
  ? readFileSync(path.resolve(path.dirname(configPath), hookPath), "utf8")
  : "";
const hookMacroStart = hookSource.indexOf("!macro NSIS_HOOK_PREUNINSTALL");
const hookMacroEnd = hookSource.indexOf("!macroend", hookMacroStart);
const hookMacro = hookMacroStart >= 0 && hookMacroEnd > hookMacroStart
  ? hookSource.slice(hookMacroStart, hookMacroEnd)
  : "";
const confirmCallback = source.indexOf("!define MUI_PAGE_CUSTOMFUNCTION_LEAVE un.ConfirmLeave");
const confirmPage = source.indexOf("!insertmacro MUI_UNPAGE_CONFIRM", confirmCallback);
const confirmLeaveStart = source.indexOf("Function un.ConfirmLeave");
const confirmLeaveEnd = source.indexOf("FunctionEnd", confirmLeaveStart);
const checkboxCapture = source.indexOf(
  "SendMessage $DeleteAppDataCheckbox ${BM_GETCHECK} 0 0 $DeleteAppDataCheckboxState",
  confirmLeaveStart,
);
const section = source.indexOf("Section Uninstall");
const sectionEnd = source.indexOf("SectionEnd", section);
const hookInsert = source.indexOf("!insertmacro NSIS_HOOK_PREUNINSTALL", section);
const appDataDelete = source.indexOf('RmDir /r "$APPDATA\\${BUNDLEID}"', section);
const localDataDelete = source.indexOf('RmDir /r "$LOCALAPPDATA\\${BUNDLEID}"', section);
const deleteGuard = source.lastIndexOf("${If} $DeleteAppDataCheckboxState = 1", appDataDelete);
const deleteGuardEnd = source.indexOf("${EndIf}", appDataDelete);
const deletePolicy = source.slice(deleteGuard, deleteGuardEnd);
const stateCaptureToUninstall = source.slice(checkboxCapture, sectionEnd);
const recursiveUserDataDeletes = [
  ...source.matchAll(/^\s*RmDir\s+\/r\s+"\$(?:APPDATA|LOCALAPPDATA)\\\$\{BUNDLEID\}(?:\\[^"]*)?"\s*$/gim),
];

try {
  assert.ok(hookFileName.length > 0, "tauri.conf.json must configure an NSIS hook for app-data preservation");
  assert.ok(existsSync(path.resolve(path.dirname(configPath), hookPath)), `configured NSIS hook does not exist: ${hookPath}`);
  assert.ok(source.includes(hookFileName), `generated NSIS script does not include ${hookFileName}`);
  assert.ok(confirmCallback >= 0, "generated uninstaller must bind a leave callback to the delete-data confirmation page");
  assert.ok(confirmPage > confirmCallback && confirmPage < section, "delete-data confirmation page must precede the uninstall section");
  assert.ok(confirmLeaveStart > confirmCallback && confirmLeaveEnd > confirmLeaveStart, "delete-data confirmation callback is missing");
  assert.ok(
    checkboxCapture > confirmLeaveStart && checkboxCapture < confirmLeaveEnd,
    "confirm page must capture the delete-checkbox state before uninstall begins",
  );
  assert.ok(section >= 0 && sectionEnd > section && hookInsert > section, "pre-uninstall safety hook is missing from the uninstaller section");
  assert.ok(appDataDelete > hookInsert && localDataDelete > hookInsert, "expected app-data removals must follow the safety hook");
  assert.ok(deleteGuard > hookInsert && deleteGuard < appDataDelete, "app-data removal is no longer guarded by the captured checkbox state");
  assert.ok(deleteGuardEnd > localDataDelete && deleteGuardEnd < sectionEnd, "LocalAppData removal is outside the user-consent guard");
  assert.equal(
    recursiveUserDataDeletes.length,
    2,
    "expected exactly two guarded user-data removals (Roaming and LocalAppData); review any additional recursive removal",
  );
  for (const deletion of recursiveUserDataDeletes) {
    assert.ok(
      deletion.index > hookInsert && deletion.index > deleteGuard && deletion.index < deleteGuardEnd,
      "every recursive user-data removal must follow the preservation hook and remain inside Tauri's guard",
    );
  }
  assert.ok(deletePolicy.includes("${AndIf} $UpdateMode <> 1"), "app-data removal must also be disabled during app updates");
  assert.ok(hookMacro.length > 0, "configured NSIS uninstall hook does not define NSIS_HOOK_PREUNINSTALL");
  assert.ok(hookMacro.includes("$DeleteAppDataCheckboxState = 1"), "checked deletion checkbox must trigger a preservation notice");
  assert.ok(hookMacro.includes("IfSilent yukinal_keep_data_silently"), "silent uninstall must skip the interactive preservation notice");
  assert.ok(hookMacro.includes("MessageBox MB_OK|MB_ICONINFORMATION"), "interactive uninstall must explain that data is preserved");
  const hookLastConditionalEnd = hookMacro.lastIndexOf("${EndIf}");
  const hookFlagReset = hookMacro.indexOf("StrCpy $DeleteAppDataCheckboxState 0");
  assert.ok(hookFlagReset > hookLastConditionalEnd, "uninstall hook must unconditionally disable Tauri's app-data deletion");
  assert.ok(!/RmDir\s+\/r/i.test(hookMacro), "uninstall hook must not recursively remove user data");
  assert.ok(
    !/StrCpy\s+\$DeleteAppDataCheckboxState\s+0\b/i.test(stateCaptureToUninstall),
    "the selected checkbox state may only be reset by the verified preservation hook",
  );
} catch (error) {
  console.error(`✗ generated NSIS uninstall policy check failed: ${error.message}`);
  process.exit(1);
}

console.log(`✓ generated NSIS uninstall policy: ${path.relative(root, nsisScriptPath)}`);
