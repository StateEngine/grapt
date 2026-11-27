param([string]$Script = "$PSScriptRoot/../land.ps1", [string]$Case)
$ErrorActionPreference='Stop'
$gitExe=(Get-Command git -CommandType Application | Select-Object -First 1).Source
$root=Join-Path $env:TEMP ('land-test-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory $root | Out-Null
function Git-Real([string]$Repo,[string[]]$A) { & $gitExe -C $Repo @A | Out-Null; if($LASTEXITCODE){throw 'test setup git failed'} }
$cases=@('runtime-rewritten','runtime-paths-changed','runtime-tracked-changed','runtime-prefix-changed','runtime-incoming','runtime-incoming-overlap','clean','unrelated','tracked-overlap','untracked-overlap','changed-during-gate','staged-unchanged','index-changed','reuse','tier1','reuse-tier1','missing-tier','invalid-tier','live-receipt-red','reuse-receipt-red','rebased-branch','finished-marker-red','moved-disjoint','moved-overlapping','moved-before-gate','composition-red','proof-red','late-main','switched-branch','reuse-moved-disjoint','reuse-compose-red','lock-timeout','reuse-proof-noop','scp-red','scp-no-status','ssh-no-status','git-stderr-success','git-no-status')
$helperNames=@('flaky_quarantine.py','nextest_gate.py','gate_parallel.py','windows_native.py','windows_native_result.py','windows_native_worker.py','windows_native_environment.py','windows_native.ps1','nightly_revert.py','landing.py','landing_docs.py','full_capacity.py','cpu_priority.py','operator_paths.py','operator_environment.ps1','git_environment.py','build_environment.py','heavy.py','heavy_queue.py','heavy_runs.py','heavy_drain.py','heavy_hardware.py','heavy_sampling.py','external_holders.py','disk_stall.py','ram_targets.py','gate_policy.py','gate_scope.py','reviewed_composition.py','gate.py','gate_plan.py','test_features.py','slow_tests.py','windows_gate.py','windows_scope.py','receipt.py','target_discovery.py','duplicate_targets.py','evict_targets.py','cargo_observation.py','cargo_targets.py','orphan_cache.py','gate_retry.py','gate_evidence.py','gate_scratch.py','gate_timing.py','signal_trace.py')
# These Git/controller probes model transport separately. Native file-backed
# process ownership and stalled-channel cleanup run in test_landing_transport.ps1.
$controller=Join-Path $root 'controller'
New-Item -ItemType Directory (Join-Path $controller 'build') | Out-Null
Copy-Item (Join-Path (Split-Path $Script) 'build/landing_lease.ps1') (Join-Path $controller 'build/landing_lease.ps1')
foreach($name in $helperNames){Copy-Item (Join-Path (Split-Path $Script) ('build/'+$name)) (Join-Path $controller ('build/'+$name))}
$source=[IO.File]::ReadAllText($Script)
$anchor='. "$PSScriptRoot/build/landing_transport.ps1"'
if(-not $source.Contains($anchor)){throw 'landing transport boundary changed'}
$shim=@'
function Invoke-LandingTransport($FileName, $Arguments, $CompletionMarker) {
 if($Arguments -notmatch '^-n ([a-zA-Z0-9_-]+) "(.*)"$'){throw 'SSH native argv changed'}
 & ssh -n $Matches[1] $Matches[2]
 return $global:LASTEXITCODE
}
'@
$Script=Join-Path $controller 'land.ps1'
[IO.File]::WriteAllText($Script,$source.Replace($anchor,$shim),[Text.Encoding]::UTF8)
$cases+=@('reviewed','reviewed-dirty','reviewed-overlap','reviewed-late','reviewed-stale','reviewed-tier1','reviewed-missing-base','reviewed-missing-log','reviewed-red','reviewed-wrong-sha')
$cases+=@('nvme-root','other-nvme-root','traversal-root','space-root')
$cases+=@('supplier-custom','supplier-missing','supplier-space','supplier-traversal','supplier-shell','supplier-empty')
$cases+=@('node-custom','node-missing','node-space','node-traversal','node-shell','node-empty','node-wrong-major')
$cases+=@('lease-held','expected-base','stale-expected-base','late-expected-base')
$cases+=@('lead-finished-target')
$cases+=@('explicit-full','docs-fast','docs-explicit-full','docs-AGENTS.md','docs-CLAUDE.md','docs-SKILL.md','docs-mixed-aGeNtS.Md','docs-mixed-cLaUdE.Md','docs-mixed-sKiLl.Md')
if($Case){$cases=@($cases | Where-Object {$_ -eq $Case}); if(-not $cases.Count){throw "Unknown landing case: $Case"}}
foreach($case in $cases) {
 $repo=Join-Path $root $case; $candidate=Join-Path $root ($case+'-candidate')
 New-Item -ItemType Directory $repo | Out-Null
 Git-Real $repo @('init','-q','-b','main'); Git-Real $repo @('config','user.name','LandProbe'); Git-Real $repo @('config','user.email','probe@localhost')
 [IO.File]::WriteAllText((Join-Path $repo 'a.txt'),'before',[Text.Encoding]::UTF8)
 Git-Real $repo @('add','--','a.txt'); Git-Real $repo @('commit','-q','-m','base','--','a.txt')
 $old=(& $gitExe -C $repo rev-parse HEAD)
 & $gitExe clone -q $repo $candidate
 Git-Real $candidate @('config','user.name','LandProbe'); Git-Real $candidate @('config','user.email','probe@localhost')
 Git-Real $candidate @('checkout','-q','-b','candidate')
 if($case -eq 'runtime-tracked-changed'){
  New-Item -ItemType Directory (Join-Path $repo '.stateengine') | Out-Null
  [IO.File]::WriteAllText((Join-Path $repo '.stateengine/tracked.txt'),'base',[Text.Encoding]::UTF8)
  Git-Real $repo @('add','--','.stateengine/tracked.txt'); Git-Real $repo @('commit','-q','-m','tracked runtime','--','.stateengine/tracked.txt')
  Git-Real $candidate @('fetch','-q',$repo,'main'); Git-Real $candidate @('rebase','-q','FETCH_HEAD')
  $old=(& $gitExe -C $repo rev-parse HEAD)
 }
 $name=if($case -match '^docs-(?:mixed-)?(.+[.]md)$'){'docs/nested/'+$Matches[1]}elseif($case -like 'docs-*'){'docs/guide.md'}elseif($case -eq 'untracked-overlap'){'new.txt'}elseif($case -in @('runtime-incoming','runtime-incoming-overlap')){'.stateengine/incoming.txt'}else{'a.txt'}
 if($name.StartsWith('.stateengine/')){New-Item -ItemType Directory (Join-Path $candidate '.stateengine') | Out-Null}
 if($name.StartsWith('docs/')){New-Item -ItemType Directory (Split-Path (Join-Path $candidate $name)) -Force | Out-Null}
 [IO.File]::WriteAllText((Join-Path $candidate $name),'incoming',[Text.Encoding]::UTF8)
 Git-Real $candidate @('add','--',$name); Git-Real $candidate @('commit','-q','-m','candidate','--',$name)
 $sha=(& $gitExe -C $candidate rev-parse HEAD)
 if($case -in @('runtime-rewritten','runtime-paths-changed','runtime-incoming-overlap')){
  New-Item -ItemType Directory (Join-Path $repo '.stateengine') | Out-Null
  $runtime=if($case -eq 'runtime-incoming-overlap'){'incoming.txt'}else{'atlas.sqlite'}
  [IO.File]::WriteAllText((Join-Path $repo ('.stateengine/'+$runtime)),'runtime-before',[Text.Encoding]::UTF8)
 }
 if($case -eq 'runtime-tracked-changed'){[IO.File]::WriteAllText((Join-Path $repo '.stateengine/tracked.txt'),'tracked-before',[Text.Encoding]::UTF8)}
 if($case -eq 'runtime-prefix-changed'){
  New-Item -ItemType Directory (Join-Path $repo '.stateengine-other') | Out-Null
  [IO.File]::WriteAllText((Join-Path $repo '.stateengine-other/owned.txt'),'owned',[Text.Encoding]::UTF8)
 }
 if($case -in @('tracked-overlap','reviewed-overlap')){[IO.File]::WriteAllText((Join-Path $repo 'a.txt'),'owned',[Text.Encoding]::UTF8)}
 if($case -eq 'untracked-overlap'){[IO.File]::WriteAllText((Join-Path $repo 'new.txt'),'owned',[Text.Encoding]::UTF8)}
 if($case -in @('unrelated','changed-during-gate','reviewed-dirty')){[IO.File]::WriteAllText((Join-Path $repo 'AGENTS.md'),'owned',[Text.Encoding]::UTF8)}
 if($case -in @('staged-unchanged','index-changed','rebased-branch')){
 if($case -eq 'rebased-branch'){Git-Real $repo @('fetch','-q',$candidate,'refs/heads/candidate:refs/remotes/build-bench/candidate')}
 [IO.File]::WriteAllText((Join-Path $repo 'owned.txt'),'base',[Text.Encoding]::UTF8)
 Git-Real $repo @('add','--','owned.txt'); Git-Real $repo @('commit','-q','-m','owned base','--','owned.txt')
 # Compose the candidate after the preserved base so its fast-forward stays valid.
 Git-Real $candidate @('fetch','-q',$repo,'main')
 Git-Real $candidate @('rebase','-q','FETCH_HEAD')
 $old=(& $gitExe -C $repo rev-parse HEAD); $sha=(& $gitExe -C $candidate rev-parse HEAD)
 [IO.File]::WriteAllText((Join-Path $repo 'owned.txt'),'staged-A',[Text.Encoding]::UTF8); Git-Real $repo @('add','--','owned.txt')
 [IO.File]::WriteAllText((Join-Path $repo 'owned.txt'),'work-B',[Text.Encoding]::UTF8)
}
 $global:fakeMergeAttempted=$false; $global:fakeCopies=0; $global:fakeStage=$null
 $global:fakeGitCalls=0
 $global:fakeProbeCandidate=$candidate
 $global:fakeOriginalBase=$old; $global:fakeAcceptedSha=$sha
function git {
  $global:fakeGitCalls++
  # Emulate a git invocation that reports no fresh exit status; the stale
  # zero left by the harness's earlier native git is the hazard under test.
  if($case -eq 'git-no-status' -and $global:fakeGitCalls -eq 1){return}
  $a=@($args)
  if($a.Length -gt 2 -and $a[2] -eq 'merge'){$global:fakeMergeAttempted=$true}
  if($a.Length -gt 3 -and $a[2] -eq 'fetch'){$a[3]=$global:fakeProbeCandidate}
  if($case -eq 'git-stderr-success'){
   # Emulate Windows PowerShell 5.1: native stderr is a terminating error under Stop.
   $prev=$ErrorActionPreference; $ErrorActionPreference='Continue'
   $output=@(& $gitExe @a 2>&1); $code=$LASTEXITCODE; $ErrorActionPreference=$prev
   $stderr=@($output | Where-Object {$_ -is [System.Management.Automation.ErrorRecord]})
   if($code -eq 0 -and $stderr.Count -gt 0 -and $ErrorActionPreference -eq 'Stop'){throw 'native command wrote to stderr (emulated Windows PowerShell 5.1)'}
   $global:LASTEXITCODE=$code
   return @($output | Where-Object {$_ -isnot [System.Management.Automation.ErrorRecord]})
  }
  & $gitExe @a
  $global:LASTEXITCODE=$LASTEXITCODE
 }
 $global:fakeReused=$false
 $global:fakeAdvanced=$false; $global:fakeProofBase=$null; $global:fakeProofCandidate=$null
 $global:fakeExpectedMain=$old
 function Advance-Main {
  if($global:fakeAdvanced){return}
  $file=if($case -eq 'moved-overlapping'){'a.txt'}else{'b.txt'}
  [IO.File]::WriteAllText((Join-Path $repo $file),'new canonical',[Text.Encoding]::UTF8)
  Git-Real $repo @('add','--',$file); Git-Real $repo @('commit','-q','-m','canonical advance','--',$file)
  $global:fakeExpectedMain=(& $gitExe -C $repo rev-parse main)
  $global:fakeAdvanced=$true
 }
 if($case -in @('moved-before-gate','reuse-moved-disjoint','reuse-compose-red','lock-timeout','reuse-proof-noop','scp-red','scp-no-status','ssh-no-status','git-no-status','reviewed-stale','lead-finished-target')){Advance-Main}
 $global:fakeFinishedSource=$null
 function scp {
  if($args[0] -ne '-q' -or -not (Test-Path -LiteralPath $args[1]) -or $args[2] -ne ('build-host:'+ $global:fakeStage+'/'+[IO.Path]::GetFileName($args[1])) -or [IO.Path]::GetFileName($args[1]) -notin $helperNames){throw 'helper copy lost its private destination'}
  $global:fakeCopies++
  if($case -eq 'scp-no-status'){return}
  $global:LASTEXITCODE=if($case -eq 'scp-red'){1}else{0}
 }
 function ssh {
  if($case -eq 'ssh-no-status'){return}
  # Model the observed native boundary: oversized command returns success without executing.
  if($case -eq 'reuse-moved-disjoint' -and $args[-1].Length -gt 8191){$global:LASTEXITCODE=0;return}
  if($args[0] -ne '-n'){throw 'noninteractive SSH must close inherited stdin'}
  if($args[-1] -notmatch 'printf %s ([a-zA-Z0-9+/=]+)'){throw 'login shell payload missing'}
  $decoded=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Matches[1]))
  if($decoded -match "mkdir -- '(/tmp/secode-land-helper-[a-f0-9]{32})'"){$global:fakeStage=$Matches[1];$global:LASTEXITCODE=0;return}
  if($case -eq 'reuse-proof-noop' -and $decoded -match "(compose|verify) '([a-zA-Z0-9+/=]+)'"){
   if($Matches[1] -eq 'compose'){
    $noopData=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Matches[2])) | ConvertFrom-Json
    $digest=[Security.Cryptography.SHA256]::Create()
    try {$noopBranch='secode-land-'+([BitConverter]::ToString($digest.ComputeHash([Text.Encoding]::UTF8.GetBytes($noopData.receipt)))).Replace('-','').ToLowerInvariant().Substring(0,32)}finally{$digest.Dispose()}
    # A transport success with a stale export must still fail ancestry.
    Git-Real $global:fakeProbeCandidate @('update-ref',('refs/heads/'+$noopBranch),$global:fakeAcceptedSha,('0'*40))
   }
   $global:LASTEXITCODE=0;return
  }
  if($case -eq 'reuse-compose-red' -and $decoded -match "compose '"){$global:LASTEXITCODE=1;return}
  if($case -eq 'lock-timeout' -and $decoded -match "compose '"){$global:LASTEXITCODE=75;return}
  if($decoded -match "(compose|verify|qualify|docs|verify-docs) '([a-zA-Z0-9+/=]+)'"){
   $operation=$Matches[1]
   $data=[Text.Encoding]::UTF8.GetString([Convert]::FromBase64String($Matches[2])) | ConvertFrom-Json
   $docsOperation=$operation -in @('docs','verify-docs')
   if($docsOperation -ne ($case -eq 'docs-fast')){throw 'documentation fast path has the wrong scope or ignored explicit FullGate'}
   if(-not $docsOperation){
   $expectedSupplier=if($case -eq 'supplier-custom'){'/srv/secode/payload'}elseif($case -eq 'supplier-missing'){'/tmp/missing-supplier'}else{'/home/example/stateengine-payload'}
   if(-not $decoded.Contains("export SECODE_STATEENGINE_PAYLOAD_ROOT='$expectedSupplier'")){throw 'remote gate lost its explicit supplier root'}
   if($case -eq 'supplier-missing'){$global:LASTEXITCODE=1;return}
   $expectedNode=if($case -eq 'node-custom'){'/opt/node24'}elseif($case -eq 'node-missing'){'/tmp/missing-node'}elseif($case -eq 'node-wrong-major'){'/opt/node22'}else{'/home/example/.nvm/versions/node/v24.19.0'}
   if(-not $decoded.Contains("export NODE='$expectedNode/bin/node'") -or -not $decoded.Contains("export PATH='$expectedNode/bin':`$PATH")){throw 'remote gate lost its explicit Node executable or PATH'}
   if(-not $decoded.Contains('test -x "$NODE" || exit 1') -or -not $decoded.Contains('process.versions.node.split(".")[0]) !== 24')){throw 'remote gate lost its required Node 24 check'}
   if($case -in @('node-missing','node-wrong-major')){$global:LASTEXITCODE=1;return}
   }
   if($operation -eq 'qualify'){
    if([bool]$data.full -ne ($case -in @('tier1','reuse-tier1','explicit-full','docs-explicit-full'))){throw 'request lost explicit full-gate choice'}
    if($data.old_base -ne $global:fakeOriginalBase -or $data.accepted -ne $global:fakeAcceptedSha){throw 'qualification lost original identity'}
    if($case.StartsWith('reviewed')){
     if($data.retained_full.commit -ne ('c'*40) -or $data.retained_full.base -ne ('b'*40) -or $data.retained_full.log -ne '/tmp/full.log' -or $data.run_gate){throw 'reviewed qualification lost retained identity'}
    if($case -eq 'reviewed-late'){Advance-Main}
     if($case -eq 'reviewed-red'){$global:LASTEXITCODE=1;return}
   }elseif($data.PSObject.Properties.Name -contains 'retained_full'){throw 'ordinary request borrowed reviewed inputs'}
   if($case -eq 'late-expected-base'){Advance-Main}
    $global:fakeReused=$true
    if($case -eq 'reuse-receipt-red'){$global:LASTEXITCODE=1;return}
   }elseif($operation -in @('compose','docs')){
    if($decoded -match 'flock ' -or $decoded -notmatch 'python3 -B'){throw 'composition must enqueue its own priority ticket'}
    if($data.owner -ne 'agt-demo'){throw 'composition lost owner seat'}
    if($data.old_base -ne $global:fakeOriginalBase -or $data.accepted -ne $global:fakeAcceptedSha){throw 'composition lost original qualification identity'}
    if([bool]$data.full -ne ($case -in @('tier1','reuse-tier1','explicit-full','docs-explicit-full'))){throw 'request lost explicit full-gate choice'}
    if($case -in @('reuse','reuse-tier1','moved-before-gate','reuse-moved-disjoint') -and $data.run_gate){throw 'composition repeated the original gate'}
    $global:fakeReused=$true
    if($case -eq 'live-receipt-red'){$global:LASTEXITCODE=1; return}
    if($case -in @('moved-disjoint','moved-overlapping','composition-red','proof-red','late-main')){Advance-Main}
    $current=(& $gitExe -C $repo rev-parse main)
    $owned=@(& $gitExe -C $global:fakeProbeCandidate diff --no-renames --name-only $global:fakeOriginalBase $global:fakeAcceptedSha)
    $changed=@(& $gitExe -C $repo diff --no-renames --name-only $global:fakeOriginalBase $current)
    if(@($owned | Where-Object {$_ -in $changed}).Count){$global:LASTEXITCODE=1;return}
    $digest=[Security.Cryptography.SHA256]::Create()
    try {$export='secode-land-'+([BitConverter]::ToString($digest.ComputeHash([Text.Encoding]::UTF8.GetBytes($data.receipt)))).Replace('-','').ToLowerInvariant().Substring(0,32)}finally{$digest.Dispose()}
    $composedRoot=Join-Path $root ('composed-'+$case)
    Git-Real $global:fakeProbeCandidate @('worktree','add','--detach',$composedRoot,$global:fakeAcceptedSha)
    try {
     Git-Real $composedRoot @('fetch','-q',$repo,'main'); Git-Real $composedRoot @('rebase','-q','FETCH_HEAD')
     $global:fakeProofBase=$current; $global:fakeProofCandidate=(& $gitExe -C $composedRoot rev-parse HEAD)
     if($case -ne 'composition-red'){Git-Real $global:fakeProbeCandidate @('update-ref',('refs/heads/'+$export),$global:fakeProofCandidate,('0'*40))}
    }finally{Git-Real $global:fakeProbeCandidate @('worktree','remove','--force',$composedRoot)}
    if($case -eq 'composition-red'){$global:LASTEXITCODE=1;return}
   }else{
    if($data.new_base -ne $global:fakeProofBase -or $data.candidate -ne $global:fakeProofCandidate){$global:LASTEXITCODE=1;return}
    if($case -eq 'proof-red'){$global:LASTEXITCODE=1;return}
    if($case -eq 'late-main'){
     [IO.File]::WriteAllText((Join-Path $repo 'late.txt'),'late',[Text.Encoding]::UTF8)
     Git-Real $repo @('add','--','late.txt'); Git-Real $repo @('commit','-q','-m','late advance','--','late.txt')
     $global:fakeExpectedMain=(& $gitExe -C $repo rev-parse main)
    }
    if($case -eq 'switched-branch'){Git-Real $repo @('checkout','-q','-b','other')}
   }
  }
  if($decoded -match '(?m)^python3 scripts/build/receipt[.]py '){
   if($decoded -notmatch "--base '$global:fakeOriginalBase'"){throw 'receipt verifier lost original base'}
   $global:fakeReused=$true
   if($case -eq 'reuse-receipt-red'){$global:LASTEXITCODE=1;return}
  }
  if($case -eq 'finished-marker-red' -and $decoded -match 'disk_hygiene.py'){$global:LASTEXITCODE=1;return}
  if($case -eq 'lead-finished-target' -and $decoded -match "disk_hygiene.py --finish '([a-f0-9]{40})'"){
   $finishedSource=$Matches[1]
   if($finishedSource -ne (& $gitExe -C $global:fakeProbeCandidate rev-parse HEAD)){$global:LASTEXITCODE=1;return}
   $global:fakeFinishedSource=$finishedSource
  }
  if($case -eq 'runtime-rewritten'){[IO.File]::WriteAllText((Join-Path $repo '.stateengine/atlas.sqlite'),'runtime-after',[Text.Encoding]::UTF8)}
  if($case -eq 'runtime-paths-changed'){
   Remove-Item -LiteralPath (Join-Path $repo '.stateengine/atlas.sqlite') -ErrorAction SilentlyContinue
   [IO.File]::WriteAllText((Join-Path $repo '.stateengine/lock-holds.jsonl'),'runtime-after',[Text.Encoding]::UTF8)
  }
  if($case -eq 'runtime-tracked-changed'){[IO.File]::WriteAllText((Join-Path $repo '.stateengine/tracked.txt'),'tracked-after',[Text.Encoding]::UTF8)}
  if($case -eq 'runtime-prefix-changed'){[IO.File]::WriteAllText((Join-Path $repo '.stateengine-other/owned.txt'),'changed',[Text.Encoding]::UTF8)}
  if($case -eq 'changed-during-gate'){[IO.File]::WriteAllText((Join-Path $repo 'AGENTS.md'),'changed',[Text.Encoding]::UTF8)}
  if($case -eq 'index-changed'){
   [IO.File]::WriteAllText((Join-Path $repo 'owned.txt'),'staged-C',[Text.Encoding]::UTF8); Git-Real $repo @('add','--','owned.txt')
   [IO.File]::WriteAllText((Join-Path $repo 'owned.txt'),'work-B',[Text.Encoding]::UTF8)
  }
  $global:LASTEXITCODE=0
 }
 $parameters=@{LinuxHost='build-host';ControllerHost='controller-host';LinuxHome='/home/example';WorktreeParent='/srv/secode/worktrees';SupplierRoot='/home/example/stateengine-payload';NodeRoot='/home/example/.nvm/versions/node/v24.19.0';Branch='candidate';AcceptedCommit=$sha;LinuxRoot='/home/example/land-probe';Repository=$repo;Tier='2';Owner='agt-demo'}
 if($case -in @('expected-base','stale-expected-base','late-expected-base')){
  $parameters.ExpectedBase=if($case -eq 'stale-expected-base'){'f'*40}else{$old}
  $parameters.GateLog='/tmp/gate.log'
 }
 switch($case){
  'node-custom' {$parameters.NodeRoot='/opt/node24'}
  'node-missing' {$parameters.NodeRoot='/tmp/missing-node'}
  'node-wrong-major' {$parameters.NodeRoot='/opt/node22'}
  'node-space' {$parameters.NodeRoot='/opt/node root'}
  'node-traversal' {$parameters.NodeRoot='/opt/../node'}
  'node-shell' {$parameters.NodeRoot='/tmp/$(touch unsafe)'}
  'node-empty' {$parameters.NodeRoot=''}
  'supplier-custom' {$parameters.SupplierRoot='/srv/secode/payload'}
  'supplier-missing' {$parameters.SupplierRoot='/tmp/missing-supplier'}
  'supplier-space' {$parameters.SupplierRoot='/home/example/space root'}
  'supplier-traversal' {$parameters.SupplierRoot='/home/example/../payload'}
  'supplier-shell' {$parameters.SupplierRoot='/tmp/$(touch unsafe)'}
  'supplier-empty' {$parameters.SupplierRoot=''}
  'nvme-root' {$parameters.LinuxRoot='/srv/secode/worktrees/land-probe'}
  'other-nvme-root' {$parameters.LinuxRoot='/srv/build/other'}
  'traversal-root' {$parameters.LinuxRoot='/srv/secode/worktrees/../x'}
  'space-root' {$parameters.LinuxRoot='/srv/secode/worktrees/land probe'}
 }
 if($case -in @('reuse','reuse-tier1','reuse-receipt-red','moved-before-gate','reuse-moved-disjoint','reuse-compose-red','lock-timeout','reuse-proof-noop','scp-red','scp-no-status','ssh-no-status','git-no-status')){$parameters.SkipFullIfGreen='/tmp/gate.log'}
 if($case.StartsWith('reviewed')){
  $parameters.GateLog='/tmp/reviewed.log'
  $parameters.ReviewedBase=$old
  $parameters.RetainedFullCommit='c'*40
  $parameters.RetainedFullBase='b'*40
  $parameters.RetainedFullLog='/tmp/full.log'
  if($case -eq 'reviewed-tier1'){$parameters.Tier='1'}
  if($case -eq 'reviewed-missing-base'){$parameters.Remove('ReviewedBase')}
  if($case -eq 'reviewed-missing-log'){$parameters.Remove('GateLog')}
  if($case -eq 'reviewed-wrong-sha'){$parameters.AcceptedCommit='f'*40}
 }
 if($case -in @('tier1','reuse-tier1')){$parameters.Tier='1'}
 if($case -in @('tier1','reuse-tier1','explicit-full','docs-explicit-full')){$parameters.FullGate=$true}
 if($case -eq 'missing-tier'){$parameters.Remove('Tier')}
 if($case -eq 'invalid-tier'){$parameters.Tier='3'}
 $threw=$false; $refusal=''
 $heldLease=$null
 if($case -eq 'lease-held'){
  $parameters.LeaseWaitSeconds=0
  $heldLease=[IO.File]::Open((Join-Path $repo '.git/secode-landing.lock'),[IO.FileMode]::OpenOrCreate,[IO.FileAccess]::ReadWrite,[IO.FileShare]::None)
 }
 try{& $Script @parameters | Out-Null}catch{$threw=$true; $refusal=$_.Exception.Message; Write-Output ($case+': '+$_.Exception.Message)}finally{if($heldLease){$heldLease.Dispose()}}
 $hold=$case -in @('runtime-tracked-changed','runtime-prefix-changed','runtime-incoming','runtime-incoming-overlap','tracked-overlap','untracked-overlap','changed-during-gate','index-changed','missing-tier','invalid-tier','live-receipt-red','reuse-receipt-red','moved-overlapping','composition-red','proof-red','late-main','switched-branch','reuse-compose-red','lock-timeout','reuse-proof-noop','scp-red','scp-no-status','ssh-no-status','git-no-status')
 if($case -in @('lease-held','stale-expected-base','late-expected-base')){
  $hold=$true
  if($global:fakeMergeAttempted){throw 'refused nightly revert reached canonical merge'}
 }
 if($case -eq 'lease-held' -and ($global:fakeGitCalls -ne 0 -or $refusal -notmatch 'landing lease is held')){throw 'held canonical lease allowed repository access'}
 if($case -eq 'stale-expected-base' -and ($global:fakeCopies -ne 0 -or $refusal -notmatch 'attributed base')){throw 'stale attribution reached qualification'}
 if($case -eq 'late-expected-base' -and $refusal -notmatch 'advanced after attribution'){throw 'late canonical advance was composed instead of refused'}
 if($case.StartsWith('reviewed') -and $case -notin @('reviewed','reviewed-dirty')){
  $hold=$true
  if($global:fakeMergeAttempted){throw 'refused reviewed composition reached merge'}
  if($case -ne 'reviewed-red' -and $case -ne 'reviewed-late' -and $global:fakeCopies){throw 'invalid reviewed request reached qualification'}
 }
 if($case -eq 'reviewed-tier1'){
  $policyPath=Join-Path (Split-Path -Parent $Script) 'build/gate_policy.py'
  $policy=[regex]::Match([IO.File]::ReadAllText($policyPath), "(?m)^RETAINED_FULL_POLICY = '([^']+)'\r?$")
  if(-not $policy.Success -or $refusal -cne $policy.Groups[1].Value){throw 'Tier 1 reviewed refusal disagrees with the receipt verifier policy'}
 }
 if($case -in @('other-nvme-root','traversal-root','space-root')){
  $hold=$true
  if($global:fakeMergeAttempted -or $global:fakeReused -or $global:fakeCopies -or $refusal -notmatch 'Use a plain branch'){
   throw 'invalid lane root must refuse before qualification, copying or merge'
  }
 }
 if($case -in @('runtime-incoming','runtime-incoming-overlap') -and ($global:fakeMergeAttempted -or $global:fakeReused -or $refusal -notmatch 'Incoming diff touches StateEngine runtime')){throw 'incoming runtime path must refuse before qualification or merge'}
 if($case -in @('changed-during-gate','runtime-tracked-changed','runtime-prefix-changed') -and $global:fakeMergeAttempted){throw 'changed preserved bytes reached merge'}
 if($case -eq 'reuse-proof-noop' -and ($global:fakeMergeAttempted -or $refusal -notmatch 'does not descend')){throw 'ancestry must refuse before merge even when transport reports success'}
 if($case -eq 'ssh-no-status' -and $refusal -notmatch 'did not report an exit status'){throw 'missing SSH status did not refuse explicitly'}
 if($case -eq 'scp-no-status' -and $refusal -notmatch 'helper copy failed'){throw 'missing SCP status did not refuse explicitly'}
 if($case -eq 'git-no-status' -and $refusal -notmatch 'did not report an exit status'){throw 'missing git status did not refuse explicitly'}
 if($case -in @('reuse-compose-red','scp-red','scp-no-status','ssh-no-status','git-no-status') -and $global:fakeMergeAttempted){throw 'failed transport reached merge'}
 if($case -eq 'reuse-moved-disjoint' -and -not $threw -and $global:fakeCopies -ne (3*$helperNames.Count)){throw 'both bounded helper copies must execute'}
 if($case -eq 'lock-timeout' -and ($refusal -notmatch 'Heavy lock wait timed out' -or $global:fakeMergeAttempted)){throw 'lock timeout was disguised as composition failure'}
 if($case -in @('supplier-missing','supplier-space','supplier-traversal','supplier-shell','supplier-empty')){
  $hold=$true
  if($global:fakeMergeAttempted){throw 'missing or invalid supplier reached canonical merge'}
 }
 if(($case -like 'supplier-*') -and $case -notin @('supplier-custom','supplier-missing') -and ($global:fakeCopies -or $refusal -notmatch 'SupplierRoot requires')){throw 'invalid supplier root reached the remote process'}
 if($case -like 'node-*' -and $case -ne 'node-custom'){
  $hold=$true
  if($global:fakeMergeAttempted){throw 'missing or invalid Node reached canonical merge'}
 }
 if($case -in @('node-space','node-traversal','node-shell','node-empty') -and ($global:fakeCopies -or $refusal -notmatch 'NodeRoot requires')){throw 'invalid Node root reached the remote process'}
 if((& $gitExe -C $candidate rev-parse candidate) -ne $sha -or (& $gitExe -C $candidate branch --show-current) -ne 'candidate'){throw 'composition moved the reviewed owner branch'}
 $actual=(& $gitExe -C $repo rev-parse HEAD)
 $expectedLanding=if($global:fakeProofCandidate){$global:fakeProofCandidate}else{$sha}
 if($threw -ne $hold -or ($hold -and $actual -ne $global:fakeExpectedMain) -or (-not $hold -and $actual -ne $expectedLanding)){throw "wrong landing outcome: $case"}
 if($case -eq 'moved-overlapping' -and (& $gitExe -C $candidate rev-parse HEAD) -ne $sha){throw 'overlap rebased candidate'}
 if($case -eq 'runtime-rewritten' -and [IO.File]::ReadAllText((Join-Path $repo '.stateengine/atlas.sqlite')) -ne 'runtime-after'){throw 'runtime rewrite was lost'}
 if($case -eq 'runtime-paths-changed' -and ((Test-Path -LiteralPath (Join-Path $repo '.stateengine/atlas.sqlite')) -or [IO.File]::ReadAllText((Join-Path $repo '.stateengine/lock-holds.jsonl')) -ne 'runtime-after')){throw 'runtime path changes were lost'}
 if($case -in @('unrelated','reviewed-dirty') -and [IO.File]::ReadAllText((Join-Path $repo 'AGENTS.md')) -ne 'owned'){throw 'dirty bytes lost'}
 if($case -eq 'staged-unchanged'){
  if([IO.File]::ReadAllText((Join-Path $repo 'owned.txt')) -ne 'work-B' -or ((& $gitExe -C $repo show ':owned.txt') -join '') -ne 'staged-A'){throw 'staged or working bytes lost'}
 }
 if(-not $hold -and -not $global:fakeReused){throw 'receipt verification skipped'}
 if($case -eq 'lead-finished-target' -and $global:fakeFinishedSource -ne $sha){throw 'finished marker did not match the preserved owner source'}
 Write-Output "PASS $case"
}
Write-Output "$($cases.Count) landing integration probes passed; fixtures retained at $root"
