param([switch]$Apply)
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$source=Join-Path $root 'target\release\QDM.exe'
$installed=Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$data=Join-Path $env:APPDATA 'QDM'
$oldState=Join-Path $data 'state-v2.json'
$newDb=Join-Path $data 'state-v3.sqlite3'
$parts=Join-Path $data 'parts-v2'
$reportAcceptance=Join-Path $env:TEMP 'qdm-v040-final-acceptance.json'
$reportAdaptive=Join-Path $env:TEMP 'qdm-v040-final-adaptive.json'
if(!(Test-Path $source) -or !(Test-Path $installed) -or !(Test-Path $oldState)) {
  throw 'Missing new release, installed binary, or legacy database; refusing migration.'
}
$expected=(Get-FileHash $source -Algorithm SHA256).Hash.ToLowerInvariant()
$a=Get-Content $reportAcceptance -Raw|ConvertFrom-Json
$b=Get-Content $reportAdaptive -Raw|ConvertFrom-Json
if($a.passed -ne 35 -or $a.failed -ne 0 -or !$a.binaryUnchangedDuringRun -or $a.binarySha256 -ne $expected) {
  throw 'QDM 0.4 release acceptance gate missing or does not match executable.'
}
if($b.result -ne 'PASS' -or $b.binarySha256 -ne $expected -or $b.binarySha256After -ne $expected) {
  throw 'QDM 0.4 adaptive origin gate missing or failed.'
}
$before=Get-Content -LiteralPath $oldState -Raw|ConvertFrom-Json
$unfinished=@($before.tasks|Where-Object {$_.view.status -in @('downloading','probing','assembling','preparing','verifying','connecting')})
if($unfinished.Count -gt 0){throw "Detected $($unfinished.Count) live legacy tasks; cannot close the app safely."}
$expectedImported=@($before.tasks|Where-Object {$_.view.status -eq 'completed'}).Count
$oldHash=(Get-FileHash $installed -Algorithm SHA256).Hash
$oldStateHash=(Get-FileHash $oldState -Algorithm SHA256).Hash
$running=@(Get-CimInstance Win32_Process -Filter "name='QDM.exe'" | Where-Object {$_.ExecutablePath -ieq $installed})
$plan=[ordered]@{mode=$(if($Apply){'APPLY'}else{'DRY_RUN'});release='0.4.0';releaseSha256=$expected;
 oldSha256=$oldHash;importCompleted=$expectedImported;activeJobs=$unfinished.Count;
 runningInstances=$running.Count;gateAcceptance=$a.passed;gateAdaptive=$b.result;
 legacyPartsPath=$parts;completedUserFilesWillNotBeRemoved=$true}
