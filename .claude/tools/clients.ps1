param([switch]$Stop)
Get-CimInstance Win32_Process -Filter "Name like 'java%'" |
    Where-Object { $_.CommandLine -like '*Manycraft*instances*' } |
    ForEach-Object {
        $name = [regex]::Match($_.CommandLine, '--username (\S+)').Groups[1].Value
        if ($Stop) { Stop-Process -Id $_.ProcessId -Force }
        '{0} {1}{2}' -f $_.ProcessId, $name, $(if ($Stop) { ' stopped' } else { '' })
    }
