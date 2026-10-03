@echo off
rem Runs the spike client against the pipe name stored in pipename.txt.
rem %1 = output tag
cd /d C:\traytray-spike
set /p PIPE=<pipename.txt
whoami /user /fo list > %1-client.txt 2>&1
traytray-pipe-spike.exe client --name %PIPE% --message "from %1" >> %1-client.txt 2>&1
echo exit=%ERRORLEVEL% >> %1-client.txt
