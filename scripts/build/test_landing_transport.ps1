param([string]$Module = "$PSScriptRoot/landing_transport.ps1")
$ErrorActionPreference='Stop'
. $Module
$root=Join-Path ([IO.Path]::GetTempPath()) ('secode-transport-test-'+[Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($root)
$exe=(Get-Process -Id $PID).Path
$foreign=$null
try {
 $foreign=Start-Process $exe '-NoProfile -NonInteractive -Command "Start-Sleep -Seconds 60"' -PassThru -NoNewWindow
 foreach($case in @('normal','nonzero','stalled','noisy-stalled','split-stalled')) {
  $script=Join-Path $root ($case+'.ps1')
  $marker='FINISHED_'+[Guid]::NewGuid().ToString('N')
  $identity=Join-Path $root ($case+'.pid')
  $body="[IO.File]::WriteAllText('$identity',[string]`$PID); Write-Output 'file-backed output'; Write-Output '${marker}:0'; "
  if($case -eq 'noisy-stalled'){$body+="Write-Output ('x'*16000); "}
  if($case -eq 'split-stalled'){
   $split=[int]($marker.Length/2)
   $body="[IO.File]::WriteAllText('$identity',[string]`$PID); Write-Output 'file-backed output'; [Console]::Write('"+$marker.Substring(0,$split)+"'); Start-Sleep -Milliseconds 400; [Console]::WriteLine('"+$marker.Substring($split)+":0'); "
  }
  if($case.EndsWith('stalled')){$body+='Start-Sleep -Seconds 60'}elseif($case -eq 'nonzero'){$body+='exit 7'}
  [IO.File]::WriteAllText($script,$body)
  $leasePath=Join-Path $root 'lease'
  $lease=[IO.File]::Open($leasePath,'OpenOrCreate','ReadWrite','None')
  $watch=[Diagnostics.Stopwatch]::StartNew()
  $threw=$false
  try {
   $output=Join-Path $root ($case+'.log')
   $status=Invoke-LandingTransport $exe "-NoProfile -NonInteractive -File `"$script`"" $marker 1 6> $output
  } catch {
   $threw=$true
   if(-not $case.EndsWith('stalled') -or $_.Exception.Message -notmatch 'did not close within 1 seconds'){throw}
  } finally {$lease.Dispose()}
  if($case.EndsWith('stalled')) {
   if(-not $threw -or $watch.Elapsed.TotalSeconds -gt 15){throw 'stalled channel was not bounded'}
  } elseif($status -ne $(if($case -eq 'nonzero'){7}else{0})){throw 'transport lost native exit status'}
  if([IO.File]::ReadAllText($output) -notmatch 'file-backed output'){throw 'file-backed output was not read back'}
  $owned=[int][IO.File]::ReadAllText($identity)
  if(Get-Process -Id $owned -ErrorAction SilentlyContinue){throw 'owned transport survived return'}
  if($foreign.HasExited){throw 'unrelated process was terminated'}
  $probe=[IO.File]::Open($leasePath,'Open','ReadWrite','None');$probe.Dispose()
  Write-Output "PASS $case"
 }
} finally {
 if($foreign){if(-not $foreign.HasExited){$foreign.Kill();[void]$foreign.WaitForExit(5000)};$foreign.Dispose()}
 Remove-Item -LiteralPath $root -Recurse -Force
}
