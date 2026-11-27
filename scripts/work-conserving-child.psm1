Set-StrictMode -Version Latest

$script:ChildPayloadEnvironmentName = 'GRAPT_CHILD_PAYLOAD_B64'
$script:MaxWindowsEnvironmentCharacters = 32767
$script:ChildEnvironmentAllowlist = @(
    'APPDATA',
    'COMSPEC',
    'HOME',
    'HOMEDRIVE',
    'HOMEPATH',
    'LOCALAPPDATA',
    'PATH',
    'PATHEXT',
    'SystemDrive',
    'SystemRoot',
    'TEMP',
    'TMP',
    'USERPROFILE',
    'WINDIR'
)

function Get-SeCodeClosedChildEnvironment {
    $environment = [ordered]@{}
    foreach ($name in $script:ChildEnvironmentAllowlist) {
        $value = [Environment]::GetEnvironmentVariable($name, 'Process')
        if (-not [string]::IsNullOrEmpty($value)) {
            $environment[$name] = $value
        }
    }
    $environment
}

function Get-SeCodePayloadBounds {
    param([Parameter(Mandatory = $true)]$Environment)

    $otherCharacters = 1
    foreach ($entry in $Environment.GetEnumerator()) {
        $otherCharacters += $entry.Key.Length + 1 + ([string]$entry.Value).Length + 1
    }
    $availableEncodedCharacters = $script:MaxWindowsEnvironmentCharacters -
        $otherCharacters - $script:ChildPayloadEnvironmentName.Length - 2
    $maxEncodedBytes = [Math]::Floor($availableEncodedCharacters / 4) * 4
    if ($maxEncodedBytes -lt 4) {
        throw 'closed child environment leaves no bounded payload capacity'
    }
    [pscustomobject]@{
        MaxEncodedPayloadBytes = [int]$maxEncodedBytes
        MaxDecodedPayloadUtf8Bytes = [int](($maxEncodedBytes / 4) * 3)
        MaxArgumentUtf8Bytes = [int](($maxEncodedBytes / 4) * 3)
    }
}

function Get-SeCodeChildBootstrapBase64 {
    $bootstrap = @'
$ErrorActionPreference = 'Stop'
try {
    $payloadEnvironmentName = 'GRAPT_CHILD_PAYLOAD_B64'
    $maxWindowsEnvironmentCharacters = 32767
    $encoded = [Environment]::GetEnvironmentVariable('GRAPT_CHILD_PAYLOAD_B64', 'Process')
    if ([string]::IsNullOrWhiteSpace($encoded)) {
        throw 'GRAPT_CHILD_PAYLOAD_B64 is absent'
    }
    $otherEnvironmentCharacters = 1
    foreach ($entry in [Environment]::GetEnvironmentVariables('Process').GetEnumerator()) {
        if ($entry.Key -ne $payloadEnvironmentName) {
            $otherEnvironmentCharacters += ([string]$entry.Key).Length + 1 + ([string]$entry.Value).Length + 1
        }
    }
    $maxEncodedPayloadBytes = [Math]::Floor(
        ($maxWindowsEnvironmentCharacters - $otherEnvironmentCharacters - $payloadEnvironmentName.Length - 2) / 4
    ) * 4
    if ($encoded.Length -gt $maxEncodedPayloadBytes) {
        throw "encoded child payload exceeds its Windows environment bound: $($encoded.Length) > $maxEncodedPayloadBytes"
    }
    $maxDecodedPayloadUtf8Bytes = [int](($maxEncodedPayloadBytes / 4) * 3)
    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    $decoded = [Convert]::FromBase64String($encoded)
    if ($decoded.Length -gt $maxDecodedPayloadUtf8Bytes) {
        throw "decoded child payload exceeds its UTF-8 bound: $($decoded.Length) > $maxDecodedPayloadUtf8Bytes"
    }
    $json = $utf8.GetString($decoded)
    $payload = $json | ConvertFrom-Json
    $names = @($payload.PSObject.Properties.Name)
    if ($names.Count -ne 3 -or
        -not $names.Contains('contract') -or
        -not $names.Contains('script') -or
        -not $names.Contains('arguments')) {
        throw 'child payload must contain exactly contract, script, and arguments'
    }
    if ($payload.contract -ne 'grapt-literal-child/v1') {
        throw 'child payload contract mismatch'
    }
    if ($payload.script -isnot [string] -or
        [string]::IsNullOrWhiteSpace($payload.script) -or
        $payload.script.IndexOf([char]0) -ge 0 -or
        -not (Test-Path -LiteralPath $payload.script -PathType Leaf)) {
        throw 'child payload script is not an existing literal file'
    }
    if ($payload.arguments -isnot [System.Array]) {
        throw 'child payload arguments must be a JSON array'
    }
    $arguments = @($payload.arguments)
    if ($arguments.Count -gt 64) {
        throw 'child payload contains more than 64 arguments'
    }
    foreach ($argument in $arguments) {
        if ($argument -isnot [string]) {
            throw 'child payload arguments must be JSON strings'
        }
        $argumentBytes = $utf8.GetByteCount($argument)
        if ($argumentBytes -gt $maxDecodedPayloadUtf8Bytes) {
            throw "child payload argument exceeds its UTF-8 bound: $argumentBytes > $maxDecodedPayloadUtf8Bytes"
        }
        if ($argument.IndexOf([char]0) -ge 0) {
            throw 'child payload arguments must be NUL-free strings'
        }
    }
    if (($arguments.Count % 2) -ne 0) {
        throw 'child payload arguments must be named-parameter pairs'
    }
    $namedArguments = @{}
    for ($index = 0; $index -lt $arguments.Count; $index += 2) {
        $name = $arguments[$index]
        if ($name -notmatch '^-[A-Za-z][A-Za-z0-9]*$') {
            throw "child payload argument name is invalid: $name"
        }
        $key = $name.Substring(1)
        if ($namedArguments.ContainsKey($key)) {
            throw "child payload argument name is duplicated: $name"
        }
        $namedArguments[$key] = $arguments[$index + 1]
    }
    [Environment]::SetEnvironmentVariable('GRAPT_CHILD_PAYLOAD_B64', $null, 'Process')
    $global:LASTEXITCODE = 0
    & $payload.script @namedArguments
    $invocationSucceeded = $?
    $childExitCode = [int]$global:LASTEXITCODE
    if (-not $invocationSucceeded -and $childExitCode -eq 0) {
        exit 1
    }
    exit $childExitCode
} catch {
    [Console]::Error.WriteLine("literal child bootstrap refused: $($_.Exception.Message)")
    exit 125
}
'@
    [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($bootstrap))
}

