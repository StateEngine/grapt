# The slice owner runs this after one reviewer ACCEPTs the exact commit.
[CmdletBinding()]
param(
    [Parameter(Mandatory)][string]$Branch,
    [Parameter(Mandatory)][string]$AcceptedCommit,
    [Parameter(Mandatory)][string]$LinuxRoot,
    [string]$LinuxHome = $env:GRAPT_LINUX_HOME,
    [string]$WorktreeParent = $env:SECODE_WORKTREE_PARENT,
    [string]$LinuxHost = $env:GRAPT_LINUX_HOST,
    [string]$Owner = $env:STATEENGINE_AGENT_ID,
    [string]$ControllerHost = $env:SECODE_WINDOWS_HOST,
    [string]$SupplierRoot = $env:SECODE_STATEENGINE_PAYLOAD_ROOT,
    [string]$NodeRoot = $env:GRAPT_NODE_ROOT,
    [string]$Repository = $env:SECODE_CANONICAL_REPOSITORY,
    [ValidateRange(0,86400)][double]$LeaseWaitSeconds = 2700,
    [Parameter(Mandatory)][ValidateSet('1','2')][string]$Tier,
    [switch]$FullGate,
    [switch]$PriorityLanding,
    [Alias('SkipFullIfGreen', 'skip-full-if-green')][string]$GateLog,
    [string]$RefundTicket,
    [string]$ExpectedBase,
    [string]$ReviewedBase,
    [string]$RetainedFullCommit,
    [string]$RetainedFullBase,
    [string]$RetainedFullLog
)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build/operator_environment.ps1')
$operatorDefaults = @{LinuxHome='GRAPT_LINUX_HOME'; WorktreeParent='SECODE_WORKTREE_PARENT'; LinuxHost='GRAPT_LINUX_HOST'; ControllerHost='SECODE_WINDOWS_HOST'; SupplierRoot='SECODE_STATEENGINE_PAYLOAD_ROOT'; NodeRoot='GRAPT_NODE_ROOT'; Repository='SECODE_CANONICAL_REPOSITORY'}
foreach ($name in $operatorDefaults.Keys) {
    if (-not $PSBoundParameters.ContainsKey($name)) { Set-Variable -Name $name -Value ([Environment]::GetEnvironmentVariable($operatorDefaults[$name], 'Process')) }
}
if (-not $Owner) { $Owner = 'manual' }
if ($Owner -notmatch '^[a-zA-Z0-9_.:@-]{1,128}$') { throw 'Use a plain owner seat.' }
if ($Branch -notmatch '^[a-z][a-z0-9-]*$' -or $AcceptedCommit -notmatch '^[0-9a-f]{40}$' -or
    ($LinuxRoot -notmatch '^/[a-zA-Z0-9_/-]+$' -or
     -not (($LinuxHome -and $LinuxRoot.StartsWith($LinuxHome.TrimEnd('/')+'/')) -or
           ($WorktreeParent -and $LinuxRoot.StartsWith($WorktreeParent.TrimEnd('/')+'/')))) -or $LinuxHost -notmatch '^[a-zA-Z0-9_-]+$') {
    throw 'Use a plain branch, full accepted commit, and an absolute Linux worktree path.'
}
if ($SupplierRoot -notmatch '^/[a-zA-Z0-9_/-]+$' -or
    $SupplierRoot.Split('/') -contains '..' -or $SupplierRoot.Contains('//')) {
    throw 'SupplierRoot requires a plain absolute Linux path.'
}
if ($NodeRoot -notmatch '^/[a-zA-Z0-9_./-]+$' -or
    $NodeRoot.Split('/') -contains '..' -or $NodeRoot.Contains('//')) {
    throw 'NodeRoot requires a plain absolute Linux path.'
}
if ($GateLog -and $GateLog -notmatch '^/[a-zA-Z0-9_./-]+$') {
    throw 'The reusable gate log needs an absolute Linux path without shell metacharacters.'
}
if ($RefundTicket -and $RefundTicket -notmatch '^[0-9-]{1,80}$') {
    throw 'RefundTicket requires the single-use ticket returned by replace_waiter.py.'
}
if ($ExpectedBase -and $ExpectedBase -notmatch '^[0-9a-f]{40}$') {
    throw 'ExpectedBase requires the exact canonical commit used for attribution.'
}
if ($ControllerHost -notmatch '^[a-zA-Z0-9_][a-zA-Z0-9_-]*$' -or $ControllerHost.Length -gt 128) {
    throw 'Use a plain SSH alias for the canonical controller.'
}
$reviewed = [bool]($ReviewedBase -or $RetainedFullCommit -or $RetainedFullBase -or $RetainedFullLog)
if ($reviewed -and $Tier -ne '2') {
    throw 'Retained full gates support only Tier 2 reviewed input compositions; Tier 1 requires a fresh full gate on the composed commit.'
}
if ($reviewed -and (-not $GateLog -or
    $ReviewedBase -notmatch '^[0-9a-f]{40}$' -or $RetainedFullCommit -notmatch '^[0-9a-f]{40}$' -or
    $RetainedFullBase -notmatch '^[0-9a-f]{40}$' -or $RetainedFullLog -notmatch '^/[a-zA-Z0-9_./-]+$')) {
    throw 'Reviewed input composition requires a completed GateLog, ReviewedBase and retained full commit/base/log.'
}
function Invoke-RepositoryGit([string[]]$Arguments) {
    # Windows PowerShell 5.1 turns a native stderr line into a terminating
    # NativeCommandError under Stop; a healthy git fetch writes to stderr.
    # Judge the exit code only, and keep the stdout shape unchanged.
    # Clear the sentinel first: an invocation that reports no fresh status
    # must refuse, never inherit an earlier native command's zero.
    $previous = $ErrorActionPreference
    $ErrorActionPreference = 'Continue'
    try {
        $global:LASTEXITCODE = $null
        $result = & git -C $Repository @Arguments
        $code = $LASTEXITCODE
        if ($null -eq $code) { throw 'git did not report an exit status.' }
        if ($code -ne 0) { throw "git failed: $Arguments" }
        return $result
    } finally {
        $ErrorActionPreference = $previous
    }
}
function Test-StateEnginePath([string]$Path) {
    return $Path.Equals('.stateengine', [StringComparison]::OrdinalIgnoreCase) -or
        $Path.StartsWith('.stateengine/', [StringComparison]::OrdinalIgnoreCase)
}
function Get-DirtySnapshot {
    $raw = (Invoke-RepositoryGit -Arguments @('status', '--porcelain=v1', '-z', '--untracked-files=all')) -join "`n"
    $rows = $raw.Split([char]0)
    $paths = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $preserved = [System.Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)
    $status = [System.Collections.Generic.List[string]]::new()
    for ($i = 0; $i -lt $rows.Length; $i++) {
        if (-not $rows[$i]) { continue }
        if ($rows[$i].Length -lt 4) { throw 'Unparseable Git status; preserve the checkout.' }
        $path = $rows[$i].Substring(3)
        [void]$paths.Add($path)
        # Live runtime files can change, appear or disappear during a landing.
        # Keep their paths for overlap refusal, but never hash untracked runtime.
        if ($rows[$i].Substring(0, 2) -eq '??' -and (Test-StateEnginePath $path)) { continue }
        [void]$preserved.Add($path)
        $status.Add($rows[$i])
        if ($rows[$i].Substring(0, 2) -match '[RC]') {
            $i++
            if ($i -ge $rows.Length -or -not $rows[$i]) { throw 'Incomplete Git rename status.' }
            [void]$paths.Add($rows[$i])
            [void]$preserved.Add($rows[$i])
            $status.Add($rows[$i])
        }
    }
    $snapshot = @{}
    foreach ($path in $preserved) {
        $absolute = Join-Path $Repository $path
        if (Test-Path -LiteralPath $absolute -PathType Leaf) {
            $snapshot[$path] = (Get-FileHash -LiteralPath $absolute -Algorithm SHA256).Hash
        } elseif (Test-Path -LiteralPath $absolute) {
            throw "Dirty path is not a regular file: $path"
        } else { $snapshot[$path] = 'absent' }
    }
    # Porcelain XY and work bytes alone cannot see an index-only edit that
    # leaves a file MM. Capture its index modes/blob ids/stages as well.
    $index = ''
    if ($paths.Count) {
        $arguments = @('ls-files', '--stage', '-z', '--') + @($paths)
        $index = (Invoke-RepositoryGit -Arguments $arguments) -join "`n"
    }
    return @{ Status = ($status -join [char]0); Files = $snapshot; Index = $index; Paths = $paths }
}
function Assert-DirtyPreserved($Before) {
    $after = Get-DirtySnapshot
    if ($after.Status -cne $Before.Status -or $after.Index -cne $Before.Index -or
        $after.Files.Count -ne $Before.Files.Count) {
        throw 'Canonical dirty paths changed during qualification; main was not moved.'
    }
    foreach ($path in $Before.Files.Keys) {
        if ($after.Files[$path] -cne $Before.Files[$path]) { throw "Dirty file changed: $path" }
    }
}
. "$PSScriptRoot/build/landing_transport.ps1"
function Invoke-LinuxCommand([string]$Command) {
    # Keep native argv short; helper bytes travel separately over SCP.
    $marker = 'GRAPT_LANDING_DONE_' + [Guid]::NewGuid().ToString('N')
    $wrapped = "(`n$Command`n)`ngrapt_status=`$?`nprintf '\n${marker}:%s\n' `"`$grapt_status`"`nexit `"`$grapt_status`""
    $encoded = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($wrapped))
    if ($encoded.Length -gt 6000) { throw 'Linux command exceeds the Windows SSH argument bound.' }
    $script:LinuxExitCode = $null
    $global:LASTEXITCODE = $null
    $ssh = (Get-Command ssh -CommandType Application | Select-Object -First 1).Source
    $script:LinuxExitCode = Invoke-LandingTransport $ssh "-n $LinuxHost `"printf %s $encoded | base64 -d | bash -l`"" $marker
    if ($null -eq $script:LinuxExitCode) { throw 'SSH did not report an exit status.' }
}
function Get-CompositionHelper([string]$Name) {
    if ($Name -eq 'windows_version_worker.py') {
        return Join-Path $PSScriptRoot ('release/' + $Name)
    }
    return Join-Path $PSScriptRoot ('build/' + $Name)
}
function Invoke-Composition([ValidateSet('compose','verify','qualify','docs','verify-docs')][string]$Mode, [hashtable]$Data) {
    $helpers = @('durable_jobs.py', 'windows_version_worker.py', 'flaky_quarantine.py', 'nextest_gate.py', 'gate_parallel.py', 'windows_native.py', 'windows_native_result.py', 'windows_native_worker.py', 'windows_native_environment.py', 'windows_native_survivors.py', 'windows_native.ps1', 'nightly_revert.py', 'landing.py', 'landing_docs.py', 'full_capacity.py','cpu_priority.py', 'operator_paths.py', 'operator_environment.ps1', 'git_environment.py', 'build_environment.py', 'heavy.py', 'heavy_queue.py', 'heavy_runs.py', 'heavy_drain.py', 'heavy_hardware.py', 'heavy_sampling.py', 'external_holders.py', 'disk_stall.py', 'ram_targets.py', 'gate_policy.py', 'gate_scope.py', 'reviewed_composition.py', 'gate.py', 'gate_plan.py', 'publication_scan.py', 'test_features.py', 'slow_tests.py', 'windows_gate.py', 'windows_scope.py', 'receipt.py', 'target_discovery.py','duplicate_targets.py','evict_targets.py', 'cargo_observation.py', 'cargo_targets.py', 'orphan_cache.py', 'gate_retry.py', 'gate_evidence.py', 'gate_scratch.py', 'gate_timing.py', 'signal_trace.py')
    foreach ($name in $helpers) {
        if ((Get-Item -LiteralPath (Get-CompositionHelper $name)).Length -gt 65536) {
            throw 'Landing helper exceeds its transport bound.'
        }
    }
    $requestData = $Data.Clone()
    if ($Mode -eq 'compose' -and $RefundTicket) { $requestData.refund = $RefundTicket }
    $payload = [Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes(($requestData | ConvertTo-Json -Compress)))
    # landing.py keeps lease-holding qualification and composition in its priority lane.
    $runner = 'python3 -B'
    $directory = '/tmp/secode-land-helper-' + [Guid]::NewGuid().ToString('N')
    Invoke-LinuxCommand -Command "umask 077 && mkdir -- '$directory'"
    if ($script:LinuxExitCode -ne 0) { throw 'Cannot create the private Linux helper directory.' }
    $cleanupFiles = ($helpers | ForEach-Object { "'$_'" }) -join ' '
    $cleanup = "if test -d '$directory'; then (cd -- '$directory' && rm -f -- $cleanupFiles) && rmdir -- '$directory'; fi"
    try {
        foreach ($name in $helpers) {
            $global:LASTEXITCODE = $null
            # Same PS 5.1 stderr rule as Invoke-RepositoryGit: exit code only.
            $previous = $ErrorActionPreference
            $ErrorActionPreference = 'Continue'
            try { & scp -q (Get-CompositionHelper $name) ("{0}:{1}/{2}" -f $LinuxHost, $directory, $name) } finally { $ErrorActionPreference = $previous }
            if ($null -eq $LASTEXITCODE -or $LASTEXITCODE -ne 0) { throw 'Linux helper copy failed.' }
        }
        # An older candidate may not contain the installed helper. Use this copy
        # under the existing lease, and remove it on remote or local failure.
        $lines = @(
            ('trap ' + "'" + $cleanup.Replace("'", "'\''") + "' EXIT"),
            "cd '$LinuxRoot' || exit"
        )
        if ($Mode -notin @('docs', 'verify-docs')) {
            $lines += @(
            "export SECODE_STATEENGINE_PAYLOAD_ROOT='$SupplierRoot'",
            "export NODE='$NodeRoot/bin/node'",
            "export PATH='$NodeRoot/bin':`$PATH",
            'test -x "$NODE" || exit 1',
            '"$NODE" -e ''if (Number(process.versions.node.split(".")[0]) !== 24) process.exit(1)'' || exit 1'
            )
        }
        $lines += ("{0} '{1}/landing.py' {2} '{3}'" -f $runner, $directory, $Mode, $payload)
        Invoke-LinuxCommand -Command ($lines -join "`n")
        if ($script:LinuxExitCode -eq 75) { throw 'Heavy lock wait timed out; see the queue position and holder above. Main was not moved.' }
    } finally {
        $result = $script:LinuxExitCode
        try {
            Invoke-LinuxCommand -Command $cleanup
            if ($script:LinuxExitCode -ne 0) { Write-Warning 'Linux helper cleanup failed.' }
        } catch { Write-Warning 'Linux helper cleanup did not complete.' }
        $script:LinuxExitCode = $result
    }
}
# Serialize canonical inspection and mutation, including dirty-path preservation.
# Queue before the first Git operation. The exclusive handle still owns the
# lease, including automatic release if this controller crashes.
. "$PSScriptRoot/build/landing_lease.ps1"
$canonicalLease = Open-CanonicalLandingLease -Repository $Repository -Branch $Branch `
    -AcceptedCommit $AcceptedCommit -WaitSeconds $LeaseWaitSeconds -Priority:$PriorityLanding
$leaseElapsed = [Diagnostics.Stopwatch]::StartNew()
$composed = $false
$landing = $null
$compositionBranch = $null
try {
# No file walks or builds on the live Windows host. Git alone inspects its checkout.
if ((Invoke-RepositoryGit -Arguments @('branch', '--show-current')) -ne 'main') { throw 'Canonical checkout must be on main.' }
$dirty = Get-DirtySnapshot
$old = Invoke-RepositoryGit -Arguments @('rev-parse', 'main')
if ($ExpectedBase -and $old -ne $ExpectedBase) {
    throw 'Canonical main differs from the attributed base; diagnose again before reverting.'
}
Invoke-RepositoryGit -Arguments @('fetch', "$($LinuxHost):$LinuxRoot", "+refs/heads/${Branch}:refs/remotes/build-bench/$Branch") | Out-Host
$candidate = Invoke-RepositoryGit -Arguments @('rev-parse', "refs/remotes/build-bench/$Branch")
if ($candidate -ne $AcceptedCommit) { throw 'Fetched branch differs from the reviewed commit.' }
$base = Invoke-RepositoryGit -Arguments @('merge-base', $old, $candidate)
if ($reviewed -and ($old -ne $ReviewedBase -or $base -ne $ReviewedBase)) {
    throw 'Reviewed composition must descend from the exact current main; recompose and obtain a new review.'
}
$incoming = ((Invoke-RepositoryGit -Arguments @('diff', '--no-renames', '--name-only', '-z', $base, $candidate)) -join "`n").Split([char]0)
foreach ($path in $incoming) {
    if (-not $path) { continue }
    if (Test-StateEnginePath $path) { throw "Incoming diff touches StateEngine runtime: $path" }
    foreach ($owned in $dirty.Paths) {
        if ($path.Equals($owned, [StringComparison]::OrdinalIgnoreCase) -or
            $path.StartsWith($owned + '/', [StringComparison]::OrdinalIgnoreCase) -or
            $owned.StartsWith($path + '/', [StringComparison]::OrdinalIgnoreCase)) {
            throw "Incoming diff overlaps preserved dirty path: $owned"
        }
    }
}
# Plain Markdown can be checked entirely by Git. Permissions, prompts, notices,
# symlinks and code-like files retain their ordinary qualification path.
$docsOnly = (-not $FullGate -and -not $reviewed)
$docRows = ((Invoke-RepositoryGit -Arguments @('diff', '--raw', '--no-abbrev', '--no-renames', '-z', $base, $candidate)) -join "`n").Split([char]0)
if ($docRows.Length -lt 3 -or $docRows.Length -gt 513 -or ($docRows.Length % 2) -ne 1) { $docsOnly = $false }
for ($i = 0; $docsOnly -and $i -lt $docRows.Length - 1; $i += 2) {
    $name = $docRows[$i + 1]
    if ($name -match '(?i)(^|/)(AGENTS|CLAUDE|SKILL)[.]md$' -or
        $docRows[$i] -notmatch '^:(000000|100644) (000000|100644) [a-f0-9]{40} [a-f0-9]{40} [AMD]$' -or
        ($name -notmatch '(?i)^docs/.+[.]md$' -and $name -notmatch '(?i)^(README|CONTRIBUTING|CHANGELOG)[.]md$')) {
        $docsOnly = $false
    }
}
# Keep the original gate exact; a disjoint rebase gets its own receipt below.
$log = "/tmp/secode-land-$AcceptedCommit.log"
# Tier retains the review classification. The pinned controller selects gate cost from paths.
$qualifiedLog = if ($GateLog) { $GateLog } else { $log }
$composition = "/tmp/secode-land-$AcceptedCommit-$([Guid]::NewGuid().ToString('N')).json"
$compositionDigest = [Security.Cryptography.SHA256]::Create()
try {
    $compositionBranch = 'secode-land-' + ([BitConverter]::ToString(
        $compositionDigest.ComputeHash([Text.Encoding]::UTF8.GetBytes($composition)))).Replace('-', '').ToLowerInvariant().Substring(0, 32)
} finally { $compositionDigest.Dispose() }
$data = @{accepted=$AcceptedCommit; old_base=$base; branch=$Branch;
    owner=$Owner; canonical=('{0}:{1}' -f $ControllerHost, $Repository.Replace('\','/'));
    gate_log=$qualifiedLog; full=[bool]$FullGate; run_gate=(-not [bool]$GateLog); receipt=$composition}
if ($reviewed) {
    $data.retained_full = @{ commit=$RetainedFullCommit; base=$RetainedFullBase; log=$RetainedFullLog }
}
if ($docsOnly) {
    Invoke-Composition -Mode 'docs' -Data $data
    $composed = $true
} elseif ($GateLog) {
    Invoke-Composition -Mode 'qualify' -Data $data
} else {
    # Both stages share class 0 while this process holds the canonical lease.
    Invoke-Composition -Mode 'compose' -Data $data
    $composed = $true
}
if ($script:LinuxExitCode -ne 0) { throw 'Linux gate failed; main was not moved.' }
$landingBase = Invoke-RepositoryGit -Arguments @('rev-parse', 'main')
if ($ExpectedBase -and $landingBase -ne $ExpectedBase) {
    throw 'Canonical main advanced after attribution; preserve the revert and diagnose again.'
}
if ($reviewed -and $landingBase -ne $ReviewedBase) {
    throw 'Canonical main advanced after review; preserve this composition and recompose for review.'
}
if (-not $composed -and $landingBase -ne $base) {
    Invoke-Composition -Mode 'compose' -Data $data
    if ($script:LinuxExitCode -ne 0) { throw 'Disjoint composition failed; main was not moved.' }
    $composed = $true
    $landingBase = Invoke-RepositoryGit -Arguments @('rev-parse', 'main')
}
$landing = $AcceptedCommit
if ($composed) {
    Invoke-RepositoryGit -Arguments @('fetch', "$($LinuxHost):$LinuxRoot", "+refs/heads/${compositionBranch}:refs/remotes/build-bench/$Branch") | Out-Host
    $landing = Invoke-RepositoryGit -Arguments @('rev-parse', "refs/remotes/build-bench/$Branch")
    $data.candidate = $landing
    $data.new_base = $landingBase
    $verifyMode = if ($docsOnly) { 'verify-docs' } else { 'verify' }
    Invoke-Composition -Mode $verifyMode -Data $data
    if ($script:LinuxExitCode -ne 0) { throw 'Composition receipt failed; main was not moved.' }
}
if ((Invoke-RepositoryGit -Arguments @('branch', '--show-current')) -ne 'main' -or
    (Invoke-RepositoryGit -Arguments @('rev-parse', 'main')) -ne $landingBase) {
    throw 'Canonical checkout changed after composition; preserve the candidate and retry.'
}
Assert-DirtyPreserved $dirty
# Independently refuse a no-op or stale remote proof before any canonical merge.
& git -C $Repository merge-base --is-ancestor $landingBase $landing
if ($LASTEXITCODE -eq 1) { throw 'Landing candidate does not descend from canonical main.' }
if ($LASTEXITCODE -ne 0) { throw 'Cannot verify landing candidate ancestry.' }
Invoke-RepositoryGit -Arguments @('merge', '--ff-only', $landing) | Out-Host
Assert-DirtyPreserved $dirty
Write-Output "LANDED $landing (was $landingBase)"
# Landing is already complete. A failed cleanup marker must not disguise that fact.
# Composition leaves the owner branch at AcceptedCommit, not the rebased landing.
# The finished marker must name that target's source so its strict HEAD check holds.
# Select the owner's target afresh; a lead runner's inherited target is not ours.
$finish = @(
    "cd '$LinuxRoot' || exit",
    'unset CARGO_TARGET_DIR',
    'source scripts/build-cache.sh || exit',
    "python3 scripts/build/disk_hygiene.py --finish '$AcceptedCommit'"
) -join "`n"
if (-not $docsOnly) { Invoke-LinuxCommand -Command $finish }
if (-not $docsOnly -and $script:LinuxExitCode -ne 0) {
    Write-Warning 'Landed successfully; automatic finished-target marking failed.'
}
} finally {
    if ($composed -and $landing -match '^[0-9a-f]{40}$') {
        try {
            Invoke-LinuxCommand -Command "cd '$LinuxRoot' && git update-ref -d 'refs/heads/$compositionBranch' '$landing'"
            if ($script:LinuxExitCode -ne 0) { Write-Warning 'Private composition ref cleanup refused; owner branch was preserved.' }
        } catch { Write-Warning 'Private composition ref cleanup did not complete; owner branch was preserved.' }
    }
    Close-CanonicalLandingLease $canonicalLease
    Write-Host ("Landing lease held {0:n1}s" -f $leaseElapsed.Elapsed.TotalSeconds)
}
