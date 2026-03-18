@echo off
REM ============================================================
REM  Survivatorium GitConfig Proxy — Startup Script
REM  Place this .bat next to svc-gitconfig-proxy.exe
REM ============================================================

REM Option 1: Set your token here (recommended)
set GITHUB_TOKEN=PASTE_YOUR_GITHUB_PAT_HERE

REM Required for /write endpoint — set to your DayZ server paths
REM These enable the proxy to write large/binary files directly to disk
set SVC_PROFILE_PATH=C:\DayZServer\profiles
set SVC_MISSION_PATH=C:\DayZServer\mpmissions\dayzOffline.chernarusplus

REM Allowed client IPs (comma-separated). Default: 127.0.0.1 (localhost only)
REM Add additional IPs if DayZ runs on a different machine. Use 0.0.0.0 to allow all.
set SVC_ALLOWED_IPS=127.0.0.1

REM Optional: TLS (HTTPS) support. Uncomment and set paths to enable.
REM See README.md for how to generate certificates.
REM set SVC_TLS_CERT=proxy-cert.pem
REM set SVC_TLS_KEY=proxy-key.pem

svc-gitconfig-proxy.exe --bind 127.0.0.1 --port 8470
pause