function Start-SeCodeLiteralChild {
    param(
        [Parameter(Mandatory = $true)]
        [string]$ScriptPath,
        [Parameter(Mandatory = $true)]
        [string[]]$Arguments
    )

    if (-not (Test-Path -LiteralPath $ScriptPath -PathType Leaf)) {
        throw "literal child script is absent: $ScriptPath"
    }
    if ($Arguments.Count -gt 64) {
        throw 'literal child argument count exceeds 64'
    }
    $childEnvironment = Get-SeCodeClosedChildEnvironment
    $bounds = Get-SeCodePayloadBounds -Environment $childEnvironment
    $utf8 = New-Object System.Text.UTF8Encoding($false, $true)
    foreach ($argument in $Arguments) {
        if ($null -eq $argument -or $argument.IndexOf([char]0) -ge 0) {
            throw 'literal child arguments must be non-null NUL-free strings'
        }
        $argumentBytes = $utf8.GetByteCount($argument)
        if ($argumentBytes -gt $bounds.MaxArgumentUtf8Bytes) {
            throw "literal child argument exceeds its UTF-8 bound: $argumentBytes > $($bounds.MaxArgumentUtf8Bytes)"
        }
    }

    $payload = [ordered]@{
        contract = 'grapt-literal-child/v1'
        script = [IO.Path]::GetFullPath($ScriptPath)
        arguments = @($Arguments)
    }
    $payloadJson = $payload | ConvertTo-Json -Depth 4 -Compress
    $payloadBytes = $utf8.GetBytes($payloadJson)
    if ($payloadBytes.Length -gt $bounds.MaxDecodedPayloadUtf8Bytes) {
        throw "decoded child payload exceeds its UTF-8 bound: $($payloadBytes.Length) > $($bounds.MaxDecodedPayloadUtf8Bytes)"
    }
    $payloadBase64 = [Convert]::ToBase64String($payloadBytes)
    if ($payloadBase64.Length -gt $bounds.MaxEncodedPayloadBytes) {
        throw "encoded child payload exceeds its Windows environment bound: $($payloadBase64.Length) > $($bounds.MaxEncodedPayloadBytes)"
    }
    $bootstrapBase64 = Get-SeCodeChildBootstrapBase64

    $startInfo = New-Object System.Diagnostics.ProcessStartInfo
    $startInfo.FileName = Join-Path $env:SystemRoot 'System32\WindowsPowerShell\v1.0\powershell.exe'
    $startInfo.Arguments = "-NoProfile -NonInteractive -EncodedCommand $bootstrapBase64"
    $startInfo.UseShellExecute = $false
    $startInfo.CreateNoWindow = $true
    $startInfo.RedirectStandardOutput = $true
    $startInfo.RedirectStandardError = $true
    $startInfo.EnvironmentVariables.Clear()
    foreach ($entry in $childEnvironment.GetEnumerator()) {
        $startInfo.EnvironmentVariables[$entry.Key] = [string]$entry.Value
    }
    $startInfo.EnvironmentVariables['GRAPT_CHILD_PAYLOAD_B64'] = $payloadBase64

    $process = New-Object System.Diagnostics.Process
    $process.StartInfo = $startInfo
    if (-not $process.Start()) {
        $process.Dispose()
        throw 'literal PowerShell child did not start'
    }
    $process
}

Export-ModuleMember -Function Start-SeCodeLiteralChild
