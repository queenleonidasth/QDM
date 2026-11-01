param([Parameter(Mandatory=$true)][string]$ConfigPath)
$ErrorActionPreference='Stop'
$qdmConfig=Get-Content -LiteralPath $ConfigPath -Raw -Encoding UTF8 | ConvertFrom-Json
function Emit($value) { [Console]::Out.WriteLine(($value | ConvertTo-Json -Compress -Depth 6)); [Console]::Out.Flush() }
$qdmProcess=[Diagnostics.Process]::GetProcessById([int]$qdmConfig.processId)
$qdmProcess.Refresh()
$qdmStarted=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
$qdmBaseline=$qdmProcess.TotalProcessorTime.TotalMilliseconds
$qdmFirstReady=$null
$qdmFirstWrite=$null
$qdmStable=0
Emit @{type='ready';at=$qdmStarted;pid=$qdmProcess.Id;name=$qdmProcess.ProcessName;cpuMs=$qdmBaseline;workingSet=$qdmProcess.WorkingSet64;privateBytes=$qdmProcess.PrivateMemorySize64;pollMs=[int]$qdmConfig.pollMs}
try {
 while(([DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()-$qdmStarted) -lt [long]$qdmConfig.timeoutMs){
  $qdmProcess.Refresh()
  if($qdmProcess.HasExited){throw 'Target process exited before the output was ready'}
  $qdmNow=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds()
  Emit @{type='sample';at=$qdmNow;cpuMs=$qdmProcess.TotalProcessorTime.TotalMilliseconds;workingSet=$qdmProcess.WorkingSet64;privateBytes=$qdmProcess.PrivateMemorySize64;threads=$qdmProcess.Threads.Count}
  $qdmUnlocked=$false
  $qdmFile=[IO.FileInfo]::new([string]$qdmConfig.file)
  if($qdmFile.Exists -and $qdmFile.Length -eq [long]$qdmConfig.expectedBytes){
   try {
    $qdmHandle=[IO.File]::Open($qdmFile.FullName,[IO.FileMode]::Open,[IO.FileAccess]::Read,[IO.FileShare]::None)
    try {$qdmUnlocked=($qdmHandle.Length -eq [long]$qdmConfig.expectedBytes)} finally {$qdmHandle.Dispose()}
   } catch [IO.IOException] {$qdmUnlocked=$false}
  }
  if($qdmUnlocked){
   $qdmWrite=$qdmFile.LastWriteTimeUtc.Ticks
   if($null -eq $qdmFirstReady -or $qdmFirstWrite -ne $qdmWrite){$qdmFirstReady=$qdmNow;$qdmFirstWrite=$qdmWrite;$qdmStable=1}
   else {$qdmStable++}
   if($qdmStable -ge 3){
    Emit @{type='complete';at=$qdmNow;firstReadyAt=$qdmFirstReady;confirmedAt=$qdmNow;fileBytes=$qdmFile.Length;cpuMs=$qdmProcess.TotalProcessorTime.TotalMilliseconds;workingSet=$qdmProcess.WorkingSet64;privateBytes=$qdmProcess.PrivateMemorySize64}
    exit 0
   }
  } else {$qdmFirstReady=$null;$qdmFirstWrite=$null;$qdmStable=0}
  Start-Sleep -Milliseconds ([int]$qdmConfig.pollMs)
 }
 throw 'Timed out waiting for an exact-size, exclusively readable final file'
} catch {Emit @{type='error';at=[DateTimeOffset]::UtcNow.ToUnixTimeMilliseconds();message=$_.Exception.Message};exit 1}
