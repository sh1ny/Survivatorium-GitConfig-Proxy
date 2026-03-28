@echo off
setlocal
REM ============================================================
REM  Survivatorium GitConfig Proxy — Startup Script
REM  Place this .bat next to svc-gitconfig-proxy.exe
REM ============================================================

REM If you use this from Architect lifecycle_hooks and the .exe is elsewhere,
REM set an absolute path here (example: C:\DayZServer\tools\gitconfig-proxy)
set "PROXY_DIR=%~dp0"
set "PROXY_EXE=%PROXY_DIR%svc-gitconfig-proxy.exe"
set "PROXY_BIND=127.0.0.1"
set "PROXY_PORT=8470"

REM Option 1: Set your token here (recommended)
set "GITHUB_TOKEN=PASTE_YOUR_GITHUB_PAT_HERE"

REM Required for /write endpoint — set to your DayZ server paths
REM These enable the proxy to write large/binary files directly to disk
set "SVC_PROFILE_PATH=C:\DayZServer\profiles"
set "SVC_MISSION_PATH=C:\DayZServer\mpmissions\dayzOffline.chernarusplus"

REM Allowed client IPs (comma-separated). Default: 127.0.0.1 (localhost only)
REM Add additional IPs if DayZ runs on a different machine. Use 0.0.0.0 to allow all.
set "SVC_ALLOWED_IPS=127.0.0.1"

REM Optional: TLS (HTTPS) support. Uncomment and set paths to enable.
REM See README.md for how to generate certificates.
REM set SVC_TLS_CERT=proxy-cert.pem
REM set SVC_TLS_KEY=proxy-key.pem

if not exist "%PROXY_EXE%" (
	echo [ERROR] Proxy executable not found: "%PROXY_EXE%"
	exit /b 1
)

tasklist /FI "IMAGENAME eq svc-gitconfig-proxy.exe" | find /I "svc-gitconfig-proxy.exe" >nul
if %ERRORLEVEL%==0 (
	echo [INFO] Proxy already running. Skipping start.
	exit /b 0
)

echo [INFO] Starting proxy from "%PROXY_EXE%"...
start "Survivatorium GitConfig Proxy" /MIN "%PROXY_EXE%" --bind %PROXY_BIND% --port %PROXY_PORT%

timeout /t 2 /nobreak >nul
tasklist /FI "IMAGENAME eq svc-gitconfig-proxy.exe" | find /I "svc-gitconfig-proxy.exe" >nul
if not %ERRORLEVEL%==0 (
	echo [ERROR] Proxy failed to start.
	exit /b 1
)

echo [OK] Proxy is running.
exit /b 0
