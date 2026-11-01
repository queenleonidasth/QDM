param([string]$EvidenceDirectory='docs\evidence\slint-idm-20261027')
$ErrorActionPreference='Stop'

function Get-QdmOutputFingerprint {
  param([AllowNull()][AllowEmptyString()][string]$Path)
  $qdmFingerprint=[ordered]@{path=$Path;present=$false;length=$null;sha256=$null;observedAt=[DateTime]::UtcNow.ToString('o');missingReason=$null}
  if([string]::IsNullOrWhiteSpace($Path)){
    $qdmFingerprint.missingReason='History does not contain an output path.'
    return [pscustomobject]$qdmFingerprint
  }
  $qdmStream=$null;$qdmHasher=$null
  try {
    # Read sharing denies concurrent modification/deletion while length and
    # hash are captured. Access failures must not be classified as absence.
    $qdmStream=[IO.File]::Open($Path,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::Read)
    $qdmHasher=[Security.Cryptography.SHA256]::Create()
    $qdmFingerprint.length=[long]$qdmStream.Length
    $qdmFingerprint.sha256=[BitConverter]::ToString($qdmHasher.ComputeHash($qdmStream)).Replace('-','').ToLowerInvariant()
    $qdmFingerprint.present=$true
  } catch {
    $qdmCause=$_.Exception.GetBaseException()
    if($qdmCause -is [IO.FileNotFoundException] -or $qdmCause -is [IO.DirectoryNotFoundException]){
      $qdmFingerprint.missingReason='Output file was not found at the recorded path.'
    } else {throw}
  } finally {
    if($null -ne $qdmHasher){$qdmHasher.Dispose()}
    if($null -ne $qdmStream){$qdmStream.Dispose()}
  }
  return [pscustomobject]$qdmFingerprint
}

function Compare-QdmCompletedOutputs {
  param([object[]]$Before)
  foreach($qdmOutput in $Before){
    $qdmAfterOutput=$null;$qdmReadError=$null
    try {$qdmAfterOutput=Get-QdmOutputFingerprint -Path $qdmOutput.path}
    catch {$qdmReadError=$_.Exception.Message}
    if($qdmReadError){$qdmOutputStatus='unreadable_after_install'}
    elseif($qdmOutput.before.present){
      if(!$qdmAfterOutput.present){$qdmOutputStatus='missing_after_install'}
      elseif($qdmAfterOutput.length -ne $qdmOutput.before.length -or $qdmAfterOutput.sha256 -cne $qdmOutput.before.sha256){$qdmOutputStatus='changed_after_install'}
      else {$qdmOutputStatus='verified_unchanged'}
    } elseif($qdmAfterOutput.present){$qdmOutputStatus='preexisting_missing_now_present'}
    else {$qdmOutputStatus='preexisting_missing'}
    [pscustomobject]@{
      taskId=$qdmOutput.taskId;filename=$qdmOutput.filename;path=$qdmOutput.path
      before=$qdmOutput.before;after=$qdmAfterOutput;status=$qdmOutputStatus;error=$qdmReadError
    }
  }
}

