param([Parameter(Mandatory=$true)][string]$ControllerDirectory,
      [string]$ControllerRoot = $env:SECODE_WINDOWS_CONTROLLER_ROOT)
$ErrorActionPreference='Stop'
. (Join-Path $PSScriptRoot 'operator_environment.ps1')
if (-not $PSBoundParameters.ContainsKey('ControllerRoot')) { $ControllerRoot = $env:SECODE_WINDOWS_CONTROLLER_ROOT }
$ProgressPreference='SilentlyContinue'
if (-not $ControllerRoot -or -not [IO.Path]::IsPathRooted($ControllerRoot)) { throw 'Set SECODE_WINDOWS_CONTROLLER_ROOT to the controller parent.' }
$controllerPattern = '^' + [regex]::Escape($ControllerRoot.TrimEnd('/')) + '/[0-9a-f]{9,40}$'
if ($ControllerDirectory -cnotmatch $controllerPattern) { throw 'Choose a fresh version under the configured controller root.' }
$python='C:/Python314/pythonw.exe'
if (-not [IO.File]::Exists($python)) { throw 'Native Python 3.14 is required.' }
if (Test-Path -LiteralPath $ControllerDirectory) { throw 'Preserve the existing controller version.' }
$name='SECode-NativeTempRetention'
$existing=Get-ScheduledTask -TaskName $name -ErrorAction SilentlyContinue
if ($existing -and ($existing.Actions.Count -ne 1 -or $existing.Actions[0].Execute -cne $python -or
        $existing.Actions[0].Arguments -cnotmatch ('^-B "' + [regex]::Escape($ControllerRoot.TrimEnd('/')) + '/[0-9a-f]{9,40}/windows_retention.py" --apply$'))) {
    throw 'Preserve the customized retention task.'
}
$source=Join-Path $PSScriptRoot 'windows_retention.py'
if (-not [IO.File]::Exists($source) -or (Get-Item -LiteralPath $source).Length -gt 65536) {
    throw 'A bounded reviewed retention helper is required.'
}
New-Item -ItemType Directory -Path $ControllerDirectory | Out-Null
$script=Join-Path $ControllerDirectory 'windows_retention.py'
Copy-Item -LiteralPath (Join-Path $PSScriptRoot 'operator_paths.py') -Destination (Join-Path $ControllerDirectory 'operator_paths.py')
Copy-Item -LiteralPath $source -Destination $script
(Get-Item -LiteralPath (Join-Path $ControllerDirectory 'operator_paths.py')).IsReadOnly=$true
(Get-Item -LiteralPath $script).IsReadOnly=$true
$action=New-ScheduledTaskAction -Execute $python -Argument ('-B "'+$ControllerDirectory+'/windows_retention.py" --apply')
$principal=New-ScheduledTaskPrincipal -UserId ([Security.Principal.WindowsIdentity]::GetCurrent().Name) -LogonType Interactive -RunLevel Limited
$settings=New-ScheduledTaskSettingsSet -ExecutionTimeLimit (New-TimeSpan -Minutes 5) -MultipleInstances IgnoreNew -Hidden
$trigger=New-ScheduledTaskTrigger -Daily -At '05:00'
Register-ScheduledTask -TaskName $name -Action $action -Trigger $trigger -Principal $principal -Settings $settings -Force | Out-Null
Write-Output 'Enabled seven-day retention for completed native SE Code temporary runs.'
