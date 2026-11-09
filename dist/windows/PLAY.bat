@echo off
title 0xGG
cd /d "%~dp0"
rem Everything happens in the game window: type the host's IP, or press HOST GAME.
gg-client.exe
if errorlevel 1 pause
