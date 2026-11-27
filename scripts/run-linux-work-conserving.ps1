param(
    [Parameter(Mandatory = $true)]
    [string]$StateEngineEntry,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$Candidate,
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..')).Path,
    [string]$ArtifactRoot = $env:GRAPT_ARTIFACT_ROOT,
    [string]$ReceiptRoot = $env:GRAPT_RECEIPT_ROOT
)

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'build/operator_environment.ps1')
if (-not $PSBoundParameters.ContainsKey('ArtifactRoot')) { $ArtifactRoot = $env:GRAPT_ARTIFACT_ROOT }
if (-not $PSBoundParameters.ContainsKey('ReceiptRoot')) { $ReceiptRoot = $env:GRAPT_RECEIPT_ROOT }
$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)

if ([string]::IsNullOrWhiteSpace($ArtifactRoot)) {
    throw 'Set GRAPT_ARTIFACT_ROOT or pass -ArtifactRoot to the immutable artifact source.'
}
if ([string]::IsNullOrWhiteSpace($ReceiptRoot)) {
    $ReceiptRoot = Join-Path ([System.IO.Path]::GetTempPath()) 'secode-remote-compute-receipts'
}

function Test-SeCodePathInside {
    param(
        [Parameter(Mandatory = $true)]
        [string]$Root,
        [Parameter(Mandatory = $true)]
        [string]$CandidatePath
    )

    $rootParts = @([System.IO.Path]::GetFullPath($Root) -split '[\\/]' | Where-Object { $_.Length -gt 0 })
    $candidateParts = @([System.IO.Path]::GetFullPath($CandidatePath) -split '[\\/]' | Where-Object { $_.Length -gt 0 })
    if ($candidateParts.Count -lt $rootParts.Count) {
        return $false
    }
    for ($index = 0; $index -lt $rootParts.Count; $index += 1) {
        if ($candidateParts[$index] -ine $rootParts[$index]) {
            return $false
        }
    }
    return $true
}

if (Test-SeCodePathInside -Root $RepoRoot -CandidatePath $ReceiptRoot) {
    throw "remote-compute receipt root must be outside the repository: $ReceiptRoot"
}

$SingleRouteScript = Join-Path $PSScriptRoot 'run-linux.ps1'
$ChildModule = Join-Path $PSScriptRoot 'work-conserving-child.psm1'
$BudgetHelper = Join-Path (Resolve-Path (Join-Path $PSScriptRoot '..')).Path 'remote-compute\cache_budget.py'

foreach ($requiredFile in @($StateEngineEntry, $SingleRouteScript, $ChildModule, $BudgetHelper)) {
    if (-not (Test-Path -LiteralPath $requiredFile -PathType Leaf)) {
        throw "required work-conserving runner file is absent: $requiredFile"
    }
}
Import-Module -Name $ChildModule -Force
foreach ($requiredRoot in @($RepoRoot, $ArtifactRoot)) {
    if (-not (Test-Path -LiteralPath $requiredRoot -PathType Container)) {
        throw "required work-conserving runner root is absent: $requiredRoot"
    }
}

& python $BudgetHelper --phase pre
if ($LASTEXITCODE -ne 0) {
    [Console]::Error.WriteLine('work-conserving batch refused by the pre-job cache budget')
    exit 125
}

$BatchRoot = Join-Path $ReceiptRoot 'work-conserving-battery'
if (Test-Path -LiteralPath $BatchRoot) {
    throw "work-conserving receipt root must be absent: $BatchRoot"
}
New-Item -ItemType Directory -Path $BatchRoot | Out-Null

$ExpectedTree = (& git -C $RepoRoot rev-parse "$Candidate`^{tree}").Trim()
if ($LASTEXITCODE -ne 0 -or $ExpectedTree -notmatch '^[0-9a-f]{40}$') {
    throw "candidate tree identity could not be derived: $Candidate"
}
$parentLine = (& git -C $RepoRoot show -s --format=%P $Candidate).Trim()
if ($LASTEXITCODE -ne 0) {
    throw "candidate ordered parents could not be derived: $Candidate"
}
$ExpectedParents = if ($parentLine.Length -eq 0) { @() } else { @($parentLine -split ' ') }

