param(
    [Parameter(Mandatory = $true)]
    [string]$StateEngineEntry,
    [ValidateSet('desktop-recovery', 'desktop-history', 'dogfood-cli-convergence', 'retained-fixture-compat', 'retained-completion-gate-tail', 'retained-completion-compat', 'retained-completion-mutations', 'retained-repin-mutations', 'retained-compatibility-tail', 'retained-correction-checks', 'retained-compatibility-mutations', 'retained-decision-mutations', 'retained-product-readiness', 'retained-shell-focused', 'retained-tui-focused', 'smoke', 'evidence-smoke', 'release-evidence', 'full-gate', 'workspace-test-attribution', 'mutation-battery', 'final-composition-battery', 'provider-mutations', 'config-mutations', 'config-ledger-mutations', 'config-all-mutations', 'runtime-mutations', 'store-mutations', 'event-mutations', 'platform-mutations', 'service-mutations', 'release-mutations', 'permission-mutations', 'product-mutations', 'pcu-mutations', 'cache-usage-benchmark', 'r4ca-mutations')]
    [string]$ComputeCommand = 'smoke',
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

$BudgetHelper = Join-Path $RepoRoot 'remote-compute\cache_budget.py'
if (-not (Test-Path -LiteralPath $StateEngineEntry -PathType Leaf)) {
    throw "StateEngine remote-compute entry is absent: $StateEngineEntry"
}
if (-not (Test-Path -LiteralPath $BudgetHelper -PathType Leaf)) {
    throw "Grapt cache-budget helper is absent: $BudgetHelper"
}
if (-not (Test-Path -LiteralPath $ArtifactRoot -PathType Container)) {
    throw "Grapt immutable artifact source root is absent: $ArtifactRoot"
}

$profileName = if ($ComputeCommand -in @('retained-completion-gate-tail', 'retained-correction-checks', 'evidence-smoke', 'release-evidence', 'full-gate', 'workspace-test-attribution', 'final-composition-battery')) {
    'secode-linux-evidence.json'
} else {
    'secode-linux.json'
}
$profile = Join-Path $RepoRoot (Join-Path 'remote-compute' $profileName)
if ($env:SECODE_COMPUTE_PROFILE_DIR) { $profile = Join-Path $env:SECODE_COMPUTE_PROFILE_DIR (Split-Path -Leaf $profile) }

& python $BudgetHelper --phase pre
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
& python $BudgetHelper --phase post
$postBudgetExit = $LASTEXITCODE
if ($postBudgetExit -ne 0) {
    [Console]::Error.WriteLine("governed job returned exit $jobExit, but the post-job cache budget refused")
    exit 125
}
exit $jobExit
