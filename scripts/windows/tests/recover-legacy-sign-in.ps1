[CmdletBinding()]
param([string]$Script,[string]$Cli)
$ErrorActionPreference='Stop'
foreach($module in @('Security','Management','Utility')){Import-Module ($PSHOME+'/Modules/Microsoft.PowerShell.'+$module) -ErrorAction Stop}
if($Script -and $Cli){throw 'choose-script-or-cli'}
if(-not $Script -and -not $Cli){$Script=[IO.Path]::GetFullPath($PSScriptRoot+'/../recover-legacy-sign-in.ps1')}
$root=[IO.Path]::Combine([IO.Path]::GetTempPath(),'grapt-recovery-test-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $root | Out-Null
function Run-Recovery([switch]$Apply,[switch]$ExpectUnprotectFailure,[switch]$ExpectPublishFailure){
 $info=New-Object Diagnostics.ProcessStartInfo
 if($Cli){
  $info.FileName=$Cli
  $info.Arguments='recover-sign-in --profile "'+$root+'"'
  if($Apply){$info.Arguments+=' --restore-legacy'}
 }else{
  $info.FileName=Join-Path $env:SystemRoot 'System32/WindowsPowerShell/v1.0/powershell.exe'
  $info.Arguments='-NoLogo -NoProfile -NonInteractive -File "'+$Script+'" -Profile "'+$root+'"'
  if($Apply){$info.Arguments+=' -RestoreLegacy'}
 }
 $info.UseShellExecute=$false;$info.RedirectStandardOutput=$true;$info.RedirectStandardError=$true
 $info.EnvironmentVariables['PSModulePath']='E:/temp/nonexistent-powershell-seven-modules'
 $child=[Diagnostics.Process]::Start($info)
 $stdout=$child.StandardOutput.ReadToEndAsync();$stderr=$child.StandardError.ReadToEndAsync()
 if(-not $child.WaitForExit(15000)){$child.Kill();$child.WaitForExit();throw 'operator timeout'}
 $output=$stdout.GetAwaiter().GetResult();$failure=$stderr.GetAwaiter().GetResult()
 if($output.Length -gt 65536 -or $failure.Length -gt 65536){throw 'operator output bound'}
 if($ExpectUnprotectFailure){if($child.ExitCode -eq 0 -or $failure -notmatch 'state-unprotect-failed'){throw 'named unprotect counterexample did not fire'}}elseif($ExpectPublishFailure){if($child.ExitCode -eq 0 -or $failure -notmatch 'recovery-refused: original formats restored'){[Console]::WriteLine('counterexample exit='+$child.ExitCode+' rollback_failure='+($failure -match 'recovery-rollback-failed')+' restored_refusal='+($failure -match 'recovery-refused')+' success='+($output -match 'Legacy credential formats restored'));throw 'named publication counterexample did not fire'}}elseif($child.ExitCode -ne 0){throw 'operator fixture refused'}
}
try{
 $user=[Security.Principal.WindowsIdentity]::GetCurrent().User
 $private=New-Object Security.AccessControl.DirectorySecurity
 $private.SetOwner($user);$private.SetAccessRuleProtection($true,$false)
 $private.AddAccessRule((New-Object Security.AccessControl.FileSystemAccessRule($user,'FullControl','ContainerInherit,ObjectInherit','None','Allow')))
 Set-Acl -LiteralPath $root -AclObject $private
 Add-Type -AssemblyName System.Security
 $plain=[Text.Encoding]::UTF8.GetBytes(' {"schema":"secode.chatgpt-oauth.v2","access_token":"fixture-access","refresh_token":"fixture-refresh","account":"fixture-account","expires_at_ms":1800000000000}')
 $sealed=[Text.Encoding]::UTF8.GetBytes("secode.dpapi.current-user.v1`n")+[Security.Cryptography.ProtectedData]::Protect($plain,$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
 $state=Join-Path $root 'chatgpt.oauth.json'
 [IO.File]::WriteAllBytes($state,$sealed)
 [IO.File]::WriteAllText((Join-Path $root 'provider.credential'),'fixture-access')
 Run-Recovery
 if([Convert]::ToBase64String([IO.File]::ReadAllBytes($state)) -ne [Convert]::ToBase64String($sealed)){throw 'check-only mutated state'}
 Run-Recovery -Apply
 $parsed=[IO.File]::ReadAllText($state)|ConvertFrom-Json
 if($parsed.schema -ne 'secode.chatgpt-oauth.v2' -or $parsed.refresh_token -ne 'fixture-refresh'){throw 'restored legacy state mismatch'}
 if(@(Get-ChildItem -LiteralPath $root -Force -Filter '*.recovery-sealed').Count -ne 1){throw 'encrypted original missing'}
 [IO.File]::WriteAllBytes($state,$sealed)
 $credential=Join-Path $root 'provider.credential'
 $sealedCredential=[Text.Encoding]::UTF8.GetBytes("secode.dpapi.current-user.v1`n")+[Security.Cryptography.ProtectedData]::Protect([Text.Encoding]::UTF8.GetBytes('fixture-access'),$null,[Security.Cryptography.DataProtectionScope]::CurrentUser)
 [IO.File]::WriteAllBytes($credential,$sealedCredential)
 $lease=[IO.File]::Open($credential,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
 try{Run-Recovery -Apply -ExpectPublishFailure}finally{$lease.Dispose()}
 if([Convert]::ToBase64String([IO.File]::ReadAllBytes($state)) -ne [Convert]::ToBase64String($sealed)){throw 'second publication failure did not restore first encrypted file'}
 if([Convert]::ToBase64String([IO.File]::ReadAllBytes($credential)) -ne [Convert]::ToBase64String($sealedCredential)){throw 'second publication failure changed the refused file'}
 if(@(Get-ChildItem -LiteralPath $root -Force).Count -ne 3){throw 'publication refusal left unexpected artifacts'}
 $damaged=[Text.Encoding]::UTF8.GetBytes("secode.dpapi.current-user.v1`nnot-a-DPAPI-blob")
 [IO.File]::WriteAllBytes((Join-Path $root 'provider.credential'),$damaged)
 Run-Recovery -Apply -ExpectUnprotectFailure
 if([Convert]::ToBase64String([IO.File]::ReadAllBytes($state)) -ne [Convert]::ToBase64String($sealed)){throw 'second-file refusal mutated first file'}
 if(@(Get-ChildItem -LiteralPath $root -Force).Count -ne 3){throw 'second-file refusal created recovery artifacts'}
 Write-Output 'PASS: default check preserves bytes; explicit legacy restore handles invalid PSModulePath; damaged second file refuses before mutation; second publication failure restores the first encrypted file'
}finally{Remove-Item -LiteralPath $root -Recurse -Force}
