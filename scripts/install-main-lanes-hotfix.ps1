$ErrorActionPreference='Stop'
$root='C:\Users\QUEEN\Projects\QDM\native'
$source=Join-Path $root 'target\release\QDM.exe'
$installed=Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$stateFile=Join-Path $env:APPDATA 'QDM\state-v2.json'
$expectedId='d1b67096-8e95-4726-804f-8e8c42ed1c5e'
$backup=Join-Path $env:LOCALAPPDATA ('QDM-MainLanes-Backup-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
if(!(Test-Path $source) -or !(Test-Path $installed) -or !(Test-Path $stateFile)){throw 'Required files absent; refusing update'}
$wantedHash=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
$before=Get-Content $stateFile -Raw|ConvertFrom-Json
$task=@($before.tasks)|Where-Object {$_.view.id -eq $expectedId}|Select-Object -First 1
if(!$task){throw 'Current download was not found; refusing to replace'}
$beforeBytes=[long]$task.view.downloaded_bytes
$partsDir=Join-Path $env:APPDATA ('QDM\parts-v2\'+$expectedId)
if(!(Test-Path $partsDir)){throw 'Part files absent'}
$old=Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|
  Where-Object {$_.ExecutablePath -ieq $installed}|Select-Object -First 1
if(!$old){throw 'Cannot identify the installed QDM process'}
$pidOld=[int]$old.ProcessId
$proc=Get-Process -Id $pidOld -ErrorAction Stop
if(!$proc.MainWindowHandle){throw 'Cannot locate QDM main window'}
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class QdmSafeUpdate { [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h, int m, IntPtr w, IntPtr l); }'
function Send-Qdm([IntPtr]$window,[int]$code){
    if(-not [QdmSafeUpdate]::PostMessage($window,273,[IntPtr]$code,[IntPtr]::Zero)){throw "QDM command failed $code"}
}
Send-Qdm $proc.MainWindowHandle 1104
$paused=$false
for($i=0;$i -lt 45;$i++){
    Start-Sleep -Milliseconds 400
    $state=Get-Content $stateFile -Raw|ConvertFrom-Json
    $current=@($state.tasks)|Where-Object {$_.view.id -eq $expectedId}|Select-Object -First 1
    if($current.view.status -eq 'paused'){$paused=$true;break}
}
if(-not $paused){throw 'Safe pause not confirmed; old QDM left running'}
New-Item -ItemType Directory -Force -Path $backup|Out-Null
Copy-Item -LiteralPath $stateFile -Destination (Join-Path $backup 'state-v2-before.json')
Copy-Item -LiteralPath $installed -Destination (Join-Path $backup 'QDM-before.exe')
Get-ChildItem -LiteralPath $partsDir -File|Select-Object Name,Length|
    ConvertTo-Json|Set-Content -LiteralPath (Join-Path $backup 'parts-inventory.json') -Encoding UTF8
$savedBytes=[long]$current.view.downloaded_bytes
if($savedBytes -lt $beforeBytes){throw 'Progress became smaller after pausing; no replacement'}
Send-Qdm $proc.MainWindowHandle 1106
for($i=0;$i -lt 40;$i++){
    if(!(Get-Process -Id $pidOld -ErrorAction SilentlyContinue)){break}
    Start-Sleep -Milliseconds 250
}
if(Get-Process -Id $pidOld -ErrorAction SilentlyContinue){throw 'QDM did not exit; old file remains installed'}
Copy-Item -LiteralPath $source -Destination $installed -Force
if((Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash -ne $wantedHash){
    Copy-Item -LiteralPath (Join-Path $backup 'QDM-before.exe') -Destination $installed -Force
    throw 'Install verification failed; rolled back'
}
$recovered=Get-Content $stateFile -Raw|ConvertFrom-Json
$task=@($recovered.tasks)|Where-Object {$_.view.id -eq $expectedId}|Select-Object -First 1
if(!$task -or $task.view.downloaded_bytes -lt $savedBytes -or !(Test-Path $partsDir)){
   throw 'Part/state verification failed; new executable installed but not launched'
}
Start-Process -FilePath $installed
$newProc=$null
for($i=0;$i -lt 32;$i++){
    Start-Sleep -Milliseconds 350
    $newProc=Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|
        Where-Object {$_.ExecutablePath -ieq $installed}|Select-Object -First 1
    if($newProc){break}
}
if(!$newProc){throw 'New QDM did not start'}
$newMain=Get-Process -Id ([int]$newProc.ProcessId)
for($i=0;$i -lt 20 -and !$newMain.MainWindowHandle;$i++){
    Start-Sleep -Milliseconds 300
    $newMain=Get-Process -Id ([int]$newProc.ProcessId)
}
if(!$newMain.MainWindowHandle){throw 'New QDM main window absent'}
Send-Qdm $newMain.MainWindowHandle 1105
Write-Output 'INSTALL=PASS'
Write-Output ('SHA256='+$wantedHash)
Write-Output ('BACKUP='+$backup)
Write-Output ('PREVIOUS_BYTES='+$beforeBytes)
Write-Output ('PRESERVED_BYTES='+$savedBytes)
Write-Output ('PARTS='+@(Get-ChildItem -LiteralPath $partsDir -File).Count)
Write-Output ('NEW_PID='+$newProc.ProcessId)
Write-Output 'RESUME_REQUESTED=YES'
