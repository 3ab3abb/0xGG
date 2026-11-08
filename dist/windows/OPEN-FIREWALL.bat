@echo off
rem Opens port 7777 (TCP + UDP) for the 0xGG server. Only needed on the HOST machine.
net session >nul 2>&1
if errorlevel 1 (
    echo Asking for administrator rights...
    powershell -NoProfile -Command "Start-Process -FilePath '%~f0' -Verb RunAs"
    exit /b
)
netsh advfirewall firewall delete rule name="0xGG" >nul 2>&1
netsh advfirewall firewall add rule name="0xGG" dir=in action=allow protocol=TCP localport=7777 profile=any
netsh advfirewall firewall add rule name="0xGG" dir=in action=allow protocol=UDP localport=7777 profile=any
echo.
echo Done: port 7777 TCP and UDP is open on this machine.
pause
