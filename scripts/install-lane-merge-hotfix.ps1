$ErrorActionPreference='Stop'
$source='C:\Users\QUEEN\Projects\QDM\native\target\release\QDM.exe'
$installed=Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$state=Join-Path $env:APPDATA 'QDM\state-v2.json'
$backup=Join-Path $env:LOCALAPPDATA ('QDM-LanesMerge-Backup-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
if(!(Test-Path -LiteralPath $source) -or !(Test-Path -LiteralPath $installed) -or !(Test-Path -LiteralPath $state)){
 throw 'Missing executable/state; no changes made'
}
$before=Get-Content $state -Raw|ConvertFrom-Json
$jobs=@($before.tasks)
if($jobs.Count -eq 0){throw 'Download history absent; refusing unchecked replacement'}
$active=@($jobs|Where-Object {$_.view.status -notin @('completed','paused','error','failed','cancelled','canceled','scheduled','queued')})
if($active.Count -gt 0){throw 'An active job is running; update withheld rather than interrupting it'}
$known=@($jobs|ForEach-Object {
  [pscustomobject]@{
    Id=$_.view.id;Status=$_.view.status;Downloaded=[long]$_.view.downloaded_bytes;
    FilePath=$_.view.file_path;Size=[long]$_.view.total_bytes;Parts=@($_.parts).Count
  }
})
foreach($job in $known|Where-Object {$_.Status -eq 'completed'}){
 if(!(Test-Path -LiteralPath $job.FilePath)){throw ('Completed file missing: '+$job.FilePath)}
 if((Get-Item -LiteralPath $job.FilePath).Length -ne $job.Size){throw 'Completed file size mismatch'}
}
$running=@(Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|Where-Object {$_.ExecutablePath -ieq $installed})
if($running.Count -ne 1){throw "Expected one installed QDM process; found $($running.Count)"}
$main=(Get-Process -Id ([int]$running[0].ProcessId)).MainWindowHandle
if(!$main){throw 'No QDM main window'}
New-Item -ItemType Directory -Path $backup -Force |Out-Null
Copy-Item -LiteralPath $state -Destination (Join-Path $backup 'state-v2-before.json') -Force
Copy-Item -LiteralPath $installed -Destination (Join-Path $backup 'QDM-before.exe') -Force
$known|ConvertTo-Json -Depth 4|Set-Content -LiteralPath (Join-Path $backup 'jobs-before.json') -Encoding UTF8
Add-Type -TypeDefinition 'using System;using System.Runtime.InteropServices;public static class QdmUpdate { [DllImport("user32.dll")] public static extern bool PostMessage(IntPtr h,int m,IntPtr w,IntPtr l); }'
if(-not [QdmUpdate]::PostMessage([IntPtr]$main,273,[IntPtr]1106,[IntPtr]::Zero)){throw 'Graceful exit request failed; original kept'}
for($i=0;$i -lt 48;$i++){
 if(!(Get-Process -Id ([int]$running[0].ProcessId) -ErrorAction SilentlyContinue)){break}
 Start-Sleep -Milliseconds 250
}
if(Get-Process -Id ([int]$running[0].ProcessId) -ErrorAction SilentlyContinue){throw 'QDM did not exit; aborting installation'}
$newHash=(Get-FileHash -LiteralPath $source -Algorithm SHA256).Hash
try{
 Copy-Item -LiteralPath $source -Destination $installed -Force
 $copied=(Get-FileHash -LiteralPath $installed -Algorithm SHA256).Hash
 if($copied -ne $newHash){throw 'Executable checksum mismatch'}
}catch{
 Copy-Item -LiteralPath (Join-Path $backup 'QDM-before.exe') -Destination $installed -Force
 throw
}
Start-Process -FilePath $installed
Start-Sleep -Seconds 2
$new=Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|Where-Object {$_.ExecutablePath -ieq $installed}|Select-Object -First 1
if(!$new){throw 'New executable failed to launch'}
$after=Get-Content $state -Raw|ConvertFrom-Json
foreach($old in $known){
 $newJob=@($after.tasks|Where-Object {$_.view.id -eq $old.Id})|Select-Object -First 1
 if(!$newJob -or [long]$newJob.view.downloaded_bytes -lt $old.Downloaded){throw "History/progress missing for $($old.Id)"}
 if($old.Status -eq 'completed' -and ($newJob.view.status -ne 'completed' -or !(Test-Path -LiteralPath $old.FilePath))){throw 'Completed file state lost'}
}
'INSTALL=PASS'
'NEW_PID='+$new.ProcessId
'EXE_SHA256='+$newHash
'BACKUP='+$backup
'COMPLETED_JOBS='+@($known|Where-Object {$_.Status -eq 'completed'}).Count
'DOWNLOADED_BYTES_PRESERVED='+($known|Measure-Object -Property Downloaded -Sum).Sum
