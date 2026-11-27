function Get-HostedStartupPaths {
    $paths = @{}
    foreach ($name in @('USERPROFILE','APPDATA','LOCALAPPDATA','TEMP','TMP','PYTHONHOME','PYTHONPATH','VIRTUAL_ENV','PATH')) {
        $value = [Environment]::GetEnvironmentVariable($name)
        $row = @{present=($null -ne $value)}
        if ($null -ne $value) {
            if ($name -in @('USERPROFILE','APPDATA','LOCALAPPDATA','TEMP','TMP')) {
                if ($value.Length -le 192) { $row.value = $value }
                else { $row.status = 'unavailable'; $row.reason = 'path exceeds projection bound' }
            } elseif ($value.Length -le 8192) {
                $hash = [Security.Cryptography.SHA256]::Create()
                try { $row.sha256 = ([BitConverter]::ToString($hash.ComputeHash([Text.Encoding]::UTF8.GetBytes($value)))).Replace('-','').ToLowerInvariant() }
                finally { $hash.Dispose() }
            } else { $row.status = 'unavailable'; $row.reason = 'hash input exceeds projection bound' }
        }
        $paths[$name] = $row
    }
    $encoded = $paths | ConvertTo-Json -Depth 4 -Compress
    if ([Text.Encoding]::UTF8.GetByteCount($encoded) -gt 2048) {
        return @{status='unavailable'; reason='path projection exceeds byte bound'}
    }
    return $paths
}

function Invoke-HostedPhase {
    param(
        [Parameter(Mandatory)][hashtable]$Options,
        [Parameter(Mandatory)][ValidateRange(1,1200000)][int]$LimitMilliseconds,
        [Parameter(Mandatory)][string]$Output,
        [Parameter(Mandatory)][string]$Receipt
    )
    $clock = [Diagnostics.Stopwatch]::StartNew()
    $started = [DateTime]::UtcNow
    $phaseResult = @{status='starting'; limit_seconds=($LimitMilliseconds / 1000); timed_out=$false; phase_process_joined=$false}
    # This deadline includes diagnostic work; cleanup retains its separate bound.
    $phaseResult.started_utc = $started.ToString('o')
    $phaseResult.deadline_utc = $started.AddMilliseconds($LimitMilliseconds).ToString('o')
    $phaseResult.observation_policy = 'best-effort; absence supplies no identity or case credit'
    $phaseResult.loader_events = @{status='unavailable'; reason='no bounded ordinary event query'; window_seconds=90; event_limit=64; byte_limit=65536}
    $child = $null
    try {
        $child = Start-Process @Options
        $phaseHandle = $child.Handle
        $phaseResult.phase_owner_pid = $child.Id
        # Keep the retained field for consumers, but label its source explicitly.
        $phaseResult.phase_owner_executable = $Options.FilePath
        $phaseResult.phase_owner_executable_kind = 'requested; not an observed image'
        try { $phaseResult.intended_parent_paths = Get-HostedStartupPaths }
        catch { $phaseResult.intended_parent_paths = @{status='unavailable'; error_type=$_.Exception.GetType().Name} }
        $phaseResult.observed_owner = @{pid=$child.Id; handle_captured=$true
            image=@{status='unavailable'; reason='Process image query has no timeout'}
            birth=@{status='unavailable'; reason='Process birth query has no timeout'}
            token=@{status='unavailable'; reason='Process token query has no timeout'}}
        try {
            if ([Text.Encoding]::UTF8.GetByteCount(($phaseResult | ConvertTo-Json -Depth 6)) -gt 4096) {
                $phaseResult.intended_parent_paths = @{status='unavailable'; reason='parent receipt exceeds projection bound'}
            }
        } catch { $phaseResult.projection_error_type = $_.Exception.GetType().Name }
        try { $phaseResult | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $Receipt }
        catch { $phaseResult.receipt_write_error_type = $_.Exception.GetType().Name }
        $remaining = [Math]::Max(0, $LimitMilliseconds - [int][Math]::Min($LimitMilliseconds, $clock.ElapsedMilliseconds))
        if (-not $child.WaitForExit($remaining)) {
            $phaseResult.status = 'timed-out'
            $phaseResult.timed_out = $true
            # The exact Python owner holds the kill-on-close job for this tree.
            $child.Kill()
            if (-not $child.WaitForExit(10000)) { throw 'Timed-out phase did not exit after owned cleanup' }
            $phaseResult.exit_code = $child.ExitCode
        } else {
            $phaseResult.exit_code = $child.ExitCode
            $phaseResult.status = if ($child.ExitCode -eq 0) { 'completed' } else { 'failed' }
        }
        $phaseResult.phase_process_joined = $true
        try {
            $phaseResult.exit_observed_utc = [DateTime]::UtcNow.ToString('o')
            $phaseResult.exit_code_hex = '0x{0:x8}' -f ([long]$phaseResult.exit_code -band 4294967295L)
        } catch { $phaseResult.exit_projection_error_type = $_.Exception.GetType().Name }
    } catch {
        $failure = $_.Exception
        if (-not $phaseResult.timed_out) { $phaseResult.status = 'error' }
        if ($child) {
            try {
                if (-not $child.HasExited) { $child.Kill() }
                $phaseResult.phase_process_joined = $child.WaitForExit(10000)
            } catch {
                $phaseResult.cleanup_error_type = $_.Exception.GetType().Name
            }
        }
        New-Item -ItemType Directory -Path $Output -Force | Out-Null
        @{status='unmeasured'; error_type=$failure.GetType().Name; hresult=$failure.HResult} |
            ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $Output 'phase-error.json')
    } finally {
        try { $phaseResult | ConvertTo-Json -Depth 6 | Set-Content -Encoding utf8 $Receipt }
        catch { $phaseResult.receipt_write_error_type = $_.Exception.GetType().Name }
    }
    return $phaseResult
}
