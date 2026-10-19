@echo off
setlocal
title Yukinal
powershell.exe -NoLogo -NoProfile -File "%~dp0start-desktop-preview.ps1" %*
if errorlevel 1 (
  echo.
  echo Yukinal could not start. See the message above.
  pause
  exit /b 1
)
exit /b 0
