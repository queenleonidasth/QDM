param(
  [Parameter(Mandatory=$true)][int[]]$RootProcessId,
  [Parameter(Mandatory=$true)][string]$OutputPath,
  [string]$Label = 'QDM',
  [int]$WarmupSeconds = 5,
  [int]$DurationSeconds = 15,
  [int]$IntervalMilliseconds = 500
)

$ErrorActionPreference = 'Stop'
if ($DurationSeconds -lt 2 -or $IntervalMilliseconds -lt 100) {
  throw 'Use a duration of at least 2 seconds and an interval of at least 100 ms.'
}
$logicalProcessors = [Environment]::ProcessorCount
$samples = [System.Collections.Generic.List[object]]::new()
$previousCpu = @{}
$previousElapsed = $null
$totalCpuSeconds = [double]0
$totalObservedSeconds = [double]0
$startedAt = [DateTime]::UtcNow

function Get-ObservedProcessTree {
  $inventory = @(Get-CimInstance Win32_Process -Property ProcessId,ParentProcessId,Name,ExecutablePath)
  $selected = [System.Collections.Generic.HashSet[int]]::new()
  foreach ($rootId in $RootProcessId) { [void]$selected.Add($rootId) }
  $changed = $true
  while ($changed) {
    $changed = $false
    foreach ($entry in $inventory) {
      if ($selected.Contains([int]$entry.ParentProcessId) -and $selected.Add([int]$entry.ProcessId)) { $changed = $true }
    }
  }
  foreach ($entry in $inventory) {
    if ($selected.Contains([int]$entry.ProcessId)) {
      $observedProcess = Get-Process -Id $entry.ProcessId -ErrorAction SilentlyContinue
      if ($null -ne $observedProcess) {
        [pscustomobject]@{
          processId = [int]$observedProcess.Id
          parentProcessId = [int]$entry.ParentProcessId
          name = $entry.Name
          executable = $entry.ExecutablePath
          workingSetBytes = [long]$observedProcess.WorkingSet64
          privateBytes = [long]$observedProcess.PrivateMemorySize64
          cpuSeconds = [double]$observedProcess.TotalProcessorTime.TotalSeconds
          threads = [int]$observedProcess.Threads.Count
          handles = [int]$observedProcess.HandleCount
        }
      }
    }
  }
}

if ($WarmupSeconds -gt 0) { Start-Sleep -Seconds $WarmupSeconds }
$watch = [Diagnostics.Stopwatch]::StartNew()
while ($watch.Elapsed.TotalSeconds -lt $DurationSeconds) {
  $processes = @(Get-ObservedProcessTree)
  if ($processes.Count -eq 0) { throw 'None of the requested application processes is running.' }
  $now = [DateTime]::UtcNow
  $elapsed = [double]$watch.Elapsed.TotalSeconds
  $cpuDelta = [double]0
  $nextCpu = @{}
  foreach ($observedProcess in $processes) {
    $key = [string]$observedProcess.processId
    $nextCpu[$key] = [double]$observedProcess.cpuSeconds
    if ($previousCpu.ContainsKey($key)) {
      # Avoid Math.Max(0, fractionalSeconds): PowerShell can select its integer
      # overload and round away sub-second CPU use. Keep both operands double.
      $processDelta = [double]$observedProcess.cpuSeconds - [double]$previousCpu[$key]
      if ($processDelta -gt 0.0) { $cpuDelta += $processDelta }
    }
  }
  $intervalSeconds = $null
  $cpuOneCorePercent = $null
  if ($null -ne $previousElapsed) {
    $intervalSeconds = $elapsed - [double]$previousElapsed
    if ($intervalSeconds -le 0.0) { throw 'The monotonic resource sample interval did not advance.' }
    $cpuOneCorePercent = 100.0 * $cpuDelta / $intervalSeconds
    $totalCpuSeconds += $cpuDelta
    $totalObservedSeconds += $intervalSeconds
  }
  $cpuMachinePercent = if ($null -eq $cpuOneCorePercent) { $null } else { $cpuOneCorePercent / $logicalProcessors }
  $samples.Add([pscustomobject]@{
    utc = $now.ToString('o')
    elapsedSeconds = $elapsed
    intervalSeconds = $intervalSeconds
    cpuDeltaSeconds = if ($null -eq $intervalSeconds) { $null } else { $cpuDelta }
    processCount = $processes.Count
    workingSetBytes = [long](($processes | Measure-Object workingSetBytes -Sum).Sum)
    privateBytes = [long](($processes | Measure-Object privateBytes -Sum).Sum)
    threadCount = [long](($processes | Measure-Object threads -Sum).Sum)
    handleCount = [long](($processes | Measure-Object handles -Sum).Sum)
    cpuOneCorePercent = $cpuOneCorePercent
    cpuMachinePercent = $cpuMachinePercent
    processes = $processes
  })
  $previousCpu = $nextCpu
  $previousElapsed = $elapsed
  Start-Sleep -Milliseconds $IntervalMilliseconds
}
$watch.Stop()

