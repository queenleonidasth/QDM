$ErrorActionPreference='Stop'
$qdmRoot='C:\Users\QUEEN\Projects\QDM\native'
$qdmInstall=Join-Path $env:LOCALAPPDATA 'Programs\QDM'
$qdmExe=Join-Path $qdmInstall 'QDM.exe'
$qdmExpected=(Get-FileHash (Join-Path $qdmRoot 'target\release\QDM.exe') -Algorithm SHA256).Hash.ToLower()
$qdmDeadline=[DateTime]::UtcNow.AddMinutes(3)
do {
  $qdmReady=$false
  try {
    $qdmCore=Get-Content -Raw (Join-Path $qdmRoot 'docs\evidence\windows-acceptance.json') | ConvertFrom-Json
    $qdmReady=$qdmCore.binarySha256 -eq $qdmExpected -and $qdmCore.passed -eq 29 -and $qdmCore.failed -eq 0 -and $qdmCore.binaryUnchangedDuringRun
  } catch {}
  if(!$qdmReady){if([DateTime]::UtcNow -gt $qdmDeadline){throw 'Final core gate did not pass'}; Start-Sleep -Milliseconds 300}
} until($qdmReady)
foreach($qdmGate in @('bridge-windows.json','public-https.json','native-ui\native-ui-smoke.json')){
  $qdmEvidence=Get-Content -Raw (Join-Path $qdmRoot ('docs\evidence\'+$qdmGate)) | ConvertFrom-Json
  if($qdmEvidence.binarySha256 -ne $qdmExpected -or $qdmEvidence.failed -gt 0 -or $qdmEvidence.status -eq 'failed'){throw ('Release gate failed: '+$qdmGate)}
}
Add-Type -TypeDefinition 'using System; using System.Runtime.InteropServices; public static class QdmUpgradeNative { [DllImport("user32.dll")] public static extern bool PostMessageW(IntPtr hwnd,uint message,IntPtr wparam,IntPtr lparam); }'
$qdmApps=@(Get-Process -Name QDM -ErrorAction SilentlyContinue | Where-Object {$_.Path -eq $qdmExe})
foreach($qdmApp in $qdmApps){
  $qdmApp.Refresh()
  if($qdmApp.MainWindowHandle -eq [IntPtr]::Zero){throw 'An installed QDM process has no GUI handle; stop before updating its executable'}
  if(![QdmUpgradeNative]::PostMessageW($qdmApp.MainWindowHandle,0x111,[IntPtr]1106,[IntPtr]::Zero)){throw 'Could not request a graceful QDM exit'}
  if(!$qdmApp.WaitForExit(30000)){throw 'QDM has not finished saving its state'}
}
$qdmStatePath=Join-Path $env:APPDATA 'QDM\state-v2.json'
$qdmSavedState=Get-Content -Raw $qdmStatePath | ConvertFrom-Json
$qdmInstaller=Start-Process -FilePath (Join-Path $qdmRoot 'release\QDM-0.2.0-win-x64-setup.exe') -ArgumentList '/S' -PassThru -Wait
if($qdmInstaller.ExitCode -ne 0){throw ('Installer failed '+$qdmInstaller.ExitCode)}
$qdmActual=(Get-FileHash $qdmExe -Algorithm SHA256).Hash.ToLower()
if($qdmActual -ne $qdmExpected){throw 'Installed executable mismatch'}
$qdmApp=Start-Process -FilePath $qdmExe -PassThru
$qdmUiDeadline=[DateTime]::UtcNow.AddSeconds(15)
do {$qdmApp.Refresh(); if($qdmApp.MainWindowHandle -ne [IntPtr]::Zero -and $qdmApp.MainWindowTitle){break}; Start-Sleep -Milliseconds 100}while([DateTime]::UtcNow -lt $qdmUiDeadline)
$qdmCaption='QDM '+[char]0x2014+' QUEEN Download Manager'
if($qdmApp.MainWindowTitle -ne $qdmCaption){throw 'Installed window caption failed'}
$qdmAfterState=Get-Content -Raw $qdmStatePath | ConvertFrom-Json
if($qdmAfterState.settings.connections -ne $qdmSavedState.settings.connections){throw 'Saved connection setting changed during update'}
$qdmShortcut=Get-ChildItem -LiteralPath ([Environment]::GetFolderPath('Desktop')) -Filter 'QDM*.lnk' | Where-Object {$_.Name -like '*Download Manager*'} | Select-Object -First 1
$qdmShell=New-Object -ComObject WScript.Shell
$qdmLink=$qdmShell.CreateShortcut($qdmShortcut.FullName)
if($qdmLink.TargetPath -ne $qdmExe){throw 'Desktop shortcut target mismatch'}
$qdmInstalled=[ordered]@{
  installedAt=[DateTime]::UtcNow.ToString('o')
  version=(Get-Item $qdmExe).VersionInfo.ProductVersion
  path=$qdmExe
  sha256=$qdmActual
  exeBytes=(Get-Item $qdmExe).Length
  installedFiles=@(Get-ChildItem $qdmInstall -Recurse -File).Count
  installedBytes=(Get-ChildItem $qdmInstall -Recurse -File | Measure-Object Length -Sum).Sum
  shortcut=$qdmShortcut.FullName
  shortcutTarget=$qdmLink.TargetPath
  oldSetupPreserved=Test-Path 'C:\Users\QUEEN\Projects\QDM\release\QDM-0.1.0-win-x64-setup.exe'
  legacyHistoryPreserved=Test-Path (Join-Path $env:APPDATA 'QDM\downloads.json')
  electronResourcesAbsent=!(Test-Path (Join-Path $qdmInstall 'resources\app.asar'))
  uninstallRegistered=(Get-ItemProperty 'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall\QDM.Native').DisplayVersion
  processId=$qdmApp.Id
  windowTitle=$qdmApp.MainWindowTitle
  currentConnections=$qdmAfterState.settings.connections
  connectionSettingPreserved=$true
}
$qdmInstalled | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $qdmRoot 'docs\evidence\installed-release.json')
$qdmInstalled | ConvertTo-Json
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $qdmRoot 'tests\native-ui-smoke.ps1') -Binary $qdmExe -NodeBinary 'C:\Program Files\nodejs\node.exe' -OutputDirectory (Join-Path $qdmRoot 'docs\evidence\installed-ui')
if($LASTEXITCODE -ne 0){throw 'Installed UI verification failed'}
& powershell.exe -NoProfile -ExecutionPolicy Bypass -File (Join-Path $qdmRoot 'tests\measure-resources.ps1') -RootProcessId $qdmApp.Id -Label 'QDM-0.2.0-Rust-native-installed-idle' -OutputPath (Join-Path $qdmRoot 'docs\evidence\resources-rust-idle.json')
if($LASTEXITCODE -ne 0){throw 'Resource measurement failed'}
exit 0
