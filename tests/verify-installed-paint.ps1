param([string]$EvidenceDirectory='docs\evidence\slint-idm-20261027')
$ErrorActionPreference='Stop'
$qdmRoot=Split-Path -Parent $PSScriptRoot
$qdmEvidence=Join-Path $qdmRoot $EvidenceDirectory
$qdmInstall=Get-Content (Join-Path $qdmEvidence 'installed-paint-hotfix.json') -Raw -Encoding UTF8 | ConvertFrom-Json
if($qdmInstall.status -cne 'passed'){throw 'Installation is not verified.'}
$qdmApp=Get-Process -Id $qdmInstall.processId -ErrorAction Stop
if($qdmApp.Path -ine $qdmInstall.installedPath -or (Get-FileHash -LiteralPath $qdmApp.Path).Hash.ToLowerInvariant() -cne $qdmInstall.binarySha256){throw 'Installed application identity changed.'}
Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes
Add-Type -AssemblyName System.Windows.Forms
Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class QdmInstalledBounds {
 [StructLayout(LayoutKind.Sequential)] public struct Rect { public int Left,Top,Right,Bottom; }
 [DllImport("user32.dll")] public static extern bool GetWindowRect(IntPtr h,out Rect r);
 [DllImport("user32.dll")] public static extern IntPtr SetThreadDpiAwarenessContext(IntPtr context);
}
'@
[void][QdmInstalledBounds]::SetThreadDpiAwarenessContext([IntPtr](-4))
$qdmElement=[System.Windows.Automation.AutomationElement]::FromHandle($qdmApp.MainWindowHandle)
if($null -eq $qdmElement){throw 'Installed main window is unavailable.'}
$qdmItems=$qdmElement.FindAll([System.Windows.Automation.TreeScope]::Descendants,[System.Windows.Automation.Condition]::TrueCondition)
$qdmNames=@(foreach($qdmItem in $qdmItems){$qdmItem.Current.Name})
$qdmObsolete='Combining verified partial files into one output...'
if($qdmNames -ccontains $qdmObsolete){throw 'The migrated completed item still displays an obsolete assembly notice.'}
$qdmState=Get-Content (Join-Path $env:APPDATA 'QDM\state-v2.json') -Raw -Encoding UTF8 | ConvertFrom-Json
$qdmCompleted=@($qdmState.tasks | Where-Object {$_.view.status -ceq 'completed'})
if($qdmCompleted.Count -eq 0){throw 'No completed history is available for this upgrade check.'}
if($qdmNames -cnotcontains $qdmCompleted[0].view.filename -or $qdmNames -cnotcontains '100.0%'){throw 'Completed history is not displayed.'}
$qdmIntegrity=@($qdmNames | Where-Object {$_ -eq 'Expected SHA-256 matched' -or $_ -like 'SHA-256 calculated*' -or $_ -eq 'Transfer complete'})
if($qdmIntegrity.Count -eq 0){throw 'Completed transfer integrity text is absent.'}
$qdmBounds=New-Object QdmInstalledBounds+Rect
if(![QdmInstalledBounds]::GetWindowRect($qdmApp.MainWindowHandle,[ref]$qdmBounds)){throw 'Window geometry is unavailable.'}
$qdmArea=[Windows.Forms.Screen]::FromHandle($qdmApp.MainWindowHandle).WorkingArea
$qdmFits=$qdmBounds.Left -ge $qdmArea.Left -and $qdmBounds.Top -ge $qdmArea.Top -and $qdmBounds.Right -le $qdmArea.Right -and $qdmBounds.Bottom -le $qdmArea.Bottom
if(!$qdmFits){throw 'Installed window extends beyond the working area.'}
$qdmCapture=Join-Path $qdmEvidence 'installed-paint-final.png'
& (Join-Path $PSScriptRoot 'capture-window.ps1') -TargetProcessId $qdmApp.Id -OutputPath $qdmCapture | Out-Null
$qdmReport=[ordered]@{
 schema=1;status='passed';verifiedAt=[DateTime]::UtcNow.ToString('o');binarySha256=$qdmInstall.binarySha256
 processId=$qdmApp.Id;installedPath=$qdmApp.Path;completedFilename=$qdmCompleted[0].view.filename
 obsoleteAssemblyNoticeHidden=$true;completedIntegrityText=$qdmIntegrity[0]
 displayedWorkerRecords=@($qdmNames | Where-Object {$_ -match '^Connection #[0-9]+:'}).Count
 windowFullyInsideWorkArea=$qdmFits
 window=[ordered]@{left=$qdmBounds.Left;top=$qdmBounds.Top;right=$qdmBounds.Right;bottom=$qdmBounds.Bottom}
 workArea=[ordered]@{left=$qdmArea.Left;top=$qdmArea.Top;right=$qdmArea.Right;bottom=$qdmArea.Bottom}
 screenshot=[ordered]@{path=$qdmCapture;bytes=(Get-Item $qdmCapture).Length;sha256=(Get-FileHash $qdmCapture).Hash.ToLowerInvariant()}
}
$qdmReport | ConvertTo-Json -Depth 6 | Set-Content (Join-Path $qdmEvidence 'installed-paint-display-verification.json') -Encoding UTF8
$qdmReport | ConvertTo-Json -Depth 6
