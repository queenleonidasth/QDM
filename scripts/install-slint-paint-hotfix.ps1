param(
  [string]$EvidenceDirectory = 'docs\evidence\slint-idm-20261027'
)
$ErrorActionPreference='Stop'
$root=Split-Path -Parent $PSScriptRoot
$evidence=Join-Path $root $EvidenceDirectory
$package=Get-Content -Raw (Join-Path $evidence 'packaging.json') -Encoding UTF8|ConvertFrom-Json
$target=Join-Path $root 'target\release\QDM.exe'
$sourceHash=(Get-FileHash -LiteralPath $target -Algorithm SHA256).Hash.ToLowerInvariant()
if($package.status -cne 'passed' -or $package.binarySha256 -cne $sourceHash){throw 'Candidate was not packaged with passing evidence.'}
. (Join-Path $PSScriptRoot 'release-gates.ps1')
$gates=@(Assert-QdmReleaseGates -EvidenceDirectory $evidence -BinarySha256 $sourceHash)
if($gates.Count -ne 9){throw 'Not all release gates passed.'}
$installer=Join-Path $root 'release\QDM-0.3.0-win-x64-setup.exe'
$declared=@($package.artifacts|Where-Object{$_.name -eq 'QDM-0.3.0-win-x64-setup.exe'})
if($declared.Count -ne 1 -or (Get-FileHash $installer -Algorithm SHA256).Hash.ToLowerInvariant() -cne $declared[0].sha256){throw 'Installer hash mismatch.'}
$installed=Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$state=Join-Path $env:APPDATA 'QDM\state-v2.json'
if(!(Test-Path $state)){throw 'Installed state not found; unsafe to assume migration.'}
$running=@(Get-Process QDM -ErrorAction SilentlyContinue)
if($running.Count){throw 'QDM is running; exit QDM gracefully before updating.'}
$beforeJson=Get-Content $state -Raw -Encoding UTF8|ConvertFrom-Json
$active=@($beforeJson.tasks|Where-Object{$_.view.status -in @('downloading','probing','assembling','verifying')})
if($active.Count){throw 'Active QDM jobs found; pause and save their state before updating.'}
$backup=Join-Path (Split-Path $root -Parent) ('backups\slint-paint-update-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $backup -Force|Out-Null
Copy-Item $installed (Join-Path $backup 'QDM-previous.exe') -Force
Copy-Item $state (Join-Path $backup 'state-v2-before.json') -Force
$oldHash=(Get-FileHash $installed).Hash.ToLowerInvariant()
$oldStateHash=(Get-FileHash $state).Hash.ToLowerInvariant()
$beforeRecord=foreach($t in $beforeJson.tasks){
  [pscustomobject]@{id=$t.view.id;status=$t.view.status;bytes=$t.view.downloaded_bytes;parts=@($t.parts).Count;workers=@($t.lane_activity).Count;filePath=$t.view.file_path;partMetaHash=(ConvertTo-Json -InputObject @($t.parts) -Depth 40 -Compress);workerMetaHash=(ConvertTo-Json -InputObject @($t.lane_activity) -Depth 40 -Compress)}
}
try {
  $proc=Start-Process -FilePath $installer -ArgumentList '/S' -PassThru -Wait
  if($proc.ExitCode -ne 0){throw ('Installer returned exit code '+$proc.ExitCode)}
  if((Get-FileHash $installed).Hash.ToLowerInvariant() -cne $sourceHash){throw 'Installed QDM hash does not match candidate.'}
  if((Get-FileHash $state).Hash.ToLowerInvariant() -cne $oldStateHash){throw 'Installer unexpectedly altered saved history before first launch.'}
  $new=Start-Process -FilePath $installed -PassThru
  $end=[DateTime]::UtcNow.AddSeconds(25)
  do {
    Start-Sleep -Milliseconds 200
    $new.Refresh()
    if($new.HasExited){throw ('New QDM exited unexpectedly '+$new.ExitCode)}
  } while($new.MainWindowHandle -eq [IntPtr]::Zero -and [DateTime]::UtcNow -lt $end)
  if($new.MainWindowHandle -eq [IntPtr]::Zero){throw 'QDM did not display a usable window.'}
  $afterJson=Get-Content -Raw $state -Encoding UTF8|ConvertFrom-Json
  if($afterJson.settings.connections -ne $beforeJson.settings.connections){throw 'Connection setting changed.'}
  if(@($afterJson.tasks).Count -ne @($beforeJson.tasks).Count){throw 'Download history count changed.'}
  foreach($old in $beforeRecord){
    $same=@($afterJson.tasks|Where-Object{$_.view.id -ceq $old.id})
    if($same.Count -ne 1){throw 'Download task identity changed.'}
    if($same[0].view.status -cne $old.status -or $same[0].view.downloaded_bytes -ne $old.bytes -or @($same[0].parts).Count -ne $old.parts -or @($same[0].lane_activity).Count -ne $old.workers){throw 'Historical task metadata changed.'}
    if((ConvertTo-Json -InputObject @($same[0].parts) -Depth 40 -Compress) -cne $old.partMetaHash){throw 'Saved part layout changed.'}
    if((ConvertTo-Json -InputObject @($same[0].lane_activity) -Depth 40 -Compress) -cne $old.workerMetaHash){throw 'Saved worker attribution changed.'}
  }
  $report=[ordered]@{schema=1;status='passed';installedAt=[DateTime]::UtcNow.ToString('o');installedPath=$installed;binarySha256=$sourceHash;previousBinarySha256=$oldHash;originalStateSha256=$oldStateHash;backup=$backup;testGates=$gates.Count;downloadCount=@($afterJson.tasks).Count;configuredConnections=$afterJson.settings.connections;processId=$new.Id;windowTitle=$new.MainWindowTitle;historyRecordsPreserved=$true}
  $report|ConvertTo-Json -Depth 6|Set-Content -LiteralPath (Join-Path $evidence 'installed-paint-hotfix.json') -Encoding UTF8
  $report|ConvertTo-Json -Depth 4
}catch{
  # No automated process termination or state overwrite: a GUI may be active.
  # Previous binary and state remain in $backup for a controlled recovery.
  throw ('QDM update verification failed; backup retained at '+$backup+'. '+$_.Exception.Message)
}
