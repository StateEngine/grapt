$ErrorActionPreference = 'Stop'
$operatorFile = Join-Path ([Environment]::GetFolderPath('UserProfile')) '.config/secode/build.env'
if (Test-Path -LiteralPath $operatorFile) {
    if ((Get-Item -LiteralPath $operatorFile).Length -gt 65536) { throw 'Operator configuration exceeds 64 KiB' }
    foreach ($row in [IO.File]::ReadAllLines($operatorFile)) {
        if (-not $row.Trim() -or $row.StartsWith('#')) { continue }
        if ($row -cnotmatch '^((?:SECODE|STATEENGINE)_[A-Z0-9_]+)=(.+)$') { throw 'Operator configuration requires plain unquoted NAME=value rows' }
        $operatorName, $operatorValue = $Matches[1], $Matches[2]
        if ($operatorValue -cne $operatorValue.Trim() -or $operatorValue.IndexOfAny([char[]]@([char]0, '"', "'")) -ge 0) { throw 'Operator configuration requires plain unquoted NAME=value rows' }
        if ($null -eq [Environment]::GetEnvironmentVariable($operatorName, 'Process')) {
            [Environment]::SetEnvironmentVariable($operatorName, $operatorValue, 'Process')
        }
    }
}
