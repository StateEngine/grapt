//! Fixed, bounded managed Windows operations. No command interpolation or secret
//! arguments/environment. Errors deliberately omit the child output.
//! Copyright 2025 OpenAI.
//! Apache-2.0; Modified by Grapt. See licenses/openai-codex/.
use std::io;

#[path = "windows/batch.rs"]
mod batch;
#[path = "windows/worker.rs"]
mod worker;
pub(super) use batch::{begin_batch, end_batch};

const INPUT_BYTES: usize = 64 * 1024;
const SCRIPT: &str = r#"
$ErrorActionPreference = 'Stop'
$PSModuleAutoLoadingPreference = 'None'
$importsPending = $true
function Test-PrivateDirectory($acl, $user) {
    $rules = @($acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier]))
    return ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -eq $user.Value -and
        $acl.AreAccessRulesProtected -and $rules.Count -eq 1 -and
        $rules[0].IdentityReference.Value -eq $user.Value -and
        $rules[0].AccessControlType -eq 'Allow' -and $rules[0].FileSystemRights -eq 'FullControl' -and
        $rules[0].InheritanceFlags -eq 'ContainerInherit,ObjectInherit' -and
        $rules[0].PropagationFlags -eq 'None' -and -not $rules[0].IsInherited)
}
while ($null -ne ($line = [Console]::In.ReadLine())) {
$failure = 'request'
$request = $null
$identity = $null
try {
    if ($line.Length -gt 100000) { throw 'request' }
    if ($importsPending) {
        # Avoid first-use discovery across ambient module locations.
        Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Utility\Microsoft.PowerShell.Utility.psd1" -ErrorAction Stop
        Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Management\Microsoft.PowerShell.Management.psd1" -ErrorAction Stop
        Import-Module -Name "$PSHOME\Modules\Microsoft.PowerShell.Security\Microsoft.PowerShell.Security.psd1" -ErrorAction Stop
        $importsPending = $false
    }
    $request = $line | ConvertFrom-Json
    $data = [Convert]::FromBase64String($request.data)
    switch -Exact ($request.operation) {
        # Opens credentials DPAPI sealed before the keystore envelope.
        'open' {
            $failure = 'open'
            Add-Type -AssemblyName System.Security
            $result = [Security.Cryptography.ProtectedData]::Unprotect($data, $null, [Security.Cryptography.DataProtectionScope]::CurrentUser)
        }
        { $_ -in 'file-owner', 'file-check', 'file-private-check' } {
            $path = [Text.Encoding]::UTF8.GetString($data)
            $failure = 'type'
            $item = Get-Item -LiteralPath $path -Force
            if ($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'type' }
            if ($request.operation -eq 'file-private-check' -and $item.LinkType -eq 'HardLink') { throw 'type' }
            $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
            $user = $identity.User
            $failure = 'acl-read'
            $acl = Get-Acl -LiteralPath $path
            $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
            if ($request.operation -eq 'file-owner' -and $owner -ne $user.Value -and $owner -eq $identity.Owner.Value) {
                $acl.SetOwner($user)
                $failure = 'acl-write'
                Set-Acl -LiteralPath $path -AclObject $acl
                $failure = 'acl-read'
                $acl = Get-Acl -LiteralPath $path
            }
            if ($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $user.Value) { throw 'owner' }
            $write = [Security.AccessControl.FileSystemRights]'Write,Delete,ChangePermissions,TakeOwnership'
            foreach ($rule in $acl.GetAccessRules($true, $true, [Security.Principal.SecurityIdentifier])) {
                if ($rule.AccessControlType -eq 'Allow' -and $rule.IdentityReference.Value -ne $user.Value -and
                    (($rule.FileSystemRights -band $write) -or $request.operation -eq 'file-private-check')) { throw 'foreign-write' }
            }
            $result = [byte[]]@()
        }
        { $_ -in 'directory', 'directory-check' } {
            $path = [Text.Encoding]::UTF8.GetString($data)
            $failure = 'type'
            $item = Get-Item -LiteralPath $path -Force
            if (-not $item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'type' }
            $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
            $user = $identity.User
            $failure = 'acl-read'
            $acl = Get-Acl -LiteralPath $path
            $owner = $acl.GetOwner([Security.Principal.SecurityIdentifier]).Value
            # Elevated creation uses the token's default owner (Administrators).
            # Only the explicit protection path normalizes it; reads stay strict.
            if ($owner -ne $user.Value -and -not ($request.operation -eq 'directory' -and $owner -eq $identity.Owner.Value)) { throw 'owner' }
            # Set-Acl can require additional privileges on a second protected
            # write under a non-elevated token. A private directory needs no
            # mutation; keep the same strict verification on both paths.
            if ($request.operation -eq 'directory' -and -not (Test-PrivateDirectory $acl $user)) {
                $private = New-Object Security.AccessControl.DirectorySecurity
                if ($owner -ne $user.Value) { $private.SetOwner($user) }
                $private.SetAccessRuleProtection($true, $false)
                $rule = New-Object Security.AccessControl.FileSystemAccessRule($user, 'FullControl', 'ContainerInherit,ObjectInherit', 'None', 'Allow')
                $private.AddAccessRule($rule)
                $failure = 'acl-write'
                Set-Acl -LiteralPath $path -AclObject $private
            }
            $failure = 'acl-read'
            $checked = Get-Acl -LiteralPath $path
            if (-not (Test-PrivateDirectory $checked $user)) { throw 'verification' }
            $result = [byte[]]@()
        }
        default { throw 'operation' }
    }
    [Console]::Out.WriteLine((@{ request_id = $request.request_id; data = [Convert]::ToBase64String($result) } | ConvertTo-Json -Compress))
} catch {
    # Never echo exception text: it may contain paths or request data.
    $known = @('type', 'owner', 'foreign-write', 'verification', 'operation')
    if ($_.Exception.Message -in $known) { $failure = $_.Exception.Message }
    $windows = @{ win32_code = $null; code_source = $null; profile_loaded = $null }
    # Only unwrap a bounded exception chain. HRESULT-derived codes are labelled;
    # GetLastWin32Error here could describe an unrelated, stale native call.
    $exception = $_.Exception
    for ($depth = 0; $depth -lt 4 -and $null -ne $exception; $depth++) {
        if ($exception -is [ComponentModel.Win32Exception]) {
            $nativeCode = $exception.NativeErrorCode
            $windows.win32_code = $null
            $windows.code_source = $null
            if ($nativeCode -ge 0 -and $nativeCode -le 65535) {
                $windows.win32_code = $nativeCode
                $windows.code_source = 'native'
            }
            break
        }
        if (($exception.HResult -band 4294901760L) -eq 2147942400L) {
            $windows.win32_code = $exception.HResult -band 65535
            $windows.code_source = 'hresult'
        }
        $exception = $exception.InnerException
    }
    $hive = $null
    try {
        if ($null -eq $identity) { $identity = [Security.Principal.WindowsIdentity]::GetCurrent() }
        # Check this token's SID hive, not HKCU's possible default-user mapping.
        # This observes profile loading, not DPAPI master-key readiness.
        $hive = [Microsoft.Win32.Registry]::Users.OpenSubKey($identity.User.Value)
        $windows.profile_loaded = ($null -ne $hive)
    } catch {
        # A failed observation must not overwrite the original refusal.
        $windows.profile_loaded = $null
    } finally {
        if ($null -ne $hive) { $hive.Dispose() }
    }
    [Console]::Out.WriteLine((@{ request_id = $request.request_id; error = $failure; windows = $windows } | ConvertTo-Json -Compress))
    exit 1
} finally {
    if ($null -ne $identity) { $identity.Dispose() }
    if ($data -is [byte[]]) { [Array]::Clear($data, 0, $data.Length) }
    if ($result -is [byte[]]) { [Array]::Clear($result, 0, $result.Length) }
    $request = $null
    $line = $null
}
}
"#;

pub(super) fn run(operation: &str, bytes: &[u8]) -> io::Result<Vec<u8>> {
    if bytes.len() > INPUT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "profile protection input exceeds its byte bound",
        ));
    }
    batch::run(operation, bytes)
}
