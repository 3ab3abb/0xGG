@echo off
title 0xGG
cd /d "%~dp0"

rem Remember the last server so rehearsals are one keypress.
set "SERVER="
if exist last_server.txt set /p SERVER=<last_server.txt

:ask
set "INPUT="
if defined SERVER (
    set /p "INPUT=Server IP [press Enter for %SERVER%]: "
) else (
    set /p "INPUT=Server IP (shown in the HOST window): "
)
if defined INPUT set "SERVER=%INPUT%"
if not defined SERVER goto ask
>last_server.txt echo %SERVER%

gg-client.exe --server %SERVER%
if errorlevel 1 pause
