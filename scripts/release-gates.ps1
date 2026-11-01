# Shared by packaging and installation. Reports are written incrementally by
# several suites, so absence of a failure is insufficient to accept a release.
function Assert-QdmReleaseGates {
  param(
    [Parameter(Mandatory=$true)][string]$EvidenceDirectory,
    [Parameter(Mandatory=$true)][string]$BinarySha256
  )
  $qdmGateHash=$BinarySha256.ToLowerInvariant()
  if($qdmGateHash -cnotmatch '^[a-f0-9]{64}$'){throw 'Invalid release executable SHA-256.'}
  $qdmVerifiedGates=New-Object 'System.Collections.Generic.List[object]'

  function Read-QdmGate([string]$Name,[switch]$RequireAfterHash){
    $qdmGatePath=Join-Path $EvidenceDirectory $Name
    $qdmRecord=Get-Content -LiteralPath $qdmGatePath -Raw -Encoding UTF8 | ConvertFrom-Json
    if(([string]$qdmRecord.binarySha256).ToLowerInvariant() -cne $qdmGateHash){throw ('Evidence binary mismatch: '+$Name)}
    if([string]::IsNullOrWhiteSpace([string]$qdmRecord.finishedAt)){throw ('Unfinished release gate: '+$Name)}
    if($qdmRecord.status -in @('running','failed','error') -or $qdmRecord.fatal){throw ('Failed or unfinished release gate: '+$Name)}
    if($RequireAfterHash -and ([string]$qdmRecord.binarySha256After).ToLowerInvariant() -cne $qdmGateHash){throw ('Evidence executable changed during run: '+$Name)}
    return $qdmRecord
  }
  function Assert-QdmGateCount($Value,[int]$Expected,[string]$Label){
    if(($Value -isnot [int] -and $Value -isnot [long]) -or $Value -ne $Expected){throw ('Incomplete release evidence: '+$Label+' must be '+$Expected)}
  }
  function Assert-QdmGateCases($Record,[string]$Name,[int]$Expected,[string]$Collection='cases',[switch]$RequireCounters){
    $qdmItems=@($Record.$Collection)
    if($qdmItems.Count -ne $Expected -or @($qdmItems | Where-Object {$null -eq $_ -or $_.status -cne 'passed'}).Count){throw ('Release cases did not all pass: '+$Name+' (expected '+$Expected+')')}
    if($RequireCounters){
      Assert-QdmGateCount $Record.passed $Expected ($Name+' passed')
      Assert-QdmGateCount $Record.failed 0 ($Name+' failed')
    }
  }
  function Add-QdmVerifiedGate([string]$Name,[int]$PassedTests){
    $qdmVerifiedGates.Add([pscustomobject]@{
      report=$Name;passedTests=$PassedTests
      reportSha256=(Get-FileHash -LiteralPath (Join-Path $EvidenceDirectory $Name) -Algorithm SHA256).Hash.ToLowerInvariant()
    })
  }

  $qdmRecord=Read-QdmGate 'engine-acceptance.json' -RequireAfterHash
  Assert-QdmGateCases $qdmRecord 'engine-acceptance.json' 40 -RequireCounters
  if($qdmRecord.binaryUnchangedDuringRun -isnot [bool] -or !$qdmRecord.binaryUnchangedDuringRun){throw 'Engine acceptance did not certify an unchanged executable.'}
  if(@($qdmRecord.cases | Where-Object {$_.name -ceq 'resume-validation-is-visible-and-pause-all-keeps-one-owner'}).Count -ne 1){throw 'Engine acceptance is missing the resume-validation ownership regression.'}
  foreach($qdmRateCase in @('per-download-speed-limit-is-enforced','queue-speed-limit-is-shared-across-concurrent-transfers','default-queue-limit-applies-to-unassigned-downloads','changing-an-active-per-download-limit-takes-effect-without-restart','pausing-one-queue-member-does-not-starve-another')){
    if(@($qdmRecord.cases | Where-Object {$_.name -ceq $qdmRateCase}).Count -ne 1){throw ('Missing rate limit regression: '+$qdmRateCase)}
  }
  Add-QdmVerifiedGate 'engine-acceptance.json' 40

  $qdmRecord=Read-QdmGate 'bridge.json'
  Assert-QdmGateCases $qdmRecord 'bridge.json' 18 -RequireCounters
  Assert-QdmGateCount $qdmRecord.skipped 0 'bridge.json skipped'
  Add-QdmVerifiedGate 'bridge.json' 18

  $qdmRecord=Read-QdmGate 'public-https.json'
  Assert-QdmGateCases $qdmRecord 'public-https.json' 2 -RequireCounters
  if($qdmRecord.ok -isnot [bool] -or !$qdmRecord.ok){throw 'Public HTTPS acceptance did not finish successfully.'}
  Add-QdmVerifiedGate 'public-https.json' 2

  $qdmRecord=Read-QdmGate 'ui\slint-ui-acceptance.json'
  Assert-QdmGateCases $qdmRecord 'ui\slint-ui-acceptance.json' 14 -Collection 'steps' -RequireCounters
  if($qdmRecord.status -cne 'passed'){throw 'Slint UI acceptance did not finish successfully.'}
  if(@($qdmRecord.steps | Where-Object {$_.name -ceq 'Details sheet updates an active task queue and per-download budget'}).Count -ne 1){throw 'Slint UI acceptance is missing the transfer limits regression.'}
  Add-QdmVerifiedGate 'ui\slint-ui-acceptance.json' 14

  $qdmRecord=Read-QdmGate 'zen-native.json'
  Assert-QdmGateCount $qdmRecord.failed 0 'zen-native.json failed'
  if($qdmRecord.status -cne 'passed' -or $qdmRecord.passed -isnot [bool] -or !$qdmRecord.passed){throw 'Zen browser acceptance did not finish successfully.'}
  $qdmChecks=@($qdmRecord.checks)
  if($qdmChecks.Count -ne 10 -or @($qdmChecks | Where-Object {$null -eq $_ -or $_.passed -isnot [bool] -or !$_.passed}).Count){throw 'Zen browser acceptance must contain all 10 passing checks.'}
  Add-QdmVerifiedGate 'zen-native.json' 10

  $qdmRecord=Read-QdmGate 'staged-origin-recovery.json' -RequireAfterHash
  Assert-QdmGateCases $qdmRecord 'staged-origin-recovery.json' 3
  if($qdmRecord.status -cne 'passed'){throw 'Staged origin recovery did not finish successfully.'}
  $qdmRollback=@($qdmRecord.cases | Where-Object {$_.mode -ceq '429' -and $_.pauseDuringCooldown -is [bool] -and !$_.pauseDuringCooldown})
  if($qdmRollback.Count -ne 1 -or $qdmRollback[0].completedWithinRetryAfterWithoutNewRequests -isnot [bool] -or !$qdmRollback[0].completedWithinRetryAfterWithoutNewRequests){throw 'Staged origin recovery is missing the accepted-stream rollback regression.'}
  Add-QdmVerifiedGate 'staged-origin-recovery.json' 3

  $qdmRecord=Read-QdmGate 'tail-scheduling.json' -RequireAfterHash
  Assert-QdmGateCases $qdmRecord 'tail-scheduling.json' 3 -RequireCounters
  if($qdmRecord.status -cne 'passed'){throw 'Tail scheduling acceptance did not finish successfully.'}
  foreach($qdmTailCase in @('waiting-headers','waiting-first-body','setup-cost')){
    if(@($qdmRecord.cases | Where-Object {$_.name -ceq $qdmTailCase}).Count -ne 1){throw ('Missing tail scheduling regression: '+$qdmTailCase)}
  }
  Add-QdmVerifiedGate 'tail-scheduling.json' 3

  $qdmRecord=Read-QdmGate 'adaptive-recovery.json' -RequireAfterHash
  if($qdmRecord.result -cne 'PASS'){throw 'Adaptive recovery did not pass on this executable.'}
  Add-QdmVerifiedGate 'adaptive-recovery.json' 1

  $qdmRecord=Read-QdmGate 'native-tests.json' -RequireAfterHash
  if($qdmRecord.status -cne 'passed'){throw 'Native unit tests did not pass.'}
  Assert-QdmGateCount $qdmRecord.exitCode 0 'native-tests.json exitCode'
  if(($qdmRecord.totalTests -isnot [int] -and $qdmRecord.totalTests -isnot [long]) -or $qdmRecord.totalTests -le 0){throw 'Native unit evidence must record a nonempty successful test run.'}
  Add-QdmVerifiedGate 'native-tests.json' $qdmRecord.totalTests

  return $qdmVerifiedGates.ToArray()
}
