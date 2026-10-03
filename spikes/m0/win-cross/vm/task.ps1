# Runs a command once through a scheduled task, either on the signed-in user's interactive
# desktop (Session 1) or as SYSTEM. ssh sessions land in Session 0, so desktop work needs this.
param(
    [Parameter(Mandatory)][string]$Name,
    [Parameter(Mandatory)][string]$Command,
    [string]$Arguments = '',
    [switch]$System
)
$ErrorActionPreference = 'Stop'
$existing = Get-ScheduledTask -TaskName $Name -ErrorAction SilentlyContinue
if ($existing -and $existing.State -eq 'Running') { throw "task $Name is still running" }
if ($Arguments) { $action = New-ScheduledTaskAction -Execute $Command -Argument $Arguments }
else { $action = New-ScheduledTaskAction -Execute $Command }
if ($System) { $principal = New-ScheduledTaskPrincipal -UserId 'SYSTEM' -LogonType ServiceAccount }
else { $principal = New-ScheduledTaskPrincipal -UserId $env:USERNAME -LogonType Interactive }
Register-ScheduledTask -TaskName $Name -Action $action -Principal $principal -Force | Out-Null
Start-ScheduledTask -TaskName $Name
"started $Name"
