$ErrorActionPreference='Stop'
$source='C:\Users\QUEEN\Projects\QDM\native\target\release\QDM.exe'
$dst=Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$state=Join-Path $env:APPDATA 'QDM\state-v2.json'
$backups=@(Get-ChildItem $env:LOCALAPPDATA -Directory -Filter 'QDM-LanesMerge-Backup-*'|Sort-Object LastWriteTime -Descending)
if(!$backups.Count){throw 'No backup from first safe install attempt'}
$backup=$backups[0].FullName
foreach($p in @($source,$dst,$state,(Join-Path $backup 'QDM-before.exe'),(Join-Path $backup 'state-v2-before.json'))){
 if(!(Test-Path -LiteralPath $p)){throw "Missing required file: $p"}
}
if(Get-Process QDM -ErrorAction SilentlyContinue){throw 'QDM is running; aborting'}
$j=Get-Content $state -Raw|ConvertFrom-Json
$jobs=@($j.tasks)
if(!$jobs.Count){throw 'No saved tasks'}
if(@($jobs|Where-Object {$_.view.status -notin @('completed','paused','queued','error','failed','cancelled','canceled','scheduled')}).Count){
 throw 'Active work cannot be interrupted'
}
foreach($job in $jobs|Where-Object {$_.view.status -eq 'completed'}){
 if(!(Test-Path $job.view.file_path)){throw 'Completed file absent'}
 if((Get-Item $job.view.file_path).Length -ne [long]$job.view.total_bytes){throw 'Completed file size mismatch'}
}
$hash=(Get-FileHash $source -Algorithm SHA256).Hash
Copy-Item -LiteralPath $source -Destination $dst -Force
if((Get-FileHash $dst -Algorithm SHA256).Hash -ne $hash){
 Copy-Item (Join-Path $backup 'QDM-before.exe') $dst -Force
 throw 'Installed EXE hash mismatch, restored prior EXE'
}
Start-Process $dst
Start-Sleep -Seconds 2
$new=Get-CimInstance Win32_Process -Filter "Name = 'QDM.exe'"|Where-Object {$_.ExecutablePath -ieq $dst}|Select-Object -First 1
if(!$new){throw 'New QDM process did not start'}
$after=Get-Content $state -Raw|ConvertFrom-Json
foreach($old in $jobs){
 $newTask=@($after.tasks|Where-Object {$_.view.id -eq $old.view.id})|Select-Object -First 1
 if(!$newTask){throw 'Task disappeared'}
 if($newTask.view.status -ne $old.view.status){throw 'Task status changed after update'}
 if([long]$newTask.view.downloaded_bytes -ne [long]$old.view.downloaded_bytes){throw 'Saved progress changed'}
}
'INSTALL=PASS'
'PID='+$new.ProcessId
'SHA256='+$hash
'BACKUP='+$backup
'COMPLETED_FILE_COUNT='+@($jobs|Where-Object {$_.view.status -eq 'completed'}).Count
'COMPLETED_BYTES='+($jobs|Measure-Object -InputObject {$_.view.downloaded_bytes} -Sum).Sum
