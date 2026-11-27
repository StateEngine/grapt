param([Parameter(Mandatory=$true)][string]$RunRoot)
$ErrorActionPreference='Stop'
$ProgressPreference='SilentlyContinue'
if ([IO.Path]::GetFullPath($RunRoot) -cne [IO.Path]::GetFullPath($PSScriptRoot) -or
    (Split-Path -Leaf $RunRoot) -cnotmatch '^[0-9a-f]{32}$' -or
    (Split-Path -Leaf (Split-Path -Parent $RunRoot)) -cne 'runs') { throw 'Invalid native run root' }
$python='C:/Python314/python.exe'
$worker=Join-Path $RunRoot 'windows_native_worker.py'
try {
    $p=Start-Process -FilePath $python -ArgumentList @('-B',$worker,$RunRoot) -WindowStyle Hidden -Wait -PassThru `
        -RedirectStandardOutput (Join-Path $RunRoot 'worker.stdout') -RedirectStandardError (Join-Path $RunRoot 'worker.stderr')
    $code=$p.ExitCode
} catch {
    $_.Exception.Message | Set-Content -LiteralPath (Join-Path $RunRoot 'launcher-error.txt')
    $code=1
}
# The worker's job handle has closed before this completion marker is published.
[IO.File]::WriteAllText((Join-Path $RunRoot 'done.json'), ('{"exit":'+$code+'}'))
exit $code
