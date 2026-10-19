Unicode true
RequestExecutionLevel user
SetCompressor /SOLID lzma

!include "MUI2.nsh"

!ifndef APP_VERSION
  !error "APP_VERSION is required"
!endif
!ifndef PAYLOAD_DIR
  !error "PAYLOAD_DIR is required"
!endif
!ifndef OUT_FILE
  !error "OUT_FILE is required"
!endif
!ifndef ICON_FILE
  !error "ICON_FILE is required"
!endif

Name "Yukinal ${APP_VERSION}"
OutFile "${OUT_FILE}"
InstallDir "$LOCALAPPDATA\Programs\Yukinal"
InstallDirRegKey HKCU "Software\Yukinal" "InstallDir"
ShowInstDetails show
ShowUninstDetails show

!define PRODUCT_KEY "Software\Microsoft\Windows\CurrentVersion\Uninstall\Yukinal"
!define APP_KEY "Software\Yukinal"

Icon "${ICON_FILE}"
UninstallIcon "${ICON_FILE}"

!define MUI_ABORTWARNING
!define MUI_ICON "${ICON_FILE}"
!define MUI_UNICON "${ICON_FILE}"
!define MUI_FINISHPAGE_RUN "$INSTDIR\yukinal.cmd"
!define MUI_FINISHPAGE_RUN_TEXT "Launch Yukinal"

!insertmacro MUI_PAGE_WELCOME
!insertmacro MUI_PAGE_LICENSE "${PAYLOAD_DIR}\LICENSE"
!insertmacro MUI_PAGE_DIRECTORY
!insertmacro MUI_PAGE_INSTFILES
!insertmacro MUI_PAGE_FINISH

!insertmacro MUI_UNPAGE_CONFIRM
!insertmacro MUI_UNPAGE_INSTFILES

!insertmacro MUI_LANGUAGE "English"

Function .onInit
  SetShellVarContext current
FunctionEnd

Function un.onInit
  SetShellVarContext current
FunctionEnd

Section "Yukinal" SecMain
  SectionIn RO
  SetOutPath "$INSTDIR"
  File /r "${PAYLOAD_DIR}\*.*"

  CreateDirectory "$SMPROGRAMS\Yukinal"
  CreateShortCut "$SMPROGRAMS\Yukinal\Yukinal.lnk" "$INSTDIR\yukinal.cmd" "" "$INSTDIR\assets\yukinal.ico"
  CreateShortCut "$DESKTOP\Yukinal.lnk" "$INSTDIR\yukinal.cmd" "" "$INSTDIR\assets\yukinal.ico"
  WriteUninstaller "$INSTDIR\Uninstall.exe"

  WriteRegStr HKCU "${APP_KEY}" "InstallDir" "$INSTDIR"
  WriteRegStr HKCU "${PRODUCT_KEY}" "DisplayName" "Yukinal ${APP_VERSION}"
  WriteRegStr HKCU "${PRODUCT_KEY}" "DisplayVersion" "${APP_VERSION}"
  WriteRegStr HKCU "${PRODUCT_KEY}" "Publisher" "Yukinal Project"
  WriteRegStr HKCU "${PRODUCT_KEY}" "InstallLocation" "$INSTDIR"
  WriteRegStr HKCU "${PRODUCT_KEY}" "DisplayIcon" "$INSTDIR\yukinal-desktop-host.exe"
  WriteRegStr HKCU "${PRODUCT_KEY}" "UninstallString" '$"$INSTDIR\Uninstall.exe$"'
  WriteRegDWORD HKCU "${PRODUCT_KEY}" "NoModify" 1
  WriteRegDWORD HKCU "${PRODUCT_KEY}" "NoRepair" 1
SectionEnd

Section "Uninstall"
  Delete "$SMPROGRAMS\Yukinal\Yukinal.lnk"
  RMDir "$SMPROGRAMS\Yukinal"
  Delete "$DESKTOP\Yukinal.lnk"
  DeleteRegKey HKCU "${PRODUCT_KEY}"
  DeleteRegValue HKCU "${APP_KEY}" "InstallDir"
  DeleteRegKey /ifempty HKCU "${APP_KEY}"
  RMDir /r "$INSTDIR"
SectionEnd
