# CFTools Architect Lifecycle Hook - pre-start (BLOCKING)
# This script executes before each game server start.
# It ensures Survivatorium GitConfig Proxy is running exactly once.

Write-Host "[HOOK] Server Root: $env:CF_SERVER_ROOT"
Write-Host "[HOOK] Server ID: $env:CF_SERVER_ID"
Write-Host "[HOOK] Game Title: $env:CF_TITLE"

# Architect runs hooks as SYSTEM. Use absolute paths.
# Keep the proxy outside lifecycle_hooks if preferred.
$proxyDir = "C:\Architect\Services\Survivatorium-GitConfig-Proxy"
$proxyExe = Join-Path $proxyDir "svc-gitconfig-proxy.exe"
$bind = "127.0.0.1"
$port = 8470

# Required env vars for proxy (/write endpoint support)
$env:GITHUB_TOKEN = "PASTE_YOUR_GITHUB_PAT_HERE"
$env:SVC_PROFILE_PATH = "C:\DayZServer\profiles"
$env:SVC_MISSION_PATH = "C:\DayZServer\mpmissions\dayzOffline.chernarusplus"
$env:SVC_ALLOWED_IPS = "127.0.0.1"

if (-not (Test-Path -LiteralPath $proxyExe)) {
    Write-Host "[HOOK][ERROR] Proxy executable not found: $proxyExe"
    exit 1
}

$running = Get-CimInstance Win32_Process -Filter "Name='svc-gitconfig-proxy.exe'" -ErrorAction SilentlyContinue
if ($running) {
    Write-Host "[HOOK][INFO] Proxy already running. Skipping start."
    exit 0
}

Write-Host "[HOOK][INFO] Starting proxy: $proxyExe"
$proc = Start-Process -FilePath $proxyExe -ArgumentList @("--bind", $bind, "--port", "$port") -WorkingDirectory $proxyDir -WindowStyle Hidden -PassThru
Start-Sleep -Seconds 2

if ($proc.HasExited) {
    Write-Host "[HOOK][ERROR] Proxy exited immediately with code $($proc.ExitCode)."
    exit 1
}

Write-Host "[HOOK][OK] Proxy is running."
exit 0