$Pending = [System.Collections.Generic.Queue[object]]::new()
@(
    [pscustomobject]@{ Position = 1; Command = 'full-gate' },
    [pscustomobject]@{ Position = 2; Command = 'store-mutations' },
    [pscustomobject]@{ Position = 3; Command = 'runtime-mutations' },
    [pscustomobject]@{ Position = 4; Command = 'event-mutations' },
    [pscustomobject]@{ Position = 5; Command = 'provider-mutations' }
) | ForEach-Object { $Pending.Enqueue($_) }

$Running = [System.Collections.Generic.List[object]]::new()
$Completed = [System.Collections.Generic.List[object]]::new()
$AdmitMore = $true

function Start-Route {
    param([Parameter(Mandatory = $true)]$Route)

    $routeRoot = Join-Path $BatchRoot ("{0:D2}-{1}" -f $Route.Position, $Route.Command)
    New-Item -ItemType Directory -Path $routeRoot | Out-Null
    $stdoutPath = Join-Path $routeRoot 'wrapper.stdout.log'
    $stderrPath = Join-Path $routeRoot 'wrapper.stderr.log'
    $evidenceRoot = Join-Path $routeRoot 'runner-receipts'

    $arguments = @(
        '-StateEngineEntry', $StateEngineEntry,
        '-ComputeCommand', $Route.Command,
        '-Candidate', $Candidate,
        '-RepoRoot', $RepoRoot,
        '-ArtifactRoot', $ArtifactRoot,
        '-ReceiptRoot', $evidenceRoot
    )
    $process = Start-SeCodeLiteralChild -ScriptPath $SingleRouteScript -Arguments $arguments

    [pscustomobject]@{
        Route = $Route
        Process = $process
        StdoutTask = $process.StandardOutput.ReadToEndAsync()
        StderrTask = $process.StandardError.ReadToEndAsync()
        Stopwatch = [System.Diagnostics.Stopwatch]::StartNew()
        StdoutPath = $stdoutPath
        StderrPath = $stderrPath
        EvidenceRoot = $evidenceRoot
    }
}

while ($Pending.Count -gt 0 -or $Running.Count -gt 0) {
    while ($AdmitMore -and $Pending.Count -gt 0 -and $Running.Count -lt 2) {
        $Running.Add((Start-Route -Route $Pending.Dequeue()))
    }
    if (-not $AdmitMore -and $Running.Count -eq 0) {
        break
    }

    $finished = $null
    while ($null -eq $finished) {
        foreach ($active in $Running) {
            if ($active.Process.HasExited) {
                $finished = $active
                break
            }
        }
        if ($null -eq $finished) {
            Start-Sleep -Milliseconds 200
        }
    }

    $finished.Process.WaitForExit()
    $finished.Stopwatch.Stop()
    $stdout = $finished.StdoutTask.GetAwaiter().GetResult()
    $stderr = $finished.StderrTask.GetAwaiter().GetResult()
    [System.IO.File]::WriteAllText($finished.StdoutPath, $stdout, $Utf8NoBom)
    [System.IO.File]::WriteAllText($finished.StderrPath, $stderr, $Utf8NoBom)
    $exitCode = $finished.Process.ExitCode
    $Completed.Add([pscustomobject]@{
        position = $finished.Route.Position
        command = $finished.Route.Command
        exit_code = $exitCode
        duration_ms = $finished.Stopwatch.ElapsedMilliseconds
        stdout_path = $finished.StdoutPath
        stdout_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $finished.StdoutPath).Hash.ToLowerInvariant()
        stderr_path = $finished.StderrPath
        stderr_sha256 = (Get-FileHash -Algorithm SHA256 -LiteralPath $finished.StderrPath).Hash.ToLowerInvariant()
        evidence_root = $finished.EvidenceRoot
    })
    [void]$Running.Remove($finished)
    $finished.Process.Dispose()

    if ($exitCode -ne 0) {
        $AdmitMore = $false
    }
}

