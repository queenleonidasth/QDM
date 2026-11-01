param([Parameter(Mandatory=$true)][string]$ConfigPath)
$ErrorActionPreference='Stop'
[Console]::OutputEncoding=New-Object Text.UTF8Encoding($false)
$qdmConfig=Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
$qdmResult=[ordered]@{schema=1;status='running';startedAt=[DateTime]::UtcNow.ToString('o');processId=$qdmConfig.processId;windowsBefore=@();windowsAfter=@();closedDialogs=@();registrationPrompt=$null}
$qdmExitCode=1
try {
  $qdmExpectedPath=[IO.Path]::GetFullPath([string]$qdmConfig.binaryPath)
  $qdmExpectedHash=([string]$qdmConfig.binarySha256).ToLowerInvariant()
  if($qdmExpectedHash -cnotmatch '^[a-f0-9]{64}$'){throw 'IDM preflight needs the benchmark executable SHA-256.'}
  $qdmProcess=Get-Process -Id ([int]$qdmConfig.processId) -ErrorAction Stop
  if($qdmProcess.ProcessName -ine 'IDMan' -or ![string]::Equals([IO.Path]::GetFullPath($qdmProcess.Path),$qdmExpectedPath,[StringComparison]::OrdinalIgnoreCase)){throw 'IDM PID/executable identity does not match the benchmark.'}
  if((Get-FileHash -LiteralPath $qdmExpectedPath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $qdmExpectedHash){throw 'IDM executable SHA-256 changed before preflight.'}
  if($qdmConfig.processStartTime){
    # ConvertFrom-Json versions may return an ISO timestamp as either text or
    # DateTime. Compare UTC ticks so the identity check is locale independent.
    $qdmExpectedStart=if($qdmConfig.processStartTime -is [DateTime]){$qdmConfig.processStartTime}else{[DateTime]::Parse([string]$qdmConfig.processStartTime,[Globalization.CultureInfo]::InvariantCulture,[Globalization.DateTimeStyles]::RoundtripKind)}
    if($qdmProcess.StartTime.ToUniversalTime().Ticks -ne $qdmExpectedStart.ToUniversalTime().Ticks){throw 'The IDM process was replaced before preflight.'}
  }
  $qdmResult.binarySha256=$qdmExpectedHash
  $qdmResult.processStartTime=$qdmProcess.StartTime.ToUniversalTime().ToString('o')
  $qdmOwnedDirectory=[IO.Path]::GetFullPath((Join-Path ([string]$qdmConfig.workdir) 'idm-files'))+[IO.Path]::DirectorySeparatorChar
  $qdmAllowed=New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
  foreach($qdmRow in @($qdmConfig.passedRows)){
    if($null -eq $qdmRow -or $qdmRow.application -cne 'IDM' -or $qdmRow.status -cne 'passed'){continue}
    if(([string]$qdmRow.actualSha256) -cnotmatch '^[a-f0-9]{64}$' -or $qdmRow.actualSha256 -cne $qdmRow.expectedSha256){continue}
    $qdmPath=[IO.Path]::GetFullPath([string]$qdmRow.file)
    if(!$qdmPath.StartsWith($qdmOwnedDirectory,[StringComparison]::OrdinalIgnoreCase) -or [IO.Path]::GetFileName($qdmPath) -notlike 'QDM-IDM-BENCH-*'){throw 'A permitted completion path is outside this benchmark workspace.'}
    [void]$qdmAllowed.Add($qdmPath)
  }
  Add-Type -AssemblyName UIAutomationClient
  Add-Type -AssemblyName UIAutomationTypes
  Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Collections.Generic;
using System.Runtime.InteropServices;
public static class QdmIdmPreflightNative {
 public delegate bool EnumProc(IntPtr h,IntPtr p);
 public sealed class Item { public IntPtr Handle; public string Title; public string ClassName; public bool Visible; }
 [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f,IntPtr p);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll")] public static extern bool IsWindow(IntPtr h);
 [DllImport("user32.dll")] public static extern bool IsWindowVisible(IntPtr h);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetWindowTextW(IntPtr h,StringBuilder s,int n);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h,StringBuilder s,int n);
 public static bool Owned(IntPtr h,int pid){uint owner;GetWindowThreadProcessId(h,out owner);return IsWindow(h)&&owner==(uint)pid;}
 public static Item[] Read(int pid){
  var result=new List<Item>();
  EnumWindows(delegate(IntPtr h,IntPtr unused){
   uint owner;GetWindowThreadProcessId(h,out owner);if(owner!=(uint)pid)return true;
   var title=new StringBuilder(512);var type=new StringBuilder(256);GetWindowTextW(h,title,title.Capacity);GetClassNameW(h,type,type.Capacity);
   result.Add(new Item{Handle=h,Title=title.ToString(),ClassName=type.ToString(),Visible=IsWindowVisible(h)});
   return result.Count<64;
  },IntPtr.Zero);
  return result.ToArray();
 }
}
'@
  function Read-QdmIdmWindow($Native){
    $qdmWindow=[ordered]@{hwnd=('0x'+$Native.Handle.ToInt64().ToString('X'));title=$Native.Title;className=$Native.ClassName;visible=$Native.Visible;isModal=$null;registrationPrompt=$false;ownedCompletedFile=$null;action='observed'}
    $qdmChildren=@();$qdmMatches=New-Object 'System.Collections.Generic.HashSet[string]' ([StringComparer]::OrdinalIgnoreCase)
    if(!$Native.Visible){$qdmWindow.action='hidden-unchanged';return [pscustomobject]@{record=[pscustomobject]$qdmWindow;native=$Native;children=$qdmChildren}}
    try {
      $qdmElement=[Windows.Automation.AutomationElement]::FromHandle($Native.Handle)
      $qdmPattern=$null
      if($qdmElement.TryGetCurrentPattern([Windows.Automation.WindowPattern]::Pattern,[ref]$qdmPattern)){$qdmWindow.isModal=$qdmPattern.Current.IsModal}
      $qdmDialog=($Native.ClassName -eq '#32770' -or $Native.Title -match '(?i)download complete|register|registration' -or $qdmWindow.isModal -eq $true)
      if($qdmDialog){
        $qdmFound=$qdmElement.FindAll([Windows.Automation.TreeScope]::Descendants,[Windows.Automation.Condition]::TrueCondition)
        $qdmNames=New-Object 'System.Collections.Generic.List[string]'
        $qdmNames.Add([string]$Native.Title)
        for($qdmIndex=0;$qdmIndex -lt [Math]::Min(300,$qdmFound.Count);$qdmIndex++){
          $qdmChild=$qdmFound.Item($qdmIndex);$qdmChildren+=,$qdmChild
          try {
            $qdmNames.Add([string]$qdmChild.Current.Name)
            # Only a known completion window may supply a saved-file value.
            if($Native.Title -ieq 'Download complete'){
              $qdmValue=$null
              if($qdmChild.TryGetCurrentPattern([Windows.Automation.ValuePattern]::Pattern,[ref]$qdmValue)){
                $qdmRaw=[string]$qdmValue.Current.Value
                if([IO.Path]::IsPathRooted($qdmRaw)){
                  try {$qdmFull=[IO.Path]::GetFullPath($qdmRaw);if($qdmAllowed.Contains($qdmFull)){[void]$qdmMatches.Add($qdmFull)}}catch{}
                }
              }
            }
          }catch{}
        }
        $qdmText=$qdmNames -join "`n"
        $qdmTrial=($qdmText -match '(?i)\b\d+\s+days?\s+(left|remaining)\b|\btrial\b|\bevaluation\b')
        $qdmRegister=($qdmText -match '(?i)\bregister\b|\bregistration\b')
        $qdmWindow.registrationPrompt=($qdmRegister -and ($qdmTrial -or $Native.Title -match '(?i)register|registration'))
        if($qdmMatches.Count -eq 1){$qdmWindow.ownedCompletedFile=@($qdmMatches)[0]}
      }
    }catch{$qdmWindow.action='unrecognized-left-unchanged'}
    return [pscustomobject]@{record=[pscustomobject]$qdmWindow;native=$Native;children=$qdmChildren}
  }
  function Read-QdmIdmWindows {
    return @([QdmIdmPreflightNative]::Read([int]$qdmConfig.processId) | ForEach-Object {Read-QdmIdmWindow $_})
  }
  $qdmWindows=Read-QdmIdmWindows
  $qdmResult.windowsBefore=@($qdmWindows | ForEach-Object {$_.record})
  $qdmBlocker=@($qdmWindows | Where-Object {$_.record.registrationPrompt})
  if($qdmBlocker.Count){
    $qdmResult.registrationPrompt=$qdmBlocker[0].record.hwnd
    throw ('IDM registration/trial reminder is open at '+$qdmResult.registrationPrompt+'. Preflight stopped before timing; the prompt was left untouched.')
  }
  foreach($qdmWindow in $qdmWindows){
    if($qdmWindow.record.title -ine 'Download complete' -or !$qdmWindow.record.ownedCompletedFile){continue}
    if(![QdmIdmPreflightNative]::Owned($qdmWindow.native.Handle,[int]$qdmConfig.processId)){continue}
    # Re-read the exact file and close control immediately before invocation.
    $qdmCurrentNative=@([QdmIdmPreflightNative]::Read([int]$qdmConfig.processId) | Where-Object {$_.Handle -eq $qdmWindow.native.Handle})
    if($qdmCurrentNative.Count -ne 1 -or $qdmCurrentNative[0].Title -ine 'Download complete'){continue}
    $qdmFresh=Read-QdmIdmWindow $qdmCurrentNative[0]
    if($qdmFresh.record.ownedCompletedFile -cne $qdmWindow.record.ownedCompletedFile){continue}
    $qdmButtons=@($qdmFresh.children | Where-Object {$_.Current.AutomationId -ceq '1079' -and $_.Current.ProcessId -eq [int]$qdmConfig.processId -and $_.Current.ControlType -eq [Windows.Automation.ControlType]::Button})
    if($qdmButtons.Count -ne 1 -or !$qdmButtons[0].Current.IsEnabled){$qdmWindow.record.action='owned-completion-close-unavailable';continue}
    $qdmInvoke=$null
    if(!$qdmButtons[0].TryGetCurrentPattern([Windows.Automation.InvokePattern]::Pattern,[ref]$qdmInvoke)){$qdmWindow.record.action='owned-completion-close-unavailable';continue}
    $qdmInvokeFailure=$null
    try {$qdmInvoke.Invoke()}catch{$qdmInvokeFailure=$_}
    for($qdmWait=0;$qdmWait -lt 10 -and [QdmIdmPreflightNative]::Owned($qdmWindow.native.Handle,[int]$qdmConfig.processId);$qdmWait++){Start-Sleep -Milliseconds 50}
    $qdmClosed=![QdmIdmPreflightNative]::Owned($qdmWindow.native.Handle,[int]$qdmConfig.processId)
    $qdmWindow.record.action=$(if($qdmClosed){'closed-owned-completion'}else{'close-invoked-still-visible'})
    $qdmResult.closedDialogs+=,[pscustomobject]@{hwnd=$qdmWindow.record.hwnd;file=$qdmWindow.record.ownedCompletedFile;automationId='1079';closed=$qdmClosed;invokeThrew=($null -ne $qdmInvokeFailure)}
    # Some providers throw after destroying their own window. Verify once and
    # never replay an invocation whose outcome may already have succeeded.
    if($qdmInvokeFailure -and !$qdmClosed){throw ('IDM completion close could not be confirmed for '+$qdmWindow.record.hwnd+': '+$qdmInvokeFailure.Exception.Message)}
  }
  $qdmAfter=Read-QdmIdmWindows
  $qdmResult.windowsAfter=@($qdmAfter | ForEach-Object {$_.record})
  $qdmBlocker=@($qdmAfter | Where-Object {$_.record.registrationPrompt})
  if($qdmBlocker.Count){$qdmResult.registrationPrompt=$qdmBlocker[0].record.hwnd;throw ('IDM registration/trial reminder is open at '+$qdmResult.registrationPrompt+'. The prompt was left untouched.')}
  $qdmResult.status='passed';$qdmExitCode=0
}catch{
  $qdmResult.status=$(if($qdmResult.registrationPrompt){'blocked'}else{'failed'})
  $qdmResult.error=$_.Exception.Message
}finally{
  $qdmResult.finishedAt=[DateTime]::UtcNow.ToString('o')
  $qdmJson=$qdmResult | ConvertTo-Json -Depth 6
  $qdmJson | Set-Content -LiteralPath ([string]$qdmConfig.reportPath) -Encoding UTF8
  [Console]::Out.WriteLine($qdmJson)
}
exit $qdmExitCode