$qdmRoot=Split-Path -Parent $PSScriptRoot
$qdmEvidence=Join-Path $qdmRoot $EvidenceDirectory
$qdmCandidate=Join-Path $qdmRoot 'target\release\QDM.exe'
$qdmExpected=(Get-FileHash -LiteralPath $qdmCandidate -Algorithm SHA256).Hash.ToLowerInvariant()
$qdmPackage=Get-Content -LiteralPath (Join-Path $qdmEvidence 'packaging.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if($qdmPackage.status -ne 'passed' -or $qdmPackage.binarySha256 -ne $qdmExpected){throw 'The candidate has not passed release packaging.'}
if($qdmPackage.nsisInputCharset -cne 'UTF8'){throw 'The installer package must record explicit UTF-8 NSIS source decoding.'}
. (Join-Path $PSScriptRoot 'release-gates.ps1')
$qdmAcceptedGates=@(Assert-QdmReleaseGates -EvidenceDirectory $qdmEvidence -BinarySha256 $qdmExpected)
foreach($qdmGate in $qdmAcceptedGates){
  $qdmPackagedGate=@($qdmPackage.releaseGates | Where-Object {$_.report -ceq $qdmGate.report})
  if($qdmPackagedGate.Count -ne 1 -or $qdmPackagedGate[0].reportSha256 -cne $qdmGate.reportSha256){throw ('Release evidence changed after packaging: '+$qdmGate.report)}
}
$qdmInstall=Join-Path $env:LOCALAPPDATA 'Programs\QDM'
$qdmInstalledExe=Join-Path $qdmInstall 'QDM.exe'
$qdmStatePath=Join-Path $env:APPDATA 'QDM\state-v2.json'
$qdmBackup=Join-Path (Split-Path -Parent $qdmRoot) ('backups\before-install-slint-'+(Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Path $qdmBackup -Force | Out-Null
Add-Type -TypeDefinition @'
using System;
using System.Text;
using System.Runtime.InteropServices;
public static class QdmSlintInstallNative {
 public delegate bool EnumProc(IntPtr h,IntPtr p);
 [DllImport("user32.dll")] public static extern bool EnumWindows(EnumProc f,IntPtr p);
 [DllImport("user32.dll")] public static extern uint GetWindowThreadProcessId(IntPtr h,out uint pid);
 [DllImport("user32.dll",CharSet=CharSet.Unicode)] public static extern int GetClassNameW(IntPtr h,StringBuilder s,int n);
 [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr h,uint m,IntPtr w,IntPtr l);
 public static IntPtr FindOldMain(int pid){
  IntPtr found=IntPtr.Zero;
  EnumWindows(delegate(IntPtr h,IntPtr unused){uint owner;GetWindowThreadProcessId(h,out owner);if(owner!=(uint)pid)return true;var text=new StringBuilder(256);GetClassNameW(h,text,256);if(text.ToString()=="QDM.Native.Main.v2"){found=h;return false;}return true;},IntPtr.Zero);
  return found;
 }
}
'@
$qdmBefore=if(Test-Path -LiteralPath $qdmStatePath){Get-Content -LiteralPath $qdmStatePath -Raw -Encoding UTF8 | ConvertFrom-Json}else{$null}
$qdmActive=@($qdmBefore.tasks | Where-Object {$_.view.status -in @('downloading','probing','assembling','verifying')})
if($qdmActive.Count){throw 'The installed profile has active downloads; checkpoint them before applying this update.'}
$qdmApps=@(Get-Process -Name QDM -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq $qdmInstalledExe})
foreach($qdmApp in $qdmApps){
  $qdmOldHandle=[QdmSlintInstallNative]::FindOldMain($qdmApp.Id)
  if($qdmOldHandle -eq [IntPtr]::Zero){throw 'Cannot identify the installed Win32 main window for a graceful upgrade.'}
  if(![QdmSlintInstallNative]::PostMessageW($qdmOldHandle,0x111,[IntPtr]1106,[IntPtr]::Zero)){throw 'Graceful QDM exit could not be requested.'}
  if(!$qdmApp.WaitForExit(30000)){throw 'Installed QDM has not finished saving its state.'}
}
if(Test-Path -LiteralPath $qdmInstalledExe){Copy-Item -LiteralPath $qdmInstalledExe -Destination (Join-Path $qdmBackup 'QDM-previous.exe') -Force}
if(Test-Path -LiteralPath $qdmStatePath){Copy-Item -LiteralPath $qdmStatePath -Destination (Join-Path $qdmBackup 'state-v2.json') -Force}
$qdmSaved=if(Test-Path -LiteralPath $qdmStatePath){Get-Content -LiteralPath $qdmStatePath -Raw -Encoding UTF8 | ConvertFrom-Json}else{$null}
$qdmCompletedBefore=@(foreach($qdmTask in @($qdmSaved.tasks)){
  if($null -ne $qdmTask -and $qdmTask.view.status -eq 'completed'){
    [pscustomobject]@{
      taskId=$qdmTask.view.id;filename=$qdmTask.view.filename;path=$qdmTask.view.file_path
      before=(Get-QdmOutputFingerprint -Path $qdmTask.view.file_path)
    }
  }
})
$qdmPreexistingMissing=@($qdmCompletedBefore | Where-Object {!$_.before.present})
$qdmOutputBeforePath=Join-Path $qdmBackup 'completed-outputs-before.json'
[ordered]@{
  schema=1;phase='before_install';capturedAt=[DateTime]::UtcNow.ToString('o');sourceStatePath=$qdmStatePath
  completedRecords=$qdmCompletedBefore.Count;existingFiles=@($qdmCompletedBefore | Where-Object {$_.before.present}).Count
  preexistingMissing=$qdmPreexistingMissing;outputs=$qdmCompletedBefore
} | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $qdmOutputBeforePath -Encoding UTF8
$qdmSetup=Join-Path $qdmRoot 'release\QDM-0.3.0-win-x64-setup.exe'
$qdmSetupExpected=@($qdmPackage.artifacts | Where-Object {$_.name -eq 'QDM-0.3.0-win-x64-setup.exe'})[0].sha256
if((Get-FileHash -LiteralPath $qdmSetup).Hash.ToLowerInvariant() -ne $qdmSetupExpected){throw 'Installer SHA-256 does not match its verified package.'}
$qdmInstaller=Start-Process -FilePath $qdmSetup -ArgumentList '/S' -PassThru -Wait
if($qdmInstaller.ExitCode -ne 0){throw ('Installer failed with '+$qdmInstaller.ExitCode)}
if((Get-FileHash -LiteralPath $qdmInstalledExe).Hash.ToLowerInvariant() -ne $qdmExpected){throw 'Installed program does not match the accepted binary.'}
$qdmNew=Start-Process -FilePath $qdmInstalledExe -PassThru
$qdmDeadline=[DateTime]::UtcNow.AddSeconds(25)
do{
  $qdmNew.Refresh()
  if($qdmNew.HasExited){throw ('Installed Slint program exited unexpectedly: '+$qdmNew.ExitCode)}
  if($qdmNew.MainWindowHandle -ne [IntPtr]::Zero -and $qdmNew.MainWindowTitle -like 'QDM*QUEEN Download Manager*'){break}
  Start-Sleep -Milliseconds 100
}while([DateTime]::UtcNow -lt $qdmDeadline)
if($qdmNew.MainWindowTitle -notlike 'QDM*QUEEN Download Manager*'){throw 'Installed Slint main window was not observed.'}
$qdmAfter=if(Test-Path -LiteralPath $qdmStatePath){Get-Content -LiteralPath $qdmStatePath -Raw -Encoding UTF8 | ConvertFrom-Json}else{$null}
$qdmOutputComparison=@(Compare-QdmCompletedOutputs -Before $qdmCompletedBefore)
$qdmOutputFailures=@($qdmOutputComparison | Where-Object {$_.status -in @('missing_after_install','changed_after_install','unreadable_after_install')})
$qdmOutputAfterPath=Join-Path $qdmBackup 'completed-outputs-after.json'
$qdmOutputAudit=[ordered]@{
  schema=1;status=$(if($qdmOutputFailures.Count){'failed'}else{'passed'});checkedAt=[DateTime]::UtcNow.ToString('o')
  existingBefore=@($qdmCompletedBefore | Where-Object {$_.before.present}).Count
  verifiedUnchanged=@($qdmOutputComparison | Where-Object {$_.status -eq 'verified_unchanged'}).Count
  preexistingMissing=$qdmPreexistingMissing;outputs=$qdmOutputComparison
}
$qdmOutputAudit | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $qdmOutputAfterPath -Encoding UTF8
if($qdmOutputFailures.Count){throw ('Completed-output verification failed: '+(($qdmOutputFailures | ForEach-Object {$_.taskId+' '+$_.status+' '+$_.path}) -join '; '))}
foreach($qdmTask in @($qdmSaved.tasks)){
  if($null -eq $qdmTask){continue}
  $qdmFound=@($qdmAfter.tasks | Where-Object {$_.view.id -eq $qdmTask.view.id})
  if($qdmFound.Count -ne 1 -or $qdmFound[0].view.downloaded_bytes -lt $qdmTask.view.downloaded_bytes){throw ('A saved download did not survive upgrade: '+$qdmTask.view.id)}
  if($qdmTask.view.status -eq 'completed' -and ($qdmFound[0].view.status -ne 'completed' -or $qdmFound[0].view.file_path -cne $qdmTask.view.file_path)){throw ('A completed history record changed status or output path: '+$qdmTask.view.id)}
}
if($qdmSaved -and $qdmAfter.settings.connections -ne $qdmSaved.settings.connections){throw 'Saved connection preference was not preserved.'}
$qdmShell=New-Object -ComObject WScript.Shell
$qdmDesktop=[Environment]::GetFolderPath('Desktop')
# Numeric characters avoid Windows PowerShell 5 interpreting UTF-8 source as
# the current ANSI codepage. Require the canonical name, never a wildcard hit.
$qdmShortcutName='QDM '+[char]0x2014+' QUEEN Download Manager.lnk'
$qdmShortcut=Get-Item -LiteralPath (Join-Path $qdmDesktop $qdmShortcutName) -Force
if($qdmShortcut.PSIsContainer -or $qdmShortcut.Name -cne $qdmShortcutName){throw 'The canonical QDM desktop shortcut is missing or incorrectly named.'}
$qdmLink=$qdmShell.CreateShortcut($qdmShortcut.FullName)
if(![string]::Equals($qdmLink.TargetPath,$qdmInstalledExe,[StringComparison]::OrdinalIgnoreCase)){throw 'The desktop shortcut points to a different program.'}
# Only the observed UTF-8-as-CP1252 duplicate is eligible for cleanup, and only
# when it still launches this installation without custom arguments.
$qdmMojibakeName='QDM '+[char]0x00E2+[char]0x20AC+[char]0x201D+' QUEEN Download Manager.lnk'
$qdmMojibakePath=Join-Path $qdmDesktop $qdmMojibakeName
$qdmShortcutCleanup=[ordered]@{path=$qdmMojibakePath;status='not_found';backup=$null;sha256=$null;target=$null}
if(Test-Path -LiteralPath $qdmMojibakePath){
  $qdmMojibakeItem=Get-Item -LiteralPath $qdmMojibakePath -Force
  if($qdmMojibakeItem.PSIsContainer -or $qdmMojibakeItem.Name -cne $qdmMojibakeName){
    $qdmShortcutCleanup.status='preserved_nonmatching_entry'
  } else {
    $qdmMojibakeLink=$qdmShell.CreateShortcut($qdmMojibakePath)
    $qdmShortcutCleanup.target=$qdmMojibakeLink.TargetPath
    if(![string]::Equals($qdmMojibakeLink.TargetPath,$qdmInstalledExe,[StringComparison]::OrdinalIgnoreCase)){
      $qdmShortcutCleanup.status='preserved_other_target'
    } elseif(![string]::IsNullOrWhiteSpace($qdmMojibakeLink.Arguments)){
      $qdmShortcutCleanup.status='preserved_custom_arguments'
    } else {
      $qdmShortcutBackup=Join-Path $qdmBackup 'desktop-shortcut-mojibake.lnk'
      $qdmShortcutHash=(Get-FileHash -LiteralPath $qdmMojibakePath -Algorithm SHA256).Hash.ToLowerInvariant()
      [IO.File]::Copy($qdmMojibakePath,$qdmShortcutBackup,$false)
      if((Get-FileHash -LiteralPath $qdmShortcutBackup -Algorithm SHA256).Hash.ToLowerInvariant() -cne $qdmShortcutHash -or
         (Get-FileHash -LiteralPath $qdmMojibakePath -Algorithm SHA256).Hash.ToLowerInvariant() -cne $qdmShortcutHash){throw 'The duplicate shortcut backup did not match; the desktop entry was not removed.'}
      Remove-Item -LiteralPath $qdmMojibakePath
      if(Test-Path -LiteralPath $qdmMojibakePath){throw 'The backed-up duplicate shortcut could not be removed.'}
      $qdmShortcutCleanup.status='archived_and_removed'
      $qdmShortcutCleanup.backup=$qdmShortcutBackup
      $qdmShortcutCleanup.sha256=$qdmShortcutHash
    }
  }
}
$qdmResult=[ordered]@{status='passed';installedAt=[DateTime]::UtcNow.ToString('o');version=(Get-Item -LiteralPath $qdmInstalledExe).VersionInfo.ProductVersion;path=$qdmInstalledExe;binarySha256=$qdmExpected;backup=$qdmBackup;processId=$qdmNew.Id;windowTitle=$qdmNew.MainWindowTitle;recordsPreserved=@($qdmSaved.tasks).Count;connections=$qdmAfter.settings.connections;shortcut=$qdmShortcut.FullName;shortcutTarget=$qdmLink.TargetPath;shortcutNameCodepoints=@($qdmShortcut.Name.ToCharArray() | ForEach-Object {[int]$_});shortcutCleanup=$qdmShortcutCleanup;uninstallRegistered=(Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native').DisplayVersion;completedOutputs=$qdmOutputAudit;preexistingMissing=$qdmPreexistingMissing;outputBeforeEvidence=$qdmOutputBeforePath;outputAfterEvidence=$qdmOutputAfterPath}
$qdmResult | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $qdmEvidence 'installed-release.json') -Encoding UTF8
$qdmResult | ConvertTo-Json -Depth 10
