param(
    [Parameter(Mandatory)][string]$Python,
    [Parameter(Mandatory)][string]$Manifest,
    [Parameter(Mandatory)][string]$Output,
    [Parameter(Mandatory)][string]$Fixture,
    [Parameter(Mandatory)][string]$ExpectedSid,
    [Parameter(Mandatory)][ValidateSet('admin','standard')][string]$Identity,
    [Parameter(Mandatory)][ValidateSet('security','product','protection','credentials')][string]$Suite
)
$ErrorActionPreference = 'Stop'
New-Item -ItemType Directory -Path $Output -Force | Out-Null
$current = [Security.Principal.WindowsIdentity]::GetCurrent()
$sid = $current.User.Value
# Capture inherited paths before the existing account-owned setup rewrites them.
# Only the existing required token checks below can qualify the executing identity.
$inheritedPaths = $null
try {
    . (Join-Path $PSScriptRoot 'windows_hosted_phase.ps1')
    $inheritedPaths = Get-HostedStartupPaths
    $startup = @{stage='worker-entered'; pid=$PID; sid=$sid
        observed_utc=[DateTime]::UtcNow.ToString('o'); paths=$inheritedPaths
        path_ownership=@{status='unavailable'; reason='no bounded pre-setup ownership query'}
        image=@{status='unavailable'; reason='no bounded worker image query'}
        executable_hash=@{status='unavailable'; reason='no bounded file hash query'}}
    $record = $startup | ConvertTo-Json -Depth 6 -Compress
    if ([Text.Encoding]::UTF8.GetByteCount($record) -le 4096) {
        Write-Output $record
    } else {
        Write-Output ('{"stage":"worker-entered","pid":' + $PID + ',"status":"unavailable","reason":"observation exceeds byte bound"}')
    }
} catch {
    # Diagnostic failure never changes the retained identity or case path.
}
$userProfileRegistryKey = "HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion\ProfileList\$sid"
$userProfile = [Environment]::ExpandEnvironmentVariables((Get-ItemProperty $userProfileRegistryKey).ProfileImagePath)
New-Item -ItemType Directory -Path $fixture | Out-Null
# Credentialed process creation can inherit the runner's environment. Establish
# the actual account's paths before Add-Type or any fixture subprocess starts.
$env:USERPROFILE = $userProfile
$env:APPDATA = Join-Path $userProfile 'AppData\Roaming'
$env:LOCALAPPDATA = Join-Path $userProfile 'AppData\Local'
$env:TEMP = $fixture
$env:TMP = $fixture
$env:PYTHONDONTWRITEBYTECODE = '1'
Remove-Item Env:HOME -ErrorAction SilentlyContinue
Remove-Item Env:GITHUB_STEP_SUMMARY -ErrorAction SilentlyContinue

# Query the executing token; a scheduler's "limited" label is not identity proof.
Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using System.Security.Principal;
public static class HostedToken {
    [DllImport("advapi32.dll", SetLastError=true)]
    static extern bool GetTokenInformation(IntPtr token, int kind, out int value, int size, out int needed);
    public static int Query(int kind) {
        int value, needed;
        using (var identity = WindowsIdentity.GetCurrent()) {
            if (!GetTokenInformation(identity.Token, kind, out value, 4, out needed))
                throw new Win32Exception(Marshal.GetLastWin32Error());
        }
        return value;
    }
}
'@
$principal = [Security.Principal.WindowsPrincipal]::new($current)
$admin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$elevated = [HostedToken]::Query(20) -ne 0
$groups = @(& whoami.exe /groups /fo csv /nh | ConvertFrom-Csv -Header Group,Type,Sid,Attributes)
$integrity = @($groups | Where-Object { $_.Sid -like 'S-1-16-*' } | ForEach-Object Sid)
function OwnerSid([string]$Path) {
    return ([Security.Principal.NTAccount]::new((Get-Acl -LiteralPath $Path).Owner)).Translate([Security.Principal.SecurityIdentifier]).Value
}
try {
    $inheritedClasses = @{}
    foreach ($name in @('USERPROFILE','APPDATA','LOCALAPPDATA','TEMP','TMP')) {
        $row = if ($null -ne $inheritedPaths) { $inheritedPaths[$name] } else { $null }
        $class = 'unavailable'
        if ($row -and $row.ContainsKey('value')) {
            $path = $row.value
            # A prefix comparison is a path classification, never an ACL witness.
            $ownPrefix = $userProfile.TrimEnd('\') + '\'
            $class = if ($path.Equals($userProfile, [StringComparison]::OrdinalIgnoreCase) -or $path.StartsWith($ownPrefix, [StringComparison]::OrdinalIgnoreCase)) {
                'resolved-own-profile-prefix; ownership-unverified'
            } else { 'outside-resolved-profile; ownership-unavailable' }
        }
        $inheritedClasses[$name] = $class
    }
} catch { $inheritedClasses = @{status='unavailable'; error_type=$_.Exception.GetType().Name} }
$facts = @{
    stage='identity-after-setup'; pid=$PID; inherited_paths_record='worker-entered in phase stdout'
    inherited_path_classification=$inheritedClasses
    sid=$sid; admin=$admin; elevated=$elevated; elevation_type=[HostedToken]::Query(18)
    integrity_sids=$integrity; uac=(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows\CurrentVersion\Policies\System').EnableLUA
    profile=$userProfile; profile_owner_sid=(OwnerSid $userProfile)
    temp=$fixture; temp_owner_sid=(OwnerSid $fixture)
    os_build=[Environment]::OSVersion.Version.ToString()
    os_revision=(Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').UBR
}
$facts | ConvertTo-Json -Depth 5 | Set-Content -Encoding utf8 (Join-Path $Output 'identity.json')
if ($sid -ne $ExpectedSid) { throw 'Executing identity differs from the requested account' }
if ($Identity -eq 'standard' -and $facts.temp_owner_sid -ne $sid) { throw 'The ordinary user does not own its fixture directory' }
if ($Identity -eq 'standard' -and ($admin -or $elevated -or $integrity -notcontains 'S-1-16-8192')) {
    throw 'The comparison requires an actual non-admin, non-elevated medium-integrity token'
}
if ($Identity -eq 'admin' -and (-not $admin -or -not $elevated)) { throw 'Default runner token is not the expected elevated administrator' }
if ($Suite -eq 'protection') {
    & $Python (Join-Path $PSScriptRoot 'windows_protection_diagnostic.py') --output $Output --identity $Identity
} else {
    & $Python (Join-Path $PSScriptRoot 'hosted_cases.py') $Suite --manifest $Manifest --output $Output --identity $Identity
}
$result = $LASTEXITCODE
$remaining = @(Get-ChildItem -LiteralPath $fixture -Force | Select-Object -ExpandProperty Name)
ConvertTo-Json -InputObject @($remaining) | Set-Content -Encoding utf8 (Join-Path $Output 'remaining-fixtures.json')
exit $result
