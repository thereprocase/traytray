@echo off
rem Starts the spike server on the interactive desktop (run through a scheduled task with /IT).
rem %1 = output tag, %2 = optional --deny-network
cd /d C:\traytray-spike
traytray-pipe-spike.exe server %2 > %1-server.txt 2>&1
echo exit=%ERRORLEVEL% >> %1-server.txt
