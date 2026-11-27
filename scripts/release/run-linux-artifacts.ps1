param(
    [Parameter(Mandatory = $true)]
    [string]$StateEngineEntry,
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-f]{40}$')]
    [string]$Candidate,
    [string]$ComputeCommand = 'release-linux-artifacts',
    [string]$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path,
    [string]$ArtifactRoot = $env:GRAPT_ARTIFACT_ROOT,
    [string]$ReceiptRoot = $env:GRAPT_RECEIPT_ROOT
)

# R6-RELEASE-PACKAGING-01 Linux ARTIFACT BUILD on the governed carrier.
#
# This mirrors scripts/run-linux.ps1 and scripts/daily-use/run-j01-mutations.ps1
# rather than replacing either. Why a campaign-local launcher exists at all:
# scripts/run-linux.ps1 validates -ComputeCommand against a HARDCODED
# ValidateSet of the shared route inventory and then picks between exactly two
# shared profiles by name. `release-linux-artifacts` is in neither list, so the
# shared script cannot reach this campaign's profile without WIDENING a shared
# route inventory that other lanes depend on. The daily-use lane hit the same
# wall and answered it the same way; this follows that settled seam instead of
# inventing a second one.
#
# Two deliberate differences from the shared script, both required here:
#   - it uses remote-compute/release-packaging-linux.json, a campaign-local
#     profile whose resources block names domain1 ONLY, so no class here can
#     spill into a domain this campaign does not own. The string domain0
#     appears nowhere in it;
#   - it runs a command the shared profiles do not name, and that command
#     RETURNS BINARIES. Every shared profile carries
#     `"evidence": {"artifacts": []}` -- they are gate profiles, so no existing
#     route can carry an artifact back at all.
#
# The command is a PARAMETER validated against the profile's own command table,
# so a second packaging job does not need a second copy of this script that
# could drift from it. A command absent from the profile is refused HERE, with
# the name, rather than reaching the governed door and failing there.
#
# Everything else is inherited: the same cache namespace and key, the same
# pinned toolchain checks, the same bounds, and the SAME shared carrier storage
# budget check before and after the job. The budget is never bypassed: a refusal
# is a coordination dependency, not a reason to retry, relocate caches
# or weaken the gate.

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

$profile = Join-Path $RepoRoot (Join-Path 'remote-compute' 'release-packaging-linux.json')
if ($env:SECODE_COMPUTE_PROFILE_DIR) { $profile = Join-Path $env:SECODE_COMPUTE_PROFILE_DIR (Split-Path -Leaf $profile) }
if (-not (Test-Path -LiteralPath $profile -PathType Leaf)) {
    throw "campaign-local compute profile is absent: $profile"
}
$profileJson = Get-Content -LiteralPath $profile -Raw -Encoding UTF8 | ConvertFrom-Json
$commandNames = $profileJson.commands.PSObject.Properties.Name
if ($ComputeCommand -notin $commandNames) {
    throw "compute command is absent from the campaign profile: $ComputeCommand"
}

# The domain this campaign was allocated is domain1. Assert it from the profile
# rather than trusting the comment above: a resources block that quietly grew a
# second domain would otherwise reach the shared carrier before anyone read it.
$declaredDomains = @($profileJson.resources.domains.PSObject.Properties.Name)
if ($declaredDomains.Count -ne 1 -or $declaredDomains[0] -ne 'domain1') {
    throw "campaign profile must declare domain1 exclusively; it declares: $($declaredDomains -join ', ')"
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
