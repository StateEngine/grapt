# The file handle owns exclusion; queue files only order cooperating callers.
function Open-LandingFile([string]$Path) {
    try {
        return [IO.File]::Open($Path, [IO.FileMode]::OpenOrCreate,
            [IO.FileAccess]::ReadWrite, [IO.FileShare]::None)
    } catch [IO.IOException] {
        $failure = $_.Exception
        if ($failure.InnerException -is [IO.IOException]) { $failure = $failure.InnerException }
        if (($failure.HResult -band 65535) -notin @(32, 33)) { throw }
        return $null
    }
}

function Read-LandingJson([string]$Path) {
    if (-not [IO.File]::Exists($Path)) { return $null }
    if ([IO.File]::GetAttributes($Path) -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Landing metadata must be a regular file.'
    }
    $stream = [IO.File]::Open($Path, [IO.FileMode]::Open, [IO.FileAccess]::Read,
        ([IO.FileShare]::ReadWrite -bor [IO.FileShare]::Delete))
    try {
        $bytes = [byte[]]::new(4097)
        $count = 0
        while ($count -lt $bytes.Length) {
            $read = $stream.Read($bytes, $count, $bytes.Length - $count)
            if (-not $read) { break }
            $count += $read
        }
        if ($count -gt 4096) { throw 'Landing metadata exceeds its bound.' }
        return ([Text.Encoding]::UTF8.GetString($bytes, 0, $count) | ConvertFrom-Json)
    } finally { $stream.Dispose() }
}

function Write-LandingJson([string]$Path, $Value) {
    $text = $Value | ConvertTo-Json -Compress
    if ([Text.Encoding]::UTF8.GetByteCount($text) -gt 4096) { throw 'Landing metadata exceeds its bound.' }
    $temporary = $Path + '.new'
    foreach ($candidate in @($Path, $temporary)) {
        if ([IO.File]::Exists($candidate) -and
            ([IO.File]::GetAttributes($candidate) -band [IO.FileAttributes]::ReparsePoint)) {
            throw 'Landing metadata must be a regular file.'
        }
    }
    [IO.File]::WriteAllText($temporary, $text, [Text.UTF8Encoding]::new($false))
    if ([IO.File]::Exists($Path)) { [IO.File]::Replace($temporary, $Path, [System.Management.Automation.Language.NullString]::Value) }
    else { [IO.File]::Move($temporary, $Path) }
}

function Get-LandingHolder([string]$Path) {
    try {
        $row = Read-LandingJson $Path
        if ($row.branch -notmatch '^[a-z][a-z0-9-]*$' -or
            $row.accepted_commit -notmatch '^[0-9a-f]{40}$' -or
            $row.pid -isnot [int] -or $row.pid -le 0 -or
            $row.started_utc -notmatch '^\d{4}-\d\d-\d\dT[0-9:.+Z-]{8,40}$') {
            return 'holder metadata unavailable (possibly an older controller)'
        }
        $process = $null
        try {
            $process = [Diagnostics.Process]::GetProcessById($row.pid)
            if (-not $row.process_started_utc -or $process.HasExited -or
                $process.StartTime.ToUniversalTime().ToString('o') -ne $row.process_started_utc) {
                return 'holder metadata stale; current holder unavailable'
            }
        } catch [ArgumentException] {
            return 'holder metadata stale; recorded process has exited'
        } catch { return 'holder metadata unverifiable' }
        finally { if ($process) { $process.Dispose() } }
        return "holder branch=$($row.branch) accepted=$($row.accepted_commit) started=$($row.started_utc) pid=$($row.pid)"
    } catch { return 'holder metadata unreadable' }
}

function Get-LandingQueueHead([string]$Directory, [string]$Own, [ref]$Position) {
    $names = [Collections.Generic.List[string]]::new()
    foreach ($path in [IO.Directory]::EnumerateFiles($Directory, '*.waiter')) {
        if ($names.Count -ge 1024) { throw 'Landing queue exceeds its bound.' }
        $name = [IO.Path]::GetFileName($path)
        if ($name -notmatch '^\d{20}-[0-9]+-[0-9a-f]{32}\.waiter$') {
            throw 'Unrecognized landing queue entry; preserve it.'
        }
        $names.Add($name)
    }
    $head = $null
    $index = 1
    foreach ($name in ($names | Sort-Object)) {
        if ($name -eq $Own) {
            if ($null -ne $Position) { $Position.Value = $index }
            if ($head) { return $head }
            return $name
        }
        $path = Join-Path $Directory $name
        try { $probe = Open-LandingFile $path }
        catch [UnauthorizedAccessException] {
            # Windows can deny an open while a retired file is delete-pending.
            # Preserve unreadable entries ahead of us until a later probe succeeds.
            if ($null -eq $Position) { return $name }
            if (-not $head) { $head = $name }
            $index++
            continue
        }
        if (-not $probe) {
            if ($null -eq $Position) { return $name }
            if (-not $head) { $head = $name }
            $index++
            continue
        }
        # A dead caller cannot retain its exclusive waiter handle.
        $probe.Dispose()
        [IO.File]::Delete($path)
    }
    return $head
}

