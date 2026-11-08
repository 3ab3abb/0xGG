@echo off
title 0xGG server
cd /d "%~dp0"
echo ============================================================
echo   0xGG SERVER  -  players type one of these IPs in PLAY.bat
echo ============================================================
ipconfig | findstr /C:"IPv4"
echo.
echo   (use the one on the Wi-Fi / hotspot everyone is on, usually 192.168.x.x or 172.20.x.x)
echo.
echo   Windows Firewall popup?  tick BOTH "Private" and "Public", then Allow.
echo   Nobody can connect?      run OPEN-FIREWALL.bat once (it asks for admin).
echo   Stop the server by closing this window.
echo ============================================================
echo.
gg-server.exe --bind 0.0.0.0:7777
pause
