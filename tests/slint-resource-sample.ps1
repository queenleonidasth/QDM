param([Parameter(Mandatory=$true)][int]$TargetProcessId,[Parameter(Mandatory=$true)][string]$OutputPath,[switch]$HideWindow,[string]$Label='Slint GUI')
$ErrorActionPreference='Stop'
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class QdmResourceWindow {
 [DllImport("user32.dll")] public static extern bool ShowWindow(IntPtr h,int command);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool GetClientRect(IntPtr h,out QdmResourceRectangle rect);
 [DllImport("user32.dll")] public static extern uint GetDpiForWindow(IntPtr h);
}
[StructLayout(LayoutKind.Sequential)] public struct QdmResourceRectangle { public int left,top,right,bottom; }
'@
$qdmResourceProcess=Get-Process -Id $TargetProcessId -ErrorAction Stop
$qdmResourceWatch=[Diagnostics.Stopwatch]::StartNew()
$qdmResourceWindowFound=$false
do{$qdmResourceProcess.Refresh();$qdmResourceHandle=$qdmResourceProcess.MainWindowHandle;if($qdmResourceHandle -ne [IntPtr]::Zero -and $qdmResourceProcess.MainWindowTitle -like 'QDM*QUEEN Download Manager*'){$qdmResourceWindowFound=$true;break};Start-Sleep -Milliseconds 50}while($qdmResourceWatch.ElapsedMilliseconds -lt 10000)
if(-not $qdmResourceWindowFound){throw 'Owned Slint product window was not found.'}
$qdmResourceClientRect=New-Object QdmResourceRectangle
if(-not [QdmResourceWindow]::GetClientRect($qdmResourceHandle,[ref]$qdmResourceClientRect)){throw 'Could not measure the owned Slint client area.'}
$qdmWindowObservation=[ordered]@{
 processId=$TargetProcessId
 title=$qdmResourceProcess.MainWindowTitle
 clientWidthPixels=($qdmResourceClientRect.right - $qdmResourceClientRect.left)
 clientHeightPixels=($qdmResourceClientRect.bottom - $qdmResourceClientRect.top)
 dpi=[QdmResourceWindow]::GetDpiForWindow($qdmResourceHandle)
 hideRequested=[bool]$HideWindow
 visibleBefore=[QdmResourceWindow]::IsWindowVisible($qdmResourceHandle)
 hiddenConfirmed=$false
 hiddenAtUnixMs=$null
 restoredAtUnixMs=$null
 visibleAfter=$null
}
try{
 if($HideWindow){
  [void][QdmResourceWindow]::ShowWindow($qdmResourceHandle,0)
  $qdmWindowObservation.hiddenConfirmed=-not [QdmResourceWindow]::IsWindowVisible($qdmResourceHandle)
  if(-not $qdmWindowObservation.hiddenConfirmed){throw 'Owned Slint window did not become hidden.'}
  $qdmWindowObservation.hiddenAtUnixMs=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
 }
 & (Join-Path $PSScriptRoot 'measure-resources.ps1') -RootProcessId $TargetProcessId -OutputPath $OutputPath -Label $Label -WarmupSeconds 3 -DurationSeconds 8 -IntervalMilliseconds 500 | Out-Null
}finally{
 if($HideWindow){
  $qdmWindowObservation.restoredAtUnixMs=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  [void][QdmResourceWindow]::ShowWindow($qdmResourceHandle,5)
 }
 $qdmWindowObservation.visibleAfter=[QdmResourceWindow]::IsWindowVisible($qdmResourceHandle)
}
$qdmResourceReport=Get-Content -LiteralPath $OutputPath -Raw | ConvertFrom-Json
$qdmResourceReport | Add-Member -NotePropertyName windowObservation -NotePropertyValue ([pscustomobject]$qdmWindowObservation) -Force
$qdmResourceReport | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath $OutputPath -Encoding utf8
