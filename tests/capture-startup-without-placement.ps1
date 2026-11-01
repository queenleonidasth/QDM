param([string]$Binary='target\release\QDM.exe',[string]$OutputDirectory='docs\evidence\slint-idm-20261027\startup09')
$ErrorActionPreference='Stop'
$qdmRoot=Split-Path -Parent $PSScriptRoot
$qdmBinary=[IO.Path]::GetFullPath((Join-Path $qdmRoot $Binary))
$qdmOutput=Join-Path $qdmRoot $OutputDirectory
New-Item -ItemType Directory -Force $qdmOutput | Out-Null
if(@(Get-Process -Name QDM -ErrorAction SilentlyContinue).Count){throw 'Run this diagnostic only after all owned benchmark applications exit.'}
$qdmState=Join-Path $env:APPDATA 'QDM\state-v2.json'
$qdmStateBefore=(Get-FileHash $qdmState).Hash.ToLowerInvariant()
$qdmFfmpeg=Join-Path $env:LOCALAPPDATA 'Microsoft\WinGet\Links\ffmpeg.exe'
$qdmStarted=[DateTime]::UtcNow.ToString('o')
$qdmApp=Start-Process -FilePath $qdmBinary -WorkingDirectory $qdmRoot -PassThru
$qdmDeadline=[DateTime]::UtcNow.AddSeconds(20)
do{$qdmApp.Refresh();if($qdmApp.HasExited){throw 'QDM exited before first window capture'};if($qdmApp.MainWindowHandle -ne [IntPtr]::Zero -and $qdmApp.MainWindowTitle -like 'QDM*QUEEN Download Manager*'){break};Start-Sleep -Milliseconds 20}while([DateTime]::UtcNow -lt $qdmDeadline)
if($qdmApp.MainWindowHandle -eq [IntPtr]::Zero -or $qdmApp.MainWindowTitle -notlike 'QDM*QUEEN Download Manager*'){throw 'No owned main window observed'}
$qdmVideo=Join-Path $qdmOutput 'native-startup-before-printwindow.mp4'
$qdmFfmpegLog=Join-Path $qdmOutput 'capture.log'
$qdmCaptureStarted=[DateTime]::UtcNow.ToString('o')
$qdmSavedPreference=$ErrorActionPreference;$ErrorActionPreference='Continue'
& $qdmFfmpeg -hide_banner -loglevel warning -nostdin -y -f gdigrab -framerate 30 -draw_mouse 0 -i ('hwnd='+$qdmApp.MainWindowHandle.ToInt64()) -t 4 -an -vf 'pad=ceil(iw/2)*2:ceil(ih/2)*2' -c:v libx264 -preset veryfast -crf 18 -pix_fmt yuv420p -movflags +faststart $qdmVideo 2> $qdmFfmpegLog
$qdmCode=$LASTEXITCODE;$ErrorActionPreference=$qdmSavedPreference
if($qdmCode -ne 0){throw ('Owned-window recording failed: '+$qdmCode)}
$qdmStill=Join-Path $qdmOutput 'native-startup-after-recording.png'
& (Join-Path $PSScriptRoot 'capture-window.ps1') -TargetProcessId $qdmApp.Id -OutputPath $qdmStill | Out-Null
$qdmStateAfter=(Get-FileHash $qdmState).Hash.ToLowerInvariant()
if($qdmStateBefore -cne $qdmStateAfter){throw 'The original state file changed during read-only startup observation'}
$qdmReport=[ordered]@{
 schema=1;status='captured_pending_review';startedAt=$qdmStarted;captureStartedAt=$qdmCaptureStarted;finishedAt=[DateTime]::UtcNow.ToString('o')
 binary=$qdmBinary;binarySha256=(Get-FileHash $qdmBinary).Hash.ToLowerInvariant();processId=$qdmApp.Id;hwnd=$qdmApp.MainWindowHandle.ToInt64()
 profile='Actual installed profile, no download command issued';stateBefore=$qdmStateBefore;stateAfter=$qdmStateAfter
 method='First capture is exact-HWND gdigrab. No placement, resize, UIA, PrintWindow or forced invalidation before recording. PrintWindow still is taken only after recording.'
 video=[ordered]@{path=$qdmVideo;bytes=(Get-Item $qdmVideo).Length;sha256=(Get-FileHash $qdmVideo).Hash.ToLowerInvariant()}
 still=[ordered]@{path=$qdmStill;bytes=(Get-Item $qdmStill).Length;sha256=(Get-FileHash $qdmStill).Hash.ToLowerInvariant()}
}
$qdmReport | ConvertTo-Json -Depth 5 | Set-Content (Join-Path $qdmOutput 'startup-capture.json') -Encoding UTF8
$qdmReport | ConvertTo-Json -Depth 5
