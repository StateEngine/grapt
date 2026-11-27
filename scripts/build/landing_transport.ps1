# File-backed native output avoids Windows OpenSSH/PowerShell pipe stalls.
function Invoke-LandingTransport([string]$FileName, [string]$Arguments,
    [string]$CompletionMarker, [int]$CloseSeconds = 30) {
    $directory = Join-Path ([IO.Path]::GetTempPath()) ('secode-land-ssh-' + [Guid]::NewGuid().ToString('N'))
    [void][IO.Directory]::CreateDirectory($directory)
    $stdout = Join-Path $directory 'stdout.log'
    $stderr = Join-Path $directory 'stderr.log'
    $child = $null
    $closed = $null
    $offset = 0L
    $overlap = ''
    $buffer = [byte[]]::new(8192)
    $elapsed = [Diagnostics.Stopwatch]::StartNew()
    try {
        $child = Start-Process -FilePath $FileName -ArgumentList $Arguments -NoNewWindow -PassThru `
            -RedirectStandardOutput $stdout -RedirectStandardError $stderr
        # Windows PowerShell's Start-Process can lose ExitCode after its
        # short-lived child exits unless the native handle is retained first.
        $null = $child.Handle
        while (-not $child.WaitForExit(200)) {
            # The marker follows the remote command and its EXIT trap. Only
            # transport shutdown, not an active compiler, gets the short bound.
            $stream = [IO.File]::Open($stdout, 'Open', 'Read', 'ReadWrite')
            try {
                [void]$stream.Seek($offset, 'Begin')
                $end = $stream.Length
                while ($stream.Position -lt $end) {
                    $count = $stream.Read($buffer, 0, [int][Math]::Min($buffer.Length, $end - $stream.Position))
                    if ($count -eq 0) { break }
                    $text = $overlap + [Text.Encoding]::ASCII.GetString($buffer, 0, $count)
                    if ($null -eq $closed -and $text.Contains($CompletionMarker)) { $closed = $elapsed.Elapsed.TotalSeconds }
                    $overlap = $text.Substring([Math]::Max(0, $text.Length - $CompletionMarker.Length + 1))
                    $offset = $stream.Position
                    if ($null -ne $closed -and $elapsed.Elapsed.TotalSeconds - $closed -ge $CloseSeconds) { break }
                }
            } finally { $stream.Dispose() }
            if ($null -ne $closed -and $elapsed.Elapsed.TotalSeconds - $closed -ge $CloseSeconds) {
                throw "SSH did not close within $CloseSeconds seconds after the remote command finished."
            }
            if ($elapsed.Elapsed.TotalHours -ge 4) { throw 'Landing SSH exceeded its four-hour execution bound.' }
        }
        return $child.ExitCode
    } finally {
        # This Process object belongs to this invocation; never find/kill a holder.
        if ($child) {
            if (-not $child.HasExited) {
                $child.Kill()
                if (-not $child.WaitForExit(5000)) { throw 'Owned landing SSH did not terminate.' }
            }
            $child.Dispose()
        }
        foreach ($path in @($stdout, $stderr)) {
            if (Test-Path -LiteralPath $path) {
                $stream = [IO.File]::OpenRead($path)
                try {
                    if ($stream.Length -gt 1048576) { Write-Host "SSH output truncated to its last MiB: $path" }
                    [void]$stream.Seek([Math]::Max(0, $stream.Length - 1048576), 'Begin')
                    $reader = [IO.StreamReader]::new($stream)
                    try { Write-Host ($reader.ReadToEnd()) } finally { $reader.Dispose() }
                } finally { $stream.Dispose() }
            }
        }
        foreach ($path in @($stdout, $stderr)) {
            if (Test-Path -LiteralPath $path) { Remove-Item -LiteralPath $path -Force }
        }
        [IO.Directory]::Delete($directory)
    }
}
