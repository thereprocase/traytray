# Compiles TrayProbe.cs into a WinForms exe with the inbox C# compiler (Windows PowerShell 5.1).
param([Parameter(Mandatory)][string]$Source, [Parameter(Mandatory)][string]$Out)
$ErrorActionPreference = 'Stop'
Add-Type -TypeDefinition (Get-Content -Raw $Source) -OutputAssembly $Out -OutputType WindowsApplication `
    -ReferencedAssemblies System.Windows.Forms, System.Drawing
Get-Item $Out | Select-Object FullName, Length
