param([switch]$ProtectionDiagnostic, [switch]$CredentialValidation, [string]$CaseManifest,
      [string]$BuildTargets)
$ErrorActionPreference = 'Stop'
. (Join-Path $PSScriptRoot 'windows_hosted_phase.ps1')
if ($ProtectionDiagnostic -and $CredentialValidation) { throw 'Choose one hosted scope' }
if ($CaseManifest -and -not $CredentialValidation) { throw 'CaseManifest requires CredentialValidation' }
if ($BuildTargets -and -not $CaseManifest) { throw 'BuildTargets requires an exact CaseManifest' }
$root = Join-Path $env:RUNNER_TEMP 'hosted-results'
New-Item -ItemType Directory -Path $root -Force | Out-Null
$python = (Get-Command python.exe).Source
if (-not $ProtectionDiagnostic) {
    $build = if ($CredentialValidation) { 'build-credentials' } else { 'build-windows' }
    $selection = if ($CaseManifest) { @('--case-manifest', (Resolve-Path -LiteralPath $CaseManifest).Path) } else { @() }
    if ($BuildTargets) { $selection += @('--build-targets', (Resolve-Path -LiteralPath $BuildTargets).Path) }
    & $python (Join-Path $PSScriptRoot 'hosted_cases.py') $build --output (Join-Path $root 'build') @selection
    if ($LASTEXITCODE -ne 0) { throw 'Pilot test binaries did not build' }
}
$manifest = Join-Path $root 'build\manifest.json'
$adminSid = [Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$account = 'hosted-pilot'
$entropy = New-Object byte[] 32
$random = [Security.Cryptography.RandomNumberGenerator]::Create()
try { $random.GetBytes($entropy) } finally { $random.Dispose() }
$password = [Convert]::ToBase64String($entropy) + 'aA1!'
[Array]::Clear($entropy, 0, $entropy.Length)
$secure = ConvertTo-SecureString $password -AsPlainText -Force
$password = $null
$user = New-LocalUser -Name $account -Password $secure -AccountNeverExpires -PasswordNeverExpires
$sid = $user.SID.Value
$credential = [PSCredential]::new(".\$account", $secure)
$failed = $false
$cleanup = @{account_removed=$false; profile_removed=$false}
try {
    $users = Get-LocalGroup -SID 'S-1-5-32-545'
    if (-not (Get-LocalGroupMember -Group $users | Where-Object { $_.SID.Value -eq $sid })) {
        Add-LocalGroupMember -Group $users -Member $user
    }
    $standard = Join-Path $root $(if ($ProtectionDiagnostic) { 'standard-protection' } elseif ($CredentialValidation) { 'standard-credentials' } else { 'standard-security' })
    New-Item -ItemType Directory -Path $standard | Out-Null
    & icacls.exe $standard /grant "*${sid}:(OI)(CI)M" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot grant the disposable account its results directory' }
    # Protection tests remove inherited ACLs from fixtures. Keep those trees
    # outside the artifact root so uploading reports never traverses them.
    $fixtureRoot = Join-Path $env:RUNNER_TEMP 'hosted-fixtures'
    New-Item -ItemType Directory -Path $fixtureRoot -Force | Out-Null
    & icacls.exe $fixtureRoot /grant "*${sid}:(OI)(CI)M" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot grant the disposable account its fixture parent' }
    & icacls.exe $env:GITHUB_WORKSPACE /grant "*${sid}:(OI)(CI)RX" | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'Cannot make the checked-out test sources readable' }
    if (-not $ProtectionDiagnostic) {
        & icacls.exe (Join-Path $root 'build') /grant "*${sid}:(OI)(CI)RX" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Cannot make the captured binary manifest readable' }
        & icacls.exe $env:CARGO_TARGET_DIR /grant "*${sid}:(OI)(CI)RX" | Out-Null
        if ($LASTEXITCODE -ne 0) { throw 'Cannot make compiled test executables readable' }
    }
    $phases = if ($ProtectionDiagnostic) { @('standard-protection','admin-protection') } elseif ($CredentialValidation) { @('standard-credentials') } else { @('admin-security','standard-security','admin-product') }
    foreach ($phase in $phases) {
        $identity = if ($phase.StartsWith('standard-')) { 'standard' } else { 'admin' }
        $suite = if ($ProtectionDiagnostic) { 'protection' } elseif ($CredentialValidation) { 'credentials' } elseif ($phase -eq 'admin-product') { 'product' } else { 'security' }
        $expected = if ($identity -eq 'standard') { $sid } else { $adminSid }
        $output = Join-Path $root $phase
        $fixture = Join-Path $fixtureRoot $phase
        $worker = Join-Path $PSScriptRoot 'windows_hosted_identity.ps1'
        $arguments = @('-NoProfile','-NonInteractive','-File',"`"$worker`"",'-Python',"`"$python`"",
            '-Manifest',"`"$manifest`"",'-Output',"`"$output`"",'-Fixture',"`"$fixture`"",
            '-ExpectedSid',$expected,'-Identity',$identity,'-Suite',$suite)
        $shell = Join-Path $PSHOME $(if ($PSVersionTable.PSVersion.Major -le 5) { 'powershell.exe' } else { 'pwsh.exe' })
        # The Python phase owner joins its kill-on-close job before launching
        # the identity worker, including under Windows PowerShell 5.1.
        $phaseArguments = @("`"$(Join-Path $PSScriptRoot 'hosted_cases.py')`"", 'phase',
            '--output', "`"$output`"", '--phase-command', "`"$shell`"") + $arguments
        $options = @{FilePath=$python; ArgumentList=$phaseArguments; PassThru=$true
            WorkingDirectory=$env:GITHUB_WORKSPACE
            RedirectStandardOutput=(Join-Path $root "$phase.stdout.log")
            RedirectStandardError=(Join-Path $root "$phase.stderr.log")}
        if ($identity -eq 'standard') { $options.Credential=$credential; $options.LoadUserProfile=$true }
        $phaseLimit = if ($ProtectionDiagnostic) { 600000 } else { 1200000 }
        $phaseResult = Invoke-HostedPhase -Options $options -LimitMilliseconds $phaseLimit -Output $output `
            -Receipt (Join-Path $root "$phase.phase.json")
        if ($phaseResult.status -ne 'completed') { $failed = $true }
        $summary = Join-Path $output 'results.md'
        if (Test-Path -LiteralPath $summary) { Get-Content -Raw $summary | Add-Content $env:GITHUB_STEP_SUMMARY }
    }
} finally {
    Remove-LocalUser -SID $user.SID
    $cleanup.account_removed = $null -eq (Get-LocalUser -SID $user.SID -ErrorAction SilentlyContinue)
    $userProfile = Get-CimInstance Win32_UserProfile -Filter "SID='$sid'"
    if ($userProfile -and -not $userProfile.Loaded) { $userProfile | Remove-CimInstance }
    $cleanup.profile_removed = $null -eq (Get-CimInstance Win32_UserProfile -Filter "SID='$sid'")
    $cleanup | ConvertTo-Json | Set-Content -Encoding utf8 (Join-Path $root 'account-cleanup.json')
}
if ($failed -or -not $cleanup.account_removed -or -not $cleanup.profile_removed) { exit 1 }