$ordered = @($Completed | Sort-Object position)
$receiptRows = foreach ($row in $ordered) {
    $receipts = if (Test-Path -LiteralPath $row.evidence_root -PathType Container) {
        @(Get-ChildItem -LiteralPath $row.evidence_root -Recurse -Filter 'remote-compute-receipt.json' -File)
    } else {
        @()
    }
    if ($receipts.Count -gt 1) {
        throw "route returned duplicate generic receipts: $($row.command) returned $($receipts.Count)"
    }
    $stderrText = Get-Content -Raw -Encoding UTF8 -LiteralPath $row.stderr_path
    $isPreBootstrapRefusal = $row.exit_code -eq 125 -and
        $receipts.Count -eq 0 -and
        $stderrText.Contains('literal child bootstrap refused:')
    if ($receipts.Count -eq 0 -and -not $isPreBootstrapRefusal) {
        throw "route returned no generic receipt outside the named child bootstrap refusal: $($row.command)"
    }
    $receipt = if ($receipts.Count -eq 1) { $receipts[0] } else { $null }
    $generic = if ($null -eq $receipt) {
        $null
    } else {
        Get-Content -Raw -Encoding UTF8 -LiteralPath $receipt.FullName | ConvertFrom-Json
    }
    if ($row.exit_code -eq 0) {
        if ($generic.candidate.commit -ne $Candidate -or $generic.candidate.tree -ne $ExpectedTree) {
            throw "generic receipt candidate identity mismatch: $($row.command)"
        }
        if ((@($generic.candidate.parents) -join ',') -ne ($ExpectedParents -join ',')) {
            throw "generic receipt ordered-parent identity mismatch: $($row.command)"
        }
        if (-not $generic.candidate_custody.tracked_clean -or
            -not $generic.candidate_custody.staged_clean -or
            @($generic.candidate_custody.unexpected_untracked).Count -ne 0) {
            throw "generic receipt candidate custody is not clean: $($row.command)"
        }
        if ($generic.resource.domain.id -notin @('domain0', 'domain1') -or
            $generic.resource.domain.cache_namespace -notin @('carrier0', 'carrier1') -or
            $generic.resource.domain.cpu_policy -ne 'host-shared' -or
            $generic.resource.domain.memory.policy -ne 'runner-unbounded') {
            throw "generic receipt did not bind one stable cache carrier: $($row.command)"
        }
    }
    [ordered]@{
        position = $row.position
        command = $row.command
        exit_code = $row.exit_code
        duration_ms = $row.duration_ms
        stdout_sha256 = $row.stdout_sha256
        stderr_sha256 = $row.stderr_sha256
        generic_receipt_path = if ($null -eq $receipt) { $null } else { $receipt.FullName }
        generic_receipt_sha256 = if ($null -eq $receipt) { $null } else { (Get-FileHash -Algorithm SHA256 -LiteralPath $receipt.FullName).Hash.ToLowerInvariant() }
        log_archive_sha256 = if ($null -eq $generic) { $null } else { $generic.log_archive.sha256 }
        resource_domain = if ($null -eq $generic) { $null } else { $generic.resource.domain.id }
        cache_namespace = if ($null -eq $generic) { $null } else { $generic.resource.domain.cache_namespace }
    }
}

$allGreen = $receiptRows.Count -eq 6 -and @($receiptRows | Where-Object { $_.exit_code -ne 0 }).Count -eq 0
$composition = [ordered]@{
    contract = 'grapt-work-conserving-battery/v1'
    candidate = [ordered]@{ commit = $Candidate; tree = $ExpectedTree; parents = $ExpectedParents }
    max_in_flight = 2
    admitted_all_routes = $receiptRows.Count -eq 6
    ok = $allGreen
    routes = @($receiptRows)
}
$compositionPath = Join-Path $BatchRoot 'work-conserving-receipt.json'
[System.IO.File]::WriteAllText(
    $compositionPath,
    (($composition | ConvertTo-Json -Depth 8 -Compress) + "`n"),
    $Utf8NoBom
)
$compositionHash = (Get-FileHash -Algorithm SHA256 -LiteralPath $compositionPath).Hash.ToLowerInvariant()
[System.IO.File]::WriteAllText(
    (Join-Path $BatchRoot 'work-conserving-receipt.sha256'),
    ($compositionHash + "`n"),
    $Utf8NoBom
)

& python $BudgetHelper --phase post
$postBudgetExit = $LASTEXITCODE
if ($postBudgetExit -ne 0) {
    [Console]::Error.WriteLine("work-conserving batch completed with ok=$allGreen but the post-job cache budget refused")
    exit 125
}

if (-not $allGreen) {
    exit 1
}
