param(
 [Parameter(Mandatory=$true)][string]$Binary,
 [string]$NodeBinary='C:\Program Files\nodejs\node.exe',
 [Parameter(Mandatory=$true)][string]$OutputDirectory,
 [string]$UrlLabel='Download URL',
 [string]$AddLabel='Download',
 [string]$SettingsLabel='Settings',
 [string]$ConnectionsLabel='Connections',
 [string]$SaveLabel='Save settings',
 [string]$PauseLabel='Pause',
 [string]$ResumeLabel='Resume',
 [string]$ExitLabel='Exit QDM',
 [string]$RemoveLabel='Remove',
 [switch]$KeepWorkspace
)
$ErrorActionPreference='Stop'
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Drawing
Add-Type -TypeDefinition @'
using System;
using System.Drawing;
using System.Drawing.Imaging;
using System.Runtime.InteropServices;
public static class QdmSlintQaNative {
 [StructLayout(LayoutKind.Sequential)] public struct Rect{public int Left,Top,Right,Bottom;}
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out Rect r);
 [DllImport("user32.dll")] public static extern bool PrintWindow(IntPtr h,IntPtr dc,uint flags);
 [DllImport("user32.dll")] public static extern bool MoveWindow(IntPtr h,int x,int y,int w,int height,bool repaint);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll")] public static extern bool SetForegroundWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern IntPtr GetForegroundWindow();
 [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h,uint m,IntPtr w,IntPtr l);
 [DllImport("user32.dll")] public static extern uint MapVirtualKeyW(uint code,uint mode);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr c);
 public static bool Capture(IntPtr h,string path){
  Rect r;if(!GetWindowRect(h,out r))return false;
  using(var b=new Bitmap(Math.Max(1,r.Right-r.Left),Math.Max(1,r.Bottom-r.Top))){using(var g=Graphics.FromImage(b)){var dc=g.GetHdc();bool ok=PrintWindow(h,dc,2);g.ReleaseHdc(dc);if(!ok)return false;}b.Save(path,ImageFormat.Png);}return true;
 }
}
'@ -ReferencedAssemblies System.Drawing
[void][QdmSlintQaNative]::SetThreadDpiAwarenessContext([IntPtr](-4))
$Binary=[IO.Path]::GetFullPath($Binary)
$OutputDirectory=[IO.Path]::GetFullPath($OutputDirectory)
New-Item -ItemType Directory -Path $OutputDirectory -Force | Out-Null
$qdmWorkspace=Join-Path $env:TEMP ('qdm-slint-ui-'+[Guid]::NewGuid().ToString('N'))
$qdmData=Join-Path $qdmWorkspace 'state'
$qdmDownloads=Join-Path $qdmWorkspace 'downloads'
New-Item -ItemType Directory -Path $qdmDownloads -Force | Out-Null
$qdmStatePath=Join-Path $qdmData 'state-v3.sqlite3'
$qdmSteps=[Collections.Generic.List[object]]::new()
$qdmOwned=[Collections.Generic.List[Diagnostics.Process]]::new()
$qdmApp=$null;$qdmFixture=$null;$qdmWindow=[IntPtr]::Zero
$qdmReport=[ordered]@{schema=1;suite='Real Slint Windows UI acceptance';binary=$Binary;binarySha256=(Get-FileHash -LiteralPath $Binary).Hash.ToLowerInvariant();startedAt=[DateTime]::UtcNow.ToString('o');isolatedWorkspace=$qdmWorkspace;steps=$qdmSteps}
function Wait-Qdm([scriptblock]$Read,[string]$Description,[int]$TimeoutMs=20000){
 $timer=[Diagnostics.Stopwatch]::StartNew()
 while($timer.ElapsedMilliseconds -lt $TimeoutMs){
  $value=& $Read
  if($value){return $value}
  if($qdmApp -and $qdmApp.HasExited){throw ('GUI exited while waiting for '+$Description+'; '+$qdmApp.StandardError.ReadToEnd())}
  Start-Sleep -Milliseconds 80
 }
 throw ('Timeout waiting for '+$Description)
}
function Assert-Qdm([bool]$Value,[string]$Message){if(!$Value){throw $Message}}
function Pass-Qdm([string]$Name,$Detail=$null){$qdmSteps.Add([pscustomobject]@{name=$Name;status='passed';at=[DateTime]::UtcNow.ToString('o');detail=$Detail});Write-Output ('PASS '+$Name)}
function Start-QdmOwned([string]$File,[string[]]$QdmArguments,[bool]$PipeInput=$false){
 $p=[Diagnostics.Process]::new();$p.StartInfo.FileName=$File
 foreach($a in $QdmArguments){if($a.Contains('"')){throw 'Invalid quote in test process argument.'}}
 $p.StartInfo.Arguments=($QdmArguments|ForEach-Object {'"'+$_+'"'}) -join ' '
 $p.StartInfo.UseShellExecute=$false;$p.StartInfo.RedirectStandardInput=$PipeInput
 $p.StartInfo.RedirectStandardOutput=$true;$p.StartInfo.RedirectStandardError=$true;$p.StartInfo.CreateNoWindow=$true
 $p.StartInfo.EnvironmentVariables['QDM_TEST_MODE']='1'
 $p.StartInfo.EnvironmentVariables['QDM_TEST_ASSEMBLY_DELAY_MS']='30'
 [void]$p.Start();$qdmOwned.Add($p);return $p
}
function Start-QdmGui{
 $script:qdmApp=Start-QdmOwned $Binary @('--data-dir',$qdmData,'--download-dir',$qdmDownloads)
 $script:qdmWindow=Wait-Qdm {$qdmApp.Refresh();if($qdmApp.MainWindowHandle -ne [IntPtr]::Zero -and $qdmApp.MainWindowTitle -like 'QDM*QUEEN Download Manager*'){$qdmApp.MainWindowHandle}} 'Slint product window'
 Assert-Qdm ($qdmApp.MainWindowTitle -like 'QDM*QUEEN Download Manager*') 'Product window title is absent.'
}
function Qdm-Root{return [Windows.Automation.AutomationElement]::FromHandle($qdmWindow)}
function Find-Qdm([string]$Name,[string]$Type=''){
 $root=Qdm-Root
 $condition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Name)
 $items=$root.FindAll([Windows.Automation.TreeScope]::Descendants,$condition)
 foreach($item in $items){try{if(($Type -eq '' -or $item.Current.ControlType.ProgrammaticName -eq ('ControlType.'+$Type)) -and !$item.Current.IsOffscreen){return $item}}catch{}}
 return $null
}
function Button-Qdm([string]$Name){return Wait-Qdm {Find-Qdm $Name 'Button'} ('button '+$Name) 6000}
function Click-Qdm([string]$Name){
 $item=Wait-Qdm {$e=Find-Qdm $Name 'Button';if($e -and $e.Current.IsEnabled){$e}} ('enabled button '+$Name) 6000
 $pattern=$null
 if(!$item.TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$pattern)){throw ('No accessible invoke action: '+$Name)}
 $pattern.Invoke()
}
function Set-Qdm([string]$Name,[string]$Value){
 [void](Wait-Qdm {Find-Qdm $Name} ('field '+$Name) 6000)
 $condition=[Windows.Automation.PropertyCondition]::new([Windows.Automation.AutomationElement]::NameProperty,$Name)
 $items=(Qdm-Root).FindAll([Windows.Automation.TreeScope]::Descendants,$condition)
 foreach($item in $items){
  if($item.Current.IsOffscreen){continue}
  $pattern=$null
  if($item.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern,[ref]$pattern)){$pattern.SetValue($Value);return}
  if($item.TryGetCurrentPattern([Windows.Automation.RangeValuePattern]::Pattern,[ref]$pattern)){$pattern.SetValue([double]$Value);return}
 }
 throw ('No editable accessibility pattern: '+$Name)
}
function Escape-Qdm {
 $scan=[QdmSlintQaNative]::MapVirtualKeyW(27,0)
 $down=[long]1 -bor ([long]$scan -shl 16)
 $up=$down -bor [long]3221225472
 Assert-Qdm ([QdmSlintQaNative]::PostMessageW($qdmWindow,0x100,[IntPtr]27,[IntPtr]$down)) 'Could not post Escape to the owned window.'
 [void][QdmSlintQaNative]::PostMessageW($qdmWindow,0x101,[IntPtr]27,[IntPtr]$up)
}
function Toggle-GoldCheckBox([string]$Name){
  $item=Wait-Qdm {Find-Qdm $Name 'CheckBox'} ('checkbox '+$Name) 6000
  $pattern=$null
  if($item.TryGetCurrentPattern([Windows.Automation.TogglePattern]::Pattern,[ref]$pattern)){$pattern.Toggle();return}
  $item.SetFocus()
  $scan=[QdmSlintQaNative]::MapVirtualKeyW(0x20,0)
  $down=[long]1 -bor ([long]$scan -shl 16)
  $up=$down -bor [long]3221225472
  Assert-Qdm ([QdmSlintQaNative]::PostMessageW($qdmWindow,0x100,[IntPtr]0x20,[IntPtr]$down)) ('Could not toggle '+$Name)
  [void][QdmSlintQaNative]::PostMessageW($qdmWindow,0x101,[IntPtr]0x20,[IntPtr]$up)
}
function State-Qdm{
  if(Test-Path -LiteralPath $qdmStatePath){
   $reader=Join-Path $PSScriptRoot 'helpers/sqlite-state.cjs'
   try{
    $json=& $NodeBinary -e 'const {openState}=require(process.argv[1]);process.stdout.write(JSON.stringify(openState(process.argv[2])))' $reader $qdmData 2>$null
    if($LASTEXITCODE -eq 0){return $json|ConvertFrom-Json}
   }catch{}
  }
  return $null
}
function Task-Qdm{
 $s=State-Qdm
 if($s -and @($s.tasks).Count){return $s.tasks[0].view}
 return $null
}
function Capture-Qdm([string]$Name){
 $target=Join-Path $OutputDirectory ($Name+'.png')
 if([QdmSlintQaNative]::Capture($qdmWindow,$target)){return $target}
 return $null
}
function Ui-Inventory{
 $all=(Qdm-Root).FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
 $list=@()
 foreach($e in $all){try{if(!$e.Current.IsOffscreen -and $e.Current.Name){$list+=[pscustomobject]@{name=$e.Current.Name;type=$e.Current.ControlType.ProgrammaticName;enabled=$e.Current.IsEnabled}}}catch{}}
 return $list
}
function Exit-QdmGui{
  $exitButton=Find-Qdm $ExitLabel 'Button'
  if($exitButton){Click-Qdm $ExitLabel}
  else{
   $current=State-Qdm
   if($current.settings.close_to_tray){
    Click-Qdm $SettingsLabel
    Toggle-GoldCheckBox 'Close to tray'
    Click-Qdm $SaveLabel
    [void](Wait-Qdm {$s=State-Qdm;if(!$s.settings.close_to_tray){$s}} 'disable close-to-tray for owned GUI shutdown')
    Start-Sleep -Milliseconds 500
   }
   Assert-Qdm ([QdmSlintQaNative]::PostMessageW($qdmWindow,0x10,[IntPtr]::Zero,[IntPtr]::Zero)) 'Could not close the owned QDM window.'
  }
  if(!$qdmApp.WaitForExit(15000)){throw 'Exit action did not close the owned GUI.'}
 Assert-Qdm ($qdmApp.ExitCode -eq 0) ('GUI exit code '+$qdmApp.ExitCode)
 $script:qdmApp=$null;$script:qdmWindow=[IntPtr]::Zero
}
try{
 $qdmFixtureInfo=Join-Path $OutputDirectory 'fixture-info.json'
 if(Test-Path -LiteralPath $qdmFixtureInfo){Remove-Item -LiteralPath $qdmFixtureInfo}
 $qdmFixture=Start-QdmOwned $NodeBinary @((Join-Path $PSScriptRoot 'serve-fixture.cjs'),'--size-mib','64','--delay-ms','100','--chunk-kib','16','--info',$qdmFixtureInfo,'--report',(Join-Path $OutputDirectory 'fixture-origin.json')) $true
 $qdmOrigin=Wait-Qdm {if(Test-Path -LiteralPath $qdmFixtureInfo){try{Get-Content -LiteralPath $qdmFixtureInfo -Raw -Encoding UTF8|ConvertFrom-Json}catch{}}} 'controlled origin'
 Start-QdmGui
 [void](Button-Qdm $AddLabel);[void](Button-Qdm $SettingsLabel)
 $qdmReport.initialControls=Ui-Inventory
 Pass-Qdm 'Real Slint window exposes usable accessible controls' @{processId=$qdmApp.Id;windowTitle=$qdmApp.MainWindowTitle;screenshot=(Capture-Qdm '01-main')}
 [void][QdmSlintQaNative]::MoveWindow($qdmWindow,40,40,480,850,$true)
 Start-Sleep -Milliseconds 250
 [void](Button-Qdm $SettingsLabel);[void](Button-Qdm $AddLabel)
 Pass-Qdm 'Resize keeps primary controls reachable' @{screenshot=(Capture-Qdm '02-resized')}
 [void][QdmSlintQaNative]::MoveWindow($qdmWindow,40,40,580,1000,$true)
 Click-Qdm $SettingsLabel
 Set-Qdm $ConnectionsLabel '17'
 $qdmEditing=Wait-Qdm {$e=Find-Qdm $ConnectionsLabel 'Edit';if($e -and $e.Current.IsEnabled){$e}} 'editable connection preference'
 $qdmEditing.SetFocus()
 Assert-Qdm (!(Find-Qdm $AddLabel 'Button').Current.IsEnabled) 'A modal sheet left the background Download button enabled.'
 Assert-Qdm (!(Find-Qdm $UrlLabel).Current.IsEnabled) 'A modal sheet left the background URL field enabled.'
 Escape-Qdm
 [void](Wait-Qdm {$a=Find-Qdm $AddLabel 'Button';if($a -and $a.Current.IsEnabled -and !(Find-Qdm 'Close settings' 'Button')){$true}} 'Escape closes an edited Settings sheet')
 Pass-Qdm 'Modal keyboard handling closes from an editable field and disables background controls'
  Click-Qdm $SettingsLabel
  Set-Qdm $ConnectionsLabel '64'
  Set-Qdm 'Queue speed limits' 'Default=0'
  Toggle-GoldCheckBox 'Reduced motion'
  Click-Qdm $SaveLabel
  [void](Wait-Qdm {$s=State-Qdm;if($s.settings.connections -eq 64 -and $s.settings.queue_speed_limits.Default -eq 0){$s}} 'saved connection and queue preferences')
  Pass-Qdm 'Settings save worker ceiling and Default queue budget'
 [void](Wait-Qdm {$p=Join-Path $qdmData 'ui-settings.json';if(Test-Path $p){$a=Get-Content $p -Raw -Encoding UTF8|ConvertFrom-Json;if($a.reduced_motion -eq $true){$true}}} 'saved reduced motion preference')
 Pass-Qdm 'Reduced motion checkbox saves the appearance preference'
 Set-Qdm $UrlLabel $qdmOrigin.url
 Click-Qdm $AddLabel
 $qdmProgress=Wait-Qdm {$t=Task-Qdm;if($t -and $t.status -eq 'downloading' -and $t.downloaded_bytes -gt 524288){$t}} 'actual UI download progress' 30000
  Assert-Qdm ($qdmProgress.connections_limit -eq 64) 'The submitted UI download ignored the saved ceiling.'
  $qdmReport.downloadingControls=Ui-Inventory
  Pass-Qdm 'URL submit drives the actual download engine and main-screen lanes' @{downloadedBytes=$qdmProgress.downloaded_bytes;active=$qdmProgress.connections_active;laneCount=@($qdmProgress.lanes).Count;screenshot=(Capture-Qdm '03-downloading')}
  Click-Qdm 'Details'
  Set-Qdm 'Queue' 'ui-acceptance'
  Set-Qdm 'Per-download limit KiB/s' '0'
  Click-Qdm 'Save limits'
  [void](Wait-Qdm {$t=Task-Qdm;if($t.queue -eq 'ui-acceptance' -and $t.speed_limit_kib -eq 0){$t}} 'saved active download limits')
  Click-Qdm 'Close connection details'
  Pass-Qdm 'Details sheet updates an active task queue and per-download budget'
  Click-Qdm $PauseLabel
 $qdmPaused=Wait-Qdm {$t=Task-Qdm;if($t.status -eq 'paused'){$t}} 'UI pause checkpoint'
 Assert-Qdm ($qdmPaused.downloaded_bytes -gt 0) 'Pause lost downloaded progress.'
 Pass-Qdm 'Pause retains received bytes' @{bytes=$qdmPaused.downloaded_bytes;screenshot=(Capture-Qdm '04-paused')}
 Click-Qdm $ResumeLabel
 $qdmLive=Wait-Qdm {$t=Task-Qdm;if($t.status -eq 'downloading' -and $t.downloaded_bytes -gt $qdmPaused.downloaded_bytes){$t}} 'UI resume advances saved work'
 Pass-Qdm 'Resume advances the same task' @{resumedBytes=$qdmLive.resumed_bytes;downloadedBytes=$qdmLive.downloaded_bytes}
 $qdmAssembly=$null
 $qdmCompleted=Wait-Qdm {
  $t=Task-Qdm
  if($t.status -in @('assembling','verifying') -and !$script:qdmAssembly){$script:qdmAssembly=@{status=$t.status;bytes=$t.assembly_bytes;total=$t.assembly_total_bytes;screenshot=(Capture-Qdm '05-assembling');controls=(Ui-Inventory)}}
  if($t.status -eq 'error'){throw ('Download error: '+$t.error)}
  if($t.status -eq 'completed'){$t}
 } 'download completion' 90000
 $qdmHash=(Get-FileHash -LiteralPath $qdmCompleted.file_path -Algorithm SHA256).Hash.ToLowerInvariant()
 Assert-Qdm ($qdmHash -eq $qdmOrigin.sha256) 'Downloaded file hash differs from independently generated source.'
 Assert-Qdm ((Get-Item -LiteralPath $qdmCompleted.file_path).Length -eq $qdmOrigin.sizeBytes) 'Completed byte length is incorrect.'
 Pass-Qdm 'Completed output matches independent SHA-256' @{sha256=$qdmHash;bytes=$qdmOrigin.sizeBytes;screenshot=(Capture-Qdm '06-completed')}
 $qdmReport.assemblyObservation=$qdmAssembly
 Assert-Qdm ($null -ne $qdmAssembly) 'The UI test did not observe the combining/verifying stage.'
 Pass-Qdm 'Combining stage remains visible during final file work' $qdmAssembly
 $qdmCurrentPid=$qdmApp.Id
 [void][QdmSlintQaNative]::PostMessageW($qdmWindow,0x10,[IntPtr]::Zero,[IntPtr]::Zero)
 [void](Wait-Qdm {![QdmSlintQaNative]::IsWindowVisible($qdmWindow)} 'close to tray')
 Assert-Qdm (!$qdmApp.HasExited) 'Close-to-tray exited the program.'
 $qdmSecond=Start-QdmOwned $Binary @('--data-dir',$qdmData,'--download-dir',$qdmDownloads)
 Assert-Qdm ($qdmSecond.WaitForExit(10000)) 'Second instance did not return.'
 [void](Wait-Qdm {[QdmSlintQaNative]::IsWindowVisible($qdmWindow)} 'single instance restores window')
 Assert-Qdm ($qdmApp.Id -eq $qdmCurrentPid) 'Restoration replaced the running process.'
 Pass-Qdm 'Close to tray and second-launch restoration preserve the process'
 Exit-QdmGui
  Start-QdmGui
  [void](Wait-Qdm {$t=Task-Qdm;if($t.status -eq 'completed'){$t}} 'restart history')
  Assert-Qdm ((State-Qdm).settings.connections -eq 64) '64 connection preference did not survive restart.'
  Assert-Qdm ((State-Qdm).settings.queue_speed_limits.Default -eq 0) 'Queue speed limits did not survive restart.'
 Click-Qdm $SettingsLabel
  $savedAppearance=Get-Content -LiteralPath (Join-Path $qdmData 'ui-settings.json') -Raw -Encoding UTF8|ConvertFrom-Json
  Assert-Qdm ($savedAppearance.reduced_motion -eq $true) 'Reduced motion did not survive restart.'
 Click-Qdm 'Close settings'
 Pass-Qdm 'Clean restart preserves history and preferences' @{screenshot=(Capture-Qdm '07-restarted')}
 # The redesigned compact queue keeps destructive actions in the Details sheet.
 Click-Qdm 'Details'
 Click-Qdm $RemoveLabel
 Click-Qdm 'Remove download'
 [void](Wait-Qdm {$s=State-Qdm;if($s -and @($s.tasks).Count -eq 0){$true}} 'history removal')
 Assert-Qdm (Test-Path -LiteralPath $qdmCompleted.file_path) 'Removing history deleted the completed file.'
 Assert-Qdm ((Get-FileHash -LiteralPath $qdmCompleted.file_path).Hash.ToLowerInvariant() -eq $qdmHash) 'Removing history changed the completed file.'
 Pass-Qdm 'Removing a completed history item preserves its downloaded file'
 Exit-QdmGui
 $qdmReport.status='passed'
}catch{
 $qdmReport.status='failed';$qdmReport.error=$_.Exception.Message
 $qdmSteps.Add([pscustomobject]@{name='Unfinished UI acceptance';status='failed';error=$_.Exception.Message;at=[DateTime]::UtcNow.ToString('o')})
 try{$qdmReport.failureControls=Ui-Inventory;$qdmReport.failureScreenshot=Capture-Qdm 'failure'}catch{}
 Write-Output ('FAIL '+$qdmReport.error)
}finally{
 if($qdmFixture -and !$qdmFixture.HasExited){try{$qdmFixture.StandardInput.WriteLine('{"op":"shutdown"}');[void]$qdmFixture.WaitForExit(5000)}catch{}}
 foreach($qdmProcess in $qdmOwned){if(!$qdmProcess.HasExited){try{$qdmProcess.Kill();$qdmProcess.WaitForExit()}catch{}}}
 $qdmReport.passed=@($qdmSteps|Where-Object {$_.status -eq 'passed'}).Count
 $qdmReport.failed=@($qdmSteps|Where-Object {$_.status -eq 'failed'}).Count
 $qdmReport.finishedAt=[DateTime]::UtcNow.ToString('o')
 $qdmReport|ConvertTo-Json -Depth 14|Set-Content -LiteralPath (Join-Path $OutputDirectory 'slint-ui-acceptance.json') -Encoding UTF8
 if(!$KeepWorkspace -and $qdmReport.status -eq 'passed'){Remove-Item -LiteralPath $qdmWorkspace -Recurse -Force}
}
if($qdmReport.status -ne 'passed'){exit 1}
