#!/usr/bin/env bash
# Host-side driver for spike M0.7 on the Windows test VM.
#   run-case.sh server <tag> [--deny-network]   start a server on the interactive desktop
#   run-case.sh client-desktop <tag>              client on the interactive desktop (same user)
#   run-case.sh client-system <tag>               client as LocalSystem (different principal)
#   run-case.sh client-ssh <tag>                  client in the ssh logon session (same user, Session 0)
#   run-case.sh show <tag>                        print server and client output
set -euo pipefail
vm="$(dirname "$0")/../../../scripts/local/vm"
dir='C:\traytray-spike'
task() { "$vm" ssh "powershell -NoProfile -ExecutionPolicy Bypass -File $dir\\task.ps1 $*"; }
case "$1" in
  server)
    task -Name TraytraySpikeServer -Command cmd.exe -Arguments "\"/c $dir\\start-server.cmd $2 ${3:-}\""
    sleep 3
    "$vm" ssh "\$t = Get-Content $dir\\$2-server.txt -Raw; \$t; if (\$t -match 'listening (\\S+)') { [IO.File]::WriteAllText('$dir\\pipename.txt', \$Matches[1]) } else { throw 'no listening line' }"
    ;;
  client-desktop) task -Name TraytraySpikeClient -Command cmd.exe -Arguments "\"/c $dir\\run-client.cmd $2\""; sleep 4 ;;
  client-system)  task -Name TraytraySpikeClientSys -Command cmd.exe -Arguments "\"/c $dir\\run-client.cmd $2\"" -System; sleep 4 ;;
  client-ssh)     "$vm" ssh "cmd /c $dir\\run-client.cmd $2" ;;
  show)           "$vm" ssh "Get-Content $dir\\$2-server.txt -ErrorAction SilentlyContinue; '---'; Get-ChildItem $dir\\$2*-client.txt | ForEach-Object { '== ' + \$_.Name; Get-Content \$_.FullName }" ;;
  *) echo "unknown command" >&2; exit 2 ;;
esac
