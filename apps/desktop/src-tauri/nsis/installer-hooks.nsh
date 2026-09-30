!macro NSIS_HOOK_PREUNINSTALL
  ${If} $DeleteAppDataCheckboxState = 1
    IfSilent yukinal_keep_data_silently
    MessageBox MB_OK|MB_ICONINFORMATION "Yukinal keeps your user data when uninstalling. To delete it, uninstall first, then remove $APPDATA\${BUNDLEID} and $LOCALAPPDATA\${BUNDLEID} manually. / 卸载 Yukinal 时会保留用户数据；如需删除，请先卸载，再手动删除 $APPDATA\${BUNDLEID} 和 $LOCALAPPDATA\${BUNDLEID}。"
  yukinal_keep_data_silently:
  ${EndIf}

  ; Tauri's generated uninstaller deletes AppData when this checkbox state is 1.
  ; Never let uninstall (interactive, silent, or update-triggered) remove user data.
  StrCpy $DeleteAppDataCheckboxState 0
!macroend
