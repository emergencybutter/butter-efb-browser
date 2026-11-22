@echo off
title EFB Browser Companion
cd /d "%~dp0"
where node >nul 2>nul || (echo Node.js is required: https://nodejs.org & pause & exit /b 1)
if not exist node_modules (call npm install --omit=dev || (pause & exit /b 1))
node src\server.js %*
pause
