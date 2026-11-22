@echo off
title EFB Browser Companion
cd /d "%~dp0"
set "EXE=%~dp0efb-browser-companion.exe"
if not exist "%EXE%" set "EXE=%~dp0target\release\efb-browser-companion.exe"
if not exist "%EXE%" (
  where cargo >nul 2>nul || (echo Rust is required to build the companion: https://rustup.rs & pause & exit /b 1)
  cargo build --release || (pause & exit /b 1)
)
"%EXE%" %*
pause
