param([switch]$Worker, [string]$Root, [string]$Branch, [double]$WaitSeconds = 15,
      [string]$ReleaseName, [string]$AcquiredName, [switch]$Priority)
$ErrorActionPreference = 'Stop'
. "$PSScriptRoot/landing_lease.ps1"
if ($Worker) {
    $lease = $null
    try {
        $lease = Open-CanonicalLandingLease $Root $Branch ('a' * 40) $WaitSeconds -Priority:$Priority
        $acquired = [Threading.EventWaitHandle]::OpenExisting($AcquiredName)
        $release = [Threading.EventWaitHandle]::OpenExisting($ReleaseName)
        try {
            [void]$acquired.Set()
            if (-not $release.WaitOne(20000)) { throw 'Fixture release event timed out.' }
        } finally { $acquired.Dispose(); $release.Dispose() }
    } catch { [Console]::Error.WriteLine($_.Exception.Message); exit 2 }
    finally { if ($lease) { Close-CanonicalLandingLease $lease } }
    exit 0
}

$directory = Join-Path $env:TEMP ('secode-landing-lease-' + [Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($directory)
$workers = [Collections.Generic.List[object]]::new()
function Start-Worker([string]$Path, [string]$Name, [double]$Wait = 15, [switch]$Priority) {
    [void][IO.Directory]::CreateDirectory((Join-Path $Path '.git'))
    $id = 'Local\secode-lease-' + [Guid]::NewGuid().ToString('N')
    $release = [Threading.EventWaitHandle]::new($false, [Threading.EventResetMode]::ManualReset, $id + '-release')
    $acquired = [Threading.EventWaitHandle]::new($false, [Threading.EventResetMode]::ManualReset, $id + '-acquired')
    $script = "& '$($PSCommandPath.Replace("'", "''"))' -Worker -Root '$($Path.Replace("'", "''"))' -Branch '$Name' -WaitSeconds $Wait -ReleaseName '$id-release' -AcquiredName '$id-acquired'"
    if ($Priority) { $script += ' -Priority' }
    $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($script))
    $info = [Diagnostics.ProcessStartInfo]::new()
    $info.FileName = Join-Path $PSHOME 'powershell.exe'
    $info.Arguments = "-NoProfile -NonInteractive -OutputFormat Text -EncodedCommand $encoded"
    $info.UseShellExecute = $false
    $info.CreateNoWindow = $true
    $info.RedirectStandardOutput = $true
    $info.RedirectStandardError = $true
    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $info
    [void]$process.Start()
    $row = [pscustomobject]@{Process=$process; Release=$release; Acquired=$acquired}
    $workers.Add($row)
    return $row
}
function Wait-Queued($Row) {
    for ($index = 0; $index -lt 8; $index++) {
        $read = $Row.Process.StandardOutput.ReadLineAsync()
        if (-not $read.Wait(15000) -or $null -eq $read.Result) {
            $detail = if ($Row.Process.WaitForExit(1000)) { $Row.Process.StandardError.ReadToEnd() } else { 'worker is still running' }
            throw "Worker did not join the landing queue: $detail"
        }
        if ($read.Result.StartsWith('Queued landing ')) { return }
    }
    throw 'Worker never reported its queued landing.'
}
function Wait-Acquired($Row) {
    if (-not $Row.Acquired.WaitOne(15000)) { throw 'Queued landing never acquired the lease.' }
}
function Wait-Position($Row, [int]$Position) {
    $read = $Row.Process.StandardOutput.ReadLineAsync()
    if (-not $read.Wait(15000) -or $null -eq $read.Result -or
        $read.Result -notmatch ('position ' + $Position + ':')) {
        throw "Waiting landing did not report its queue position: $($read.Result)"
    }
}
function Finish-Worker($Row) {
    [void]$Row.Release.Set()
    if (-not $Row.Process.WaitForExit(15000)) { throw 'Released landing did not exit.' }
    if ($Row.Process.ExitCode) { throw $Row.Process.StandardError.ReadToEnd() }
}
try {
    $path = Join-Path $directory 'sequence'
    $first = Start-Worker $path 'first'; Wait-Acquired $first
    $blocked = Join-Path $path '.git/secode-landing.queue/00000000000000000000-1-00000000000000000000000000000000.waiter'
    [IO.File]::WriteAllText($blocked, '{}')
    [IO.File]::SetAttributes($blocked, [IO.FileAttributes]::ReadOnly)
    try {
        $head = Get-LandingQueueHead (Join-Path $path '.git/secode-landing.queue') ''
        if ($head -ne [IO.Path]::GetFileName($blocked)) {
            throw 'Unreadable queue entry was bypassed.'
        }
    } finally {
        [IO.File]::SetAttributes($blocked, [IO.FileAttributes]::Normal)
        [IO.File]::Delete($blocked)
    }
    $second = Start-Worker $path 'second'; Wait-Queued $second
    Wait-Position $second 1
    $third = Start-Worker $path 'third'; Wait-Queued $third
    Wait-Position $third 2
    Finish-Worker $first
    Wait-Acquired $second
    if ($third.Acquired.WaitOne(100)) { throw 'Later arrival bypassed the earlier landing.' }
    Finish-Worker $second
    Wait-Acquired $third
    Finish-Worker $third
    Write-Output 'PASS queued landings acquire in arrival order'

    $path = Join-Path $directory 'priority'
    $first = Start-Worker $path 'holder'; Wait-Acquired $first
    $second = Start-Worker $path 'ordinary'; Wait-Queued $second
    $urgent = Start-Worker $path 'boot-recovery' -Priority; Wait-Queued $urgent
    if ($urgent.Acquired.WaitOne(100) -or $first.Process.HasExited) { throw 'Priority interrupted the holder.' }
    Finish-Worker $first
    Wait-Acquired $urgent
    if ($second.Acquired.WaitOne(100)) { throw 'Ordinary FIFO caller bypassed priority recovery.' }
    Finish-Worker $urgent
    Wait-Acquired $second; Finish-Worker $second
    Write-Output 'PASS priority recovery follows the holder and preserves ordinary waiters'

    $path = Join-Path $directory 'crash'
    $first = Start-Worker $path 'crashed'; Wait-Acquired $first
    $first.Process.Kill()
    if (-not $first.Process.WaitForExit(5000)) { throw 'Owned fixture holder did not stop.' }
    # An older controller can acquire the handle without replacing the dead
    # owner's sidecar. It must not be attributed to that crashed process.
    $legacy = Open-LandingFile (Join-Path $path '.git/secode-landing.lock')
    if (-not $legacy) { throw 'Crashed holder retained the canonical handle.' }
    try {
        $second = Start-Worker $path 'legacy-blocked' 0.25
        if (-not $second.Process.WaitForExit(15000)) { throw 'Legacy-holder wait ignored its timeout.' }
        $errorText = $second.Process.StandardError.ReadToEnd()
        if ($second.Process.ExitCode -eq 0 -or $errorText -notmatch 'metadata stale' -or
            $errorText.Contains('holder branch=crashed')) {
            throw "Crashed metadata was reported as a current holder: $errorText"
        }
    } finally { $legacy.Dispose() }
    $second = Start-Worker $path 'successor'; Wait-Acquired $second; Finish-Worker $second
    Write-Output 'PASS holder crash releases the canonical lease'

    $path = Join-Path $directory 'timeout'
    $first = Start-Worker $path 'holder'; Wait-Acquired $first
    $second = Start-Worker $path 'timeout' 0.25
    if (-not $second.Process.WaitForExit(15000)) { throw 'Landing wait ignored its timeout.' }
    $errorText = $second.Process.StandardError.ReadToEnd()
    if ($second.Process.ExitCode -eq 0 -or $errorText -notmatch 'wait timed out' -or
        -not $errorText.Contains('branch=holder') -or -not $errorText.Contains('accepted=' + ('a' * 40)) -or
        -not $errorText.Contains('pid=' + $first.Process.Id) -or $errorText -notmatch 'started=\d{4}-\d\d-\d\dT') {
        throw "Timed out landing did not identify its holder (exit=$($second.Process.ExitCode), holderPid=$($first.Process.Id)): $errorText"
    }
    if ($first.Process.HasExited) { throw 'Wait timeout stopped the holder.' }
    Finish-Worker $first
    $third = Start-Worker $path 'after-timeout'; Wait-Acquired $third; Finish-Worker $third
    Write-Output 'PASS timeout names the holder and retires its own waiter'
} finally {
    foreach ($row in $workers) {
        if (-not $row.Process.HasExited) { $row.Process.Kill(); [void]$row.Process.WaitForExit(5000) }
        $row.Process.Dispose(); $row.Release.Dispose(); $row.Acquired.Dispose()
    }
}
