# Lists NotifyIconSettings entries whose ExecutablePath matches a pattern, with every value,
# its registry kind, and for binary values the length and first bytes.
param([string]$Like = '*Traytray*')
$root = 'HKCU:\Control Panel\NotifyIconSettings'
foreach ($key in Get-ChildItem $root) {
    $props = Get-ItemProperty $key.PSPath
    if ($props.ExecutablePath -notlike $Like) { continue }
    "[$($key.PSChildName)]"
    foreach ($name in $key.GetValueNames()) {
        $kind = $key.GetValueKind($name)
        $value = $key.GetValue($name)
        if ($value -is [byte[]]) {
            $head = ($value[0..([Math]::Min(7, $value.Length - 1))] | ForEach-Object { $_.ToString('X2') }) -join ' '
            "  $name ($kind) = $($value.Length) bytes, starts $head"
        } else {
            "  $name ($kind) = $value"
        }
    }
}
"UIOrderList entries: $(@((Get-ItemProperty $root).UIOrderList).Count)"
