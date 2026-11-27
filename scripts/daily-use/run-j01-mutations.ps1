param(
    [Parameter(Mandatory = $true)]
    [string]$StateEngineEntry,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$Candidate,
    [string]$ComputeCommand = 'daily-use-j01-mutations',
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$ArtifactRoot = $env:GRAPT_ARTIFACT_ROOT,
    [string]$ReceiptRoot = $env:GRAPT_RECEIPT_ROOT
)

# DAILY-USE-BACKEND-01 J01 mutation battery on the governed carrier.
#
# This mirrors scripts/run-linux.ps1 rather than replacing it. Two deliberate
# differences, both required by this campaign's charter:
#   - it uses remote-compute/daily-use-linux.json, a campaign-local profile
#     derived from the accepted one with domain1 removed, so no class here can
#     spill into a domain this campaign does not own;
#   - it runs a campaign mutation command the accepted profile does not name.
#     The command is a PARAMETER validated against the profile, so a
#     second battery does not need a second copy of this script that could drift
#     from it; the default is unchanged, so every existing invocation still runs
#     J01. `daily-use-form-mutations` is the DUF form battery.
#     `daily-use-windows-build` cross-builds the candidate product and CLI for
#     x86_64-pc-windows-msvc with cargo-xwin ON THE LINUX WORKSTATION. That is
#     Linux execution producing a Windows TARGET -- it is not the Windows-host
#     fallback the shared scheduler forbids, and it is the same route the
#     accepted desktop host was built by. It is added HERE, in the campaign's
#     own seam, rather than by widening scripts/run-linux.ps1's shared route
#     inventory.
# Everything else is inherited: the same cache namespace and key, the same
# toolchain pins, the same bounds, and the SAME shared carrier storage budget
# check before and after the job. The budget is never bypassed: a refusal is a
# coordination dependency, not a reason to retry, relocate caches or
# weaken the gate.

$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot '../build/operator_environment.ps1')
if (-not $PSBoundParameters.ContainsKey('ArtifactRoot')) { $ArtifactRoot = $env:GRAPT_ARTIFACT_ROOT }
if (-not $PSBoundParameters.ContainsKey('ReceiptRoot')) { $ReceiptRoot = $env:GRAPT_RECEIPT_ROOT }

if ([string]::IsNullOrWhiteSpace($ArtifactRoot)) {
    throw 'Set GRAPT_ARTIFACT_ROOT or pass -ArtifactRoot to the immutable artifact source.'
}
if ([string]::IsNullOrWhiteSpace($ReceiptRoot)) {
    $ReceiptRoot = Join-Path ([System.IO.Path]::GetTempPath()) 'secode-remote-compute-receipts'
}
if (-not (Test-Path -LiteralPath $ArtifactRoot -PathType Container)) {
    throw "Grapt immutable artifact source root is absent: $ArtifactRoot"
}

$profile = Join-Path $RepoRoot (Join-Path 'remote-compute' 'daily-use-linux.json')
if ($env:SECODE_COMPUTE_PROFILE_DIR) { $profile = Join-Path $env:SECODE_COMPUTE_PROFILE_DIR (Split-Path -Leaf $profile) }
if (-not (Test-Path -LiteralPath $profile -PathType Leaf)) {
    throw "campaign-local compute profile is absent: $profile"
}
$commandNames = (Get-Content -LiteralPath $profile -Raw -Encoding UTF8 | ConvertFrom-Json).commands.PSObject.Properties.Name
if ($ComputeCommand -notin $commandNames) {
    throw "compute command is absent from the campaign profile: $ComputeCommand"
}
$budgetHelper = Join-Path $RepoRoot 'remote-compute\cache_budget.py'

& python $budgetHelper --phase pre
if ($LASTEXITCODE -ne 0) {
    [Console]::Error.WriteLine('governed job was not started because the pre-job cache budget refused')
    exit 125
}

& node $StateEngineEntry remote-compute `
    --project secode-rust `
    --profile $profile `
    --compute-command $ComputeCommand `
    --candidate $Candidate `
    --repo-root $RepoRoot `
    --artifact-root $ArtifactRoot `
    --evidence-dir $ReceiptRoot

$jobExit = $LASTEXITCODE
& python $budgetHelper --phase post
$postBudgetExit = $LASTEXITCODE
if ($postBudgetExit -ne 0) {
    [Console]::Error.WriteLine("governed job returned exit $jobExit, but the post-job cache budget refused")
    exit 125
}
exit $jobExit