if(!$Apply){$plan|ConvertTo-Json;return}
$backup=Join-Path $root ('backups\install-v040-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $backup|Out-Null
Copy-Item -LiteralPath $installed -Destination (Join-Path $backup 'QDM-previous.exe') -Force
Copy-Item -LiteralPath $oldState -Destination (Join-Path $backup 'state-v2.json') -Force
$pair=Join-Path $data 'browser-pairing-v2.bin'
if(Test-Path $pair){Copy-Item -LiteralPath $pair -Destination (Join-Path $backup 'browser-pairing-v2.bin') -Force}
$stage=Join-Path (Split-Path $installed -Parent) 'QDM.exe.direct-stage'
Copy-Item $source $stage -Force
if((Get-FileHash $stage -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected) {throw 'Staging SHA mismatch'}
$newProcess=$null
$installedNew=$false
try {
  foreach($r in $running){
    $p=Get-Process -Id $r.ProcessId -ErrorAction Stop
    if($p.MainWindowHandle -ne [IntPtr]::Zero){[void]$p.CloseMainWindow();[void]$p.WaitForExit(3000)}
    $p.Refresh()
    if(!$p.HasExited){
      $check=Get-Content -LiteralPath $oldState -Raw|ConvertFrom-Json
      if(@($check.tasks|Where-Object {$_.view.status -in @('downloading','probing','assembling','preparing','verifying')}).Count -gt 0){throw 'Active job appeared before upgrade'}
      Stop-Process -Id $p.Id -Force -ErrorAction Stop
      [void]$p.WaitForExit(10000)
    }
  }
  if((Get-FileHash $oldState -Algorithm SHA256).Hash -ne $oldStateHash){throw 'Legacy state changed before migration'}
  Copy-Item -LiteralPath $stage -Destination $installed -Force
  if((Get-FileHash $installed -Algorithm SHA256).Hash.ToLowerInvariant() -ne $expected){throw 'Installed executable SHA mismatch'}
  $installedNew=$true
  $newProcess=Start-Process -FilePath $installed -PassThru
  $windowReady=$false
  for($i=0;$i -lt 120;$i++){
    Start-Sleep -Milliseconds 150
    $newProcess.Refresh()
    if($newProcess.HasExited){throw 'QDM 0.4 failed to start'}
    if($newProcess.MainWindowTitle -eq 'QDM v0.4.0 - QUEEN Download Manager'){
      $windowReady=$true;break
    }
  }
  if(!$windowReady){throw 'Expected QDM 0.4.0 native GUI title did not appear'}
  if(!(Test-Path $newDb)){throw 'QDM did not create SQLite WAL database'}
  $check=& python (Join-Path $root 'scripts\check_sqlite_install.py') $newDb | ConvertFrom-Json
  if($LASTEXITCODE -ne 0 -or $null -eq $check){throw 'SQLite verification command failed'}
  if($check.downloads -ne $expectedImported){throw "SQLite import mismatch: $($check.downloads), expected $expectedImported"}
  if($check.settings.connections -ne $before.settings.connections -or $check.settings.max_concurrent -ne $before.settings.max_concurrent){
    throw 'Connection settings not preserved'
  }
  if($check.journal -ne 'wal'){throw 'SQLite journal mode is not WAL'}
  if((Test-Path $pair) -and (Get-FileHash $pair -Algorithm SHA256).Hash -ne
     (Get-FileHash (Join-Path $backup 'browser-pairing-v2.bin') -Algorithm SHA256).Hash){throw 'Browser-pairing settings changed'}
  # The user explicitly authorized deletion of legacy metadata/parts.
  # Cleanup is after v0.4 SQLite import and native GUI verification.
  Remove-Item -LiteralPath $oldState -Force
  if(Test-Path $parts){Remove-Item -LiteralPath $parts -Recurse -Force}
  $legacyTemp=Join-Path $data 'state-v2.tmp'
  if(Test-Path $legacyTemp){Remove-Item -LiteralPath $legacyTemp -Force}
  $receipt=[ordered]@{
     status='INSTALLED';installedAt=(Get-Date).ToString('o');version='0.4.0';
     path=$installed;sha256=$expected;processId=$newProcess.Id;sqlitePath=$newDb;
     sqliteWal=$true;importedCompleted=$check.downloads;connections=$check.settings.connections;
     downloadsAtOnce=$check.settings.max_concurrent;legacyHistoryRemoved=$true;
     legacyPartDirectoryRemoved=!(Test-Path $parts);browserPairingPreserved=$true;
     backup=$backup;acceptance='35/35';adaptive='PASS'
  }
  $receipt|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $root 'docs\QDM-DIRECT-SQLITE-INSTALLED-2026-11-01.json') -Encoding UTF8
  $receipt|ConvertTo-Json -Depth 6
}catch{
  $why=$_
  if($installedNew){
    if($newProcess -and !$newProcess.HasExited){Stop-Process -Id $newProcess.Id -Force -ErrorAction SilentlyContinue}
    Copy-Item -LiteralPath (Join-Path $backup 'QDM-previous.exe') -Destination $installed -Force
    if(!(Test-Path $oldState)){Copy-Item -LiteralPath (Join-Path $backup 'state-v2.json') -Destination $oldState -Force}
    Start-Process -FilePath $installed | Out-Null
  }
  throw "QDM 0.4 upgrade rolled back (where possible); inspect: $why"
}finally{
  if(Test-Path $stage){Remove-Item $stage -Force -ErrorAction SilentlyContinue}
}
