# Operator recovery only: restores the pre-DPAPI file format under a private profile.
[CmdletBinding()]
param([Parameter(Mandatory=$true)][string]$Profile, [switch]$RestoreLegacy)
$ErrorActionPreference='Stop'
# Use the bundled Windows PowerShell security module even from a pwsh-7 shell.
foreach($module in @('Security','Management','Utility')){Import-Module ($PSHOME+'/Modules/Microsoft.PowerShell.'+$module) -ErrorAction Stop}
$user=[Security.Principal.WindowsIdentity]::GetCurrent().User
$root=Get-Item -LiteralPath $Profile -Force
if(-not $root.PSIsContainer -or ($root.Attributes -band [IO.FileAttributes]::ReparsePoint)){throw 'profile-type'}
$acl=Get-Acl -LiteralPath $Profile
$rules=@($acl.GetAccessRules($true,$true,[Security.Principal.SecurityIdentifier]))
if($acl.GetOwner([Security.Principal.SecurityIdentifier]).Value -ne $user.Value -or -not $acl.AreAccessRulesProtected -or $rules.Count -ne 1 -or $rules[0].IdentityReference.Value -ne $user.Value -or $rules[0].AccessControlType -ne 'Allow' -or $rules[0].FileSystemRights -ne 'FullControl' -or $rules[0].InheritanceFlags -ne 'ContainerInherit,ObjectInherit' -or $rules[0].PropagationFlags -ne 'None' -or $rules[0].IsInherited){throw 'profile-not-private'}
$prefix=[Text.Encoding]::UTF8.GetBytes("secode.dpapi.current-user.v1`n")
Add-Type -AssemblyName System.Security
$changes=@()
$nonce=[Guid]::NewGuid().ToString('N')
foreach($name in @('chatgpt.oauth.json','provider.credential')){
 $path=Join-Path $Profile $name
 $item=Get-Item -LiteralPath $path -Force
 if($item.PSIsContainer -or ($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -or $item.Length -gt 66560){throw 'state-type-or-bound'}
 $bytes=[IO.File]::ReadAllBytes($path)
 $sealed=$bytes.Length -gt $prefix.Length
 for($i=0;$sealed -and $i -lt $prefix.Length;$i++){if($bytes[$i] -ne $prefix[$i]){$sealed=$false}}
 if($sealed){
  try{$plain=[Security.Cryptography.ProtectedData]::Unprotect($bytes[$prefix.Length..($bytes.Length-1)],$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)}catch{throw 'state-unprotect-failed'}
 }else{$plain=$bytes}
 if($plain.Length -gt 65536){throw 'plaintext-bound'}
 if($name -eq 'chatgpt.oauth.json'){
  try{$state=[Text.Encoding]::UTF8.GetString($plain)|ConvertFrom-Json}catch{throw 'state-json'}
  if($state.schema -ne 'secode.chatgpt-oauth.v2' -or -not $state.access_token -or -not $state.refresh_token -or -not $state.account){throw 'state-schema'}
 }
 if($sealed){$changes+=@{path=$path;plain=$plain;temporary=Join-Path $Profile ('.'+$name+'.'+$nonce+'.recovery-new');backup=Join-Path $Profile ('.'+$name+'.'+$nonce+'.recovery-sealed');published=$false}}
}
if(-not $RestoreLegacy){Write-Output 'Profile sign-in state is recoverable by this Windows account. Use -RestoreLegacy only with the service stopped and a backup.';return}
# Validate/decrypt all files before the first content change. Originals remain private.
try{
 foreach($change in $changes){
  $stream=[IO.File]::Open($change.temporary,[IO.FileMode]::CreateNew,[IO.FileAccess]::Write,[IO.FileShare]::None)
  try{$stream.Write($change.plain,0,$change.plain.Length);$stream.Flush($true)}finally{$stream.Dispose()}
 }
 foreach($change in $changes){
  [IO.File]::Replace($change.temporary,$change.path,$change.backup)
  $change.published=$true
 }
}catch{
 $failed=$false
 foreach($change in $changes){if($change.published){try{[IO.File]::Replace($change.backup,$change.path,[NullString]::Value)}catch{$failed=$true}}}
 if($failed){throw 'recovery-rollback-failed: preserve this private profile and its recovery-sealed files'}
 throw 'recovery-refused: original formats restored'
}finally{foreach($change in $changes){if(Test-Path -LiteralPath $change.temporary){Remove-Item -LiteralPath $change.temporary -Force}}}
Write-Output 'Legacy credential formats restored; encrypted originals retained in private recovery-sealed files. No provider call was made.'
