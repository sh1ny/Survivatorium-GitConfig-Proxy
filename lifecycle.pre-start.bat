@echo off
setlocal
REM CFTools Architect Lifecycle Hook - pre-start (BLOCKING)
REM This script executes before each game server start.
REM It ensures Survivatorium GitConfig Proxy is running exactly once.

echo [HOOK] Server Root: %CF_SERVER_ROOT%
echo [HOOK] Server ID: %CF_SERVER_ID%
echo [HOOK] Game Title: %CF_TITLE%

REM Architect runs hooks as SYSTEM. Use absolute paths.
REM Keep the proxy outside lifecycle_hooks if preferred.
set "PROXY_START_SCRIPT=C:\Architect\Services\Survivatorium-GitConfig-Proxy\start_proxy.bat"

if not exist "%PROXY_START_SCRIPT%" (
  echo [HOOK][ERROR] Missing proxy start script: "%PROXY_START_SCRIPT%"
  exit /b 1
)

call "%PROXY_START_SCRIPT%"
if not %ERRORLEVEL%==0 (
  echo [HOOK][ERROR] Proxy bootstrap failed with exit code %ERRORLEVEL%.
  exit /b 1
)

echo [HOOK][OK] Proxy check complete.
exit /b 0