function Summarize-Values([string]$Property) {
  $values = @($samples | ForEach-Object { $_.$Property } | Where-Object { $null -ne $_ } | Sort-Object)
  if ($values.Count -eq 0) { return $null }
  $middle = [int][Math]::Floor($values.Count / 2)
  $median = if ($values.Count % 2) { [double]$values[$middle] } else { ([double]$values[$middle - 1] + [double]$values[$middle]) / 2 }
  [pscustomobject]@{
    minimum = [double]$values[0]
    median = $median
    mean = [double](($values | Measure-Object -Average).Average)
    maximum = [double]$values[-1]
  }
}

$aggregateOneCore = if ($totalObservedSeconds -gt 0.0) { 100.0 * $totalCpuSeconds / $totalObservedSeconds } else { $null }
$report = [pscustomobject]@{
  schema = 2
  label = $Label
  startedAt = $startedAt.ToString('o')
  finishedAt = [DateTime]::UtcNow.ToString('o')
  os = [Environment]::OSVersion.VersionString
  logicalProcessors = $logicalProcessors
  rootProcessIds = $RootProcessId
  warmupSeconds = $WarmupSeconds
  durationSeconds = $watch.Elapsed.TotalSeconds
  intervalMilliseconds = $IntervalMilliseconds
  method = 'Observed application process tree only. Working sets can count shared pages more than once; private bytes are reported separately. CPU uses double-precision cumulative process-time deltas divided by monotonic sample intervals; one-core percent uses 100% for one fully utilized logical processor and machine percent divides by logical processor count. Aggregate CPU is time-weighted. Sampling/fixture process CPU is excluded; very short-lived children between samples are not observed.'
  cpuTotalSeconds = $totalCpuSeconds
  cpuObservedSeconds = $totalObservedSeconds
  cpuAggregateOneCorePercent = $aggregateOneCore
  cpuAggregateMachinePercent = if ($null -eq $aggregateOneCore) { $null } else { $aggregateOneCore / $logicalProcessors }
  workingSetBytes = Summarize-Values 'workingSetBytes'
  privateBytes = Summarize-Values 'privateBytes'
  cpuOneCorePercent = Summarize-Values 'cpuOneCorePercent'
  cpuMachinePercent = Summarize-Values 'cpuMachinePercent'
  processCount = Summarize-Values 'processCount'
  threadCount = Summarize-Values 'threadCount'
  handleCount = Summarize-Values 'handleCount'
  samples = $samples
}
$parent = Split-Path -Parent ([IO.Path]::GetFullPath($OutputPath))
[IO.Directory]::CreateDirectory($parent) | Out-Null
$report | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $OutputPath -Encoding utf8
$report | Select-Object label,workingSetBytes,privateBytes,cpuAggregateOneCorePercent,cpuOneCorePercent,processCount | ConvertTo-Json -Depth 4