function Open-CanonicalLandingLease {
    param([string]$Repository, [string]$Branch, [string]$AcceptedCommit,
          [ValidateRange(0,86400)][double]$WaitSeconds = 2700,
          [switch]$Priority)
    $path = Join-Path $Repository '.git/secode-landing.lock'
    $ownerPath = $path + '.owner.json'
    $directory = Join-Path $Repository '.git/secode-landing.queue'
    [void][IO.Directory]::CreateDirectory($directory)
    if ([IO.File]::GetAttributes($directory) -band [IO.FileAttributes]::ReparsePoint) {
        throw 'Landing queue must be a real directory.'
    }
    $watch = [Diagnostics.Stopwatch]::StartNew()
    $nextProgress = 0
    $ticket = $null
    $ticketPath = $null
    $primary = $null
    $delivered = $false
    $first = $true
    $position = 0
    try {
        while ($first -or $watch.Elapsed.TotalSeconds -lt $WaitSeconds) {
            $first = $false
            $guard = Open-LandingFile (Join-Path $directory 'queue.lock')
            if ($guard) {
                try {
                    if (-not $ticket) {
                        [void](Get-LandingQueueHead $directory '')
                        $counterPath = Join-Path $directory 'counter.json'
                        $counter = Read-LandingJson $counterPath
                        $sequence = 0L
                        if ($counter) {
                            if (-not [long]::TryParse([string]$counter.next, [ref]$sequence) -or $sequence -lt 0) {
                                throw 'Invalid landing queue sequence; preserve it.'
                            }
                        } elseif ([IO.File]::Exists($counterPath)) { throw 'Unreadable landing queue sequence.' }
                        if ($sequence -eq [long]::MaxValue) { throw 'Landing queue sequence exhausted.' }
                        if ($Priority) {
                            # Sequence zero fences ordinary callers, including
                            # older FIFO controllers, without moving a live file.
                            foreach ($entry in [IO.Directory]::EnumerateFiles($directory, '00000000000000000000-*.waiter')) {
                                throw 'A priority landing is already queued; preserve its position.'
                            }
                            $sequence = 0L
                        } else {
                            $sequence++
                            Write-LandingJson $counterPath @{next=$sequence}
                        }
                        $token = [Guid]::NewGuid().ToString('N')
                        $name = '{0:D20}-{1}-{2}.waiter' -f $sequence, $PID, $token
                        $ticketPath = Join-Path $directory $name
                        $ticket = [IO.File]::Open($ticketPath, [IO.FileMode]::CreateNew,
                            [IO.FileAccess]::ReadWrite, [IO.FileShare]::Read)
                        $metadata = @{branch=$Branch; accepted_commit=$AcceptedCommit; pid=$PID;
                            started_utc=[DateTimeOffset]::UtcNow.ToString('o'); token=$token}
                        $process = [Diagnostics.Process]::GetCurrentProcess()
                        try { $metadata.process_started_utc = $process.StartTime.ToUniversalTime().ToString('o') }
                        finally { $process.Dispose() }
                        $bytes = [Text.Encoding]::UTF8.GetBytes(($metadata | ConvertTo-Json -Compress))
                        $ticket.Write($bytes, 0, $bytes.Length)
                        $ticket.Flush()
                        Write-Host "Queued landing branch=$Branch accepted=$AcceptedCommit pid=$PID"
                    }
                    if ((Get-LandingQueueHead $directory $name ([ref]$position)) -eq $name) {
                        $primary = Open-LandingFile $path
                        if ($primary) {
                            $metadata.started_utc = [DateTimeOffset]::UtcNow.ToString('o')
                            Write-LandingJson $ownerPath $metadata
                            $delivered = $true
                            return [pscustomobject]@{Handle=$primary; OwnerPath=$ownerPath; Token=$token}
                        }
                    }
                } finally { $guard.Dispose() }
            }
            if ($watch.Elapsed.TotalSeconds -ge $WaitSeconds) { break }
            if ($watch.Elapsed.TotalSeconds -ge $nextProgress) {
                $place = if ($position) { [string]$position } else { 'unknown' }
                Write-Host ("Waiting for canonical landing lease ({0:n0}s elapsed), position {1}: {2}" -f
                    $watch.Elapsed.TotalSeconds, $place, (Get-LandingHolder $ownerPath))
                $nextProgress = $watch.Elapsed.TotalSeconds + 60
            }
            $remaining = ($WaitSeconds - $watch.Elapsed.TotalSeconds) * 1000
            [Threading.Thread]::Sleep([int][Math]::Max(1, [Math]::Min(500, $remaining)))
        }
        $place = if ($position) { [string]$position } else { 'unknown' }
        throw ("Canonical landing lease is held; wait timed out after {0:n1}s; position {1}; {2}." -f
            $WaitSeconds, $place, (Get-LandingHolder $ownerPath))
    } finally {
        if (-not $delivered -and $primary) { $primary.Dispose() }
        if ($ticket) { $ticket.Dispose() }
        if ($ticketPath) {
            try { [IO.File]::Delete($ticketPath) }
            catch [IO.IOException] { Write-Warning 'Exited landing waiter will be retired by the next caller.' }
        }
    }
}

function Close-CanonicalLandingLease($Lease) {
    try {
        $row = Read-LandingJson $Lease.OwnerPath
        if ($row -and $row.token -eq $Lease.Token) { [IO.File]::Delete($Lease.OwnerPath) }
    } finally { $Lease.Handle.Dispose() }
}
