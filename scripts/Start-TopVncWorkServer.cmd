@echo off
setlocal
rem Configure your own local private IPv4 and port through TOPVNC_BIND.
rem Keep it aligned with your separately reviewed peer firewall rule.
if not defined TOPVNC_BIND set "TOPVNC_BIND=127.0.0.1:5900"
set "TOPVNC_SERVER=%~dp0..\target\debug\topvnc.exe"

if not "%~1"=="" if /I not "%~1"=="/check" goto usage
if not "%~2"=="" goto usage
rem Validate the environment value before cmd expands it in commands or output.
powershell.exe -NoProfile -NonInteractive -Command "$v=$env:TOPVNC_BIND; if ($v -notmatch '^(\d{1,3}\.){3}\d{1,3}:[0-9]{1,5}$') { exit 1 }; $ip=$null; $parts=$v.Split(':'); if (-not [Net.IPAddress]::TryParse($parts[0],[ref]$ip) -or $ip.ToString() -ne $parts[0] -or [int]$parts[1] -lt 1 -or [int]$parts[1] -gt 65535) { exit 1 }"
if errorlevel 1 (
    echo ERROR: Set TOPVNC_BIND to one local IPv4 address and port, such as 127.0.0.1:5900.
    exit /b 2
)
if not exist "%TOPVNC_SERVER%" (
    echo ERROR: Expected server executable is missing:
    echo "%TOPVNC_SERVER%"
    echo Build TopVNC from the repository root with: cargo build
    exit /b 1
)

echo TopVNC work server: %TOPVNC_BIND%
echo Command: "%TOPVNC_SERVER%" --serve %TOPVNC_BIND% --tls-cert TOPVNC_TLS_CERT --tls-key TOPVNC_TLS_KEY
if /I "%~1"=="/check" (
    echo CHECK ONLY: executable found; server was not started.
    echo TLS hosting requires TOPVNC_TLS_CERT and TOPVNC_TLS_KEY PEM file paths.
    echo This does not verify certificates, VPN policy, firewall rules, or port availability.
    exit /b 0
)
echo Enter the VNC password at the local prompt. Do not share it in chat.
echo Keep this terminal open. Press Ctrl+C to stop the server.
echo Meshnet and permitted WARP settings must already be ready.
rem Pass credential paths as PowerShell argument values, never expanded cmd code.
powershell.exe -NoProfile -Command "if (-not $env:TOPVNC_TLS_CERT -or -not $env:TOPVNC_TLS_KEY) { Write-Error 'Set TOPVNC_TLS_CERT and TOPVNC_TLS_KEY to your PEM files.'; exit 2 }; & $env:TOPVNC_SERVER --serve $env:TOPVNC_BIND --tls-cert $env:TOPVNC_TLS_CERT --tls-key $env:TOPVNC_TLS_KEY; exit $LASTEXITCODE"
exit /b %ERRORLEVEL%

:usage
echo Usage: Start-TopVncWorkServer.cmd [/check]
echo Starts the existing debug server on TOPVNC_BIND, default 127.0.0.1:5900.
echo /check checks the executable and prints the command without starting it.
exit /b 2
