[CmdletBinding()]
param([switch]$Apply)
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$source = Join-Path $root 'target\release\QDM.exe'
$installed = Join-Path $env:LOCALAPPDATA 'Programs\QDM\QDM.exe'
$statePath = Join-Path $env:APPDATA 'QDM\state-v2.json'
$acceptPath = Join-Path $env:TEMP 'qdm-acceptance-v031-20261101.json'
$adaptivePath = Join-Path $env:TEMP 'qdm-adaptive-v031-20261101.json'
if (!(Test-Path $source) -or !(Test-Path $installed) -or !(Test-Path $statePath)) {
  throw 'QDM source executable, installed executable, or current state missing.'
}
$sourceHash = (Get-FileHash $source -Algorithm SHA256).Hash
$acceptance = Get-Content -LiteralPath $acceptPath -Raw | ConvertFrom-Json
$adaptive = Get-Content -LiteralPath $adaptivePath -Raw | ConvertFrom-Json
if ($acceptance.binarySha256 -ne $sourceHash -or $acceptance.passed -ne 34 -or $acceptance.failed -ne 0 -or !$acceptance.binaryUnchangedDuringRun) {
  throw 'Fresh 34/34 QDM acceptance gate is missing or is for another executable.'
}
if ($adaptive.binarySha256 -ne $sourceHash -or $adaptive.result -ne 'PASS' -or $adaptive.binarySha256After -ne $sourceHash) {
  throw 'Fresh adaptive-recovery gate is missing or failed.'
}
function Read-ProductionState {
  $state = Get-Content -LiteralPath $statePath -Raw | ConvertFrom-Json
  $activeStates = @('downloading','preparing','probing','connecting','verifying','assembling','merging')
  $active = @($state.tasks | Where-Object { $_.view.status -in $activeStates })
  if ($active.Count -ne 0) { throw "Production QDM has $($active.Count) active transfers; refusing update." }
  return $state
}
$before = Read-ProductionState
$running = @(Get-CimInstance Win32_Process -Filter "name='QDM.exe'" | Where-Object { $_.ExecutablePath -ieq $installed })
$oldHash = (Get-FileHash $installed -Algorithm SHA256).Hash
$plan = [ordered]@{ action = $(if ($Apply) { 'INSTALL' } else { 'DRY_RUN' }); sourceHash = $sourceHash; installedHash = $oldHash; historyCount = @($before.tasks).Count; productionActiveDownloads = 0; installedProcessCount = $running.Count; gateAcceptance = '34/34'; gateAdaptive = 'PASS' }
if (!$Apply) { $plan | ConvertTo-Json; return }
$backup = Join-Path $root ('backups\install-v031-' + (Get-Date -Format 'yyyyMMdd-HHmmss'))
New-Item -ItemType Directory -Force -Path $backup | Out-Null
Copy-Item -LiteralPath $statePath -Destination (Join-Path $backup 'state-v2.json') -Force
Copy-Item -LiteralPath $installed -Destination (Join-Path $backup 'QDM-previous.exe') -Force
foreach ($name in @('ui-settings.json','browser-pairing-v2.bin')) {
  $path = Join-Path (Split-Path $statePath -Parent) $name
  if (Test-Path $path) { Copy-Item -LiteralPath $path -Destination (Join-Path $backup $name) -Force }
}
$stage = Join-Path (Split-Path $installed -Parent) 'QDM.exe.perf-stage'
Copy-Item -LiteralPath $source -Destination $stage -Force
if ((Get-FileHash $stage -Algorithm SHA256).Hash -ne $sourceHash) { throw 'Staged executable hash failed.' }
$stopped = $false
$installedNew = $false
try {
  foreach ($item in $running) {
    $p = Get-Process -Id $item.ProcessId -ErrorAction Stop
    # Request ordinary close before an idle-only termination. Tray mode may hide
    # instead of quitting; never force-stop an active transfer.
    if ($p.MainWindowHandle -ne [IntPtr]::Zero) { [void]$p.CloseMainWindow(); [void]$p.WaitForExit(3000) }
    $p.Refresh()
    if (!$p.HasExited) {
      $confirm = Read-ProductionState
      if (@($confirm.tasks).Count -ne @($before.tasks).Count) { throw 'History changed during idle shutdown window.' }
      Stop-Process -Id $p.Id -Force -ErrorAction Stop
      [void]$p.WaitForExit(10000)
      $p.Refresh()
      if (!$p.HasExited) { throw 'QDM process would not stop.' }
    }
  }
  $stopped = $running.Count -gt 0
  $beforeReplacement = Read-ProductionState
  if (@($beforeReplacement.tasks).Count -ne @($before.tasks).Count) { throw 'Production history changed before replacement.' }
  Copy-Item -LiteralPath $stage -Destination $installed -Force
  if ((Get-FileHash $installed -Algorithm SHA256).Hash -ne $sourceHash) { throw 'Installed SHA-256 mismatch.' }
  $installedNew = $true
  $pNew = Start-Process -FilePath $installed -PassThru
  for ($i = 0; $i -lt 100; $i++) {
    Start-Sleep -Milliseconds 100
    $pNew.Refresh()
    if ($pNew.HasExited) { throw 'Updated QDM exited during startup.' }
    if ($pNew.MainWindowHandle -ne [IntPtr]::Zero) { break }
  }
  if ($pNew.MainWindowHandle -eq [IntPtr]::Zero) { throw 'Updated QDM did not open a native window.' }
  $after = Read-ProductionState
  if (@($after.tasks).Count -ne @($before.tasks).Count) { throw 'Downloaded history count changed after upgrade.' }
  if ($after.settings.connections -ne $before.settings.connections -or $after.settings.max_concurrent -ne $before.settings.max_concurrent) { throw 'Saved download settings changed after upgrade.' }
  $receipt = [ordered]@{ status = 'INSTALLED'; installedAt = (Get-Date).ToString('o'); installedExe = $installed; version = '0.3.1'; sha256 = $sourceHash; previousHash = $oldHash; productionHistory = @($after.tasks).Count; connections = $after.settings.connections; downloadsAtOnce = $after.settings.max_concurrent; active = 0; pid = $pNew.Id; backup = $backup; acceptance = '34/34'; adaptiveRecovery = 'PASS' }
  $receipt | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $root 'docs\QDM-PERFORMANCE-INSTALL-2026-11-01.json') -Encoding UTF8
  $receipt | ConvertTo-Json
} catch {
  $failure = $_
  if ($installedNew -and (Test-Path $installed)) {
    $newProc = @(Get-CimInstance Win32_Process -Filter "name='QDM.exe'" | Where-Object { $_.ExecutablePath -ieq $installed })
    foreach ($p in $newProc) { Stop-Process -Id $p.ProcessId -Force -ErrorAction SilentlyContinue }
  }
  Copy-Item -LiteralPath (Join-Path $backup 'QDM-previous.exe') -Destination $installed -Force
  if ($stopped) { Start-Process -FilePath $installed | Out-Null }
  throw "QDM performance upgrade rolled back: $failure"
} finally {
  if (Test-Path $stage) { Remove-Item -LiteralPath $stage -Force -ErrorAction SilentlyContinue }
}