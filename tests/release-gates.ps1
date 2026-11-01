# Synthetic report validation only: never installs, packages or touches a user profile.
$ErrorActionPreference='Stop'
. (Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\release-gates.ps1')
$qdmTestDirectory=Join-Path ([IO.Path]::GetTempPath()) ('qdm-gate-validation-'+[Guid]::NewGuid().ToString('N'))
$qdmTestHash='a'*64
$qdmTestFinished=[DateTime]::UtcNow.ToString('o')
$qdmFixtures=@{}
$qdmResults=New-Object 'System.Collections.Generic.List[object]'
function New-QdmPassingCases([int]$Count){
  return @(1..$Count | ForEach-Object {[pscustomobject]@{name=('case-'+$_);status='passed'}})
}
function New-QdmPassingReport {
  return [ordered]@{binarySha256=$qdmTestHash;binarySha256After=$qdmTestHash;finishedAt=$qdmTestFinished}
}
function Save-QdmTestFixture([string]$Name,$Record){
  $qdmTestPath=Join-Path $qdmTestDirectory $Name
  New-Item -ItemType Directory -Path (Split-Path -Parent $qdmTestPath) -Force | Out-Null
  $Record | ConvertTo-Json -Depth 8 | Set-Content -LiteralPath $qdmTestPath -Encoding UTF8
}
function Reject-QdmTestFixture([string]$Name,[string]$Reason,[scriptblock]$Mutate){
  $qdmDocument=$qdmFixtures[$Name] | ConvertFrom-Json
  & $Mutate $qdmDocument
  Save-QdmTestFixture $Name $qdmDocument
  $qdmRejected=$false
  try {Assert-QdmReleaseGates -EvidenceDirectory $qdmTestDirectory -BinarySha256 $qdmTestHash | Out-Null}
  catch {$qdmRejected=$true}
  finally {Save-QdmTestFixture $Name ($qdmFixtures[$Name] | ConvertFrom-Json)}
  if(!$qdmRejected){throw ('Release validator incorrectly accepted '+$Reason)}
  $qdmResults.Add([pscustomobject]@{name=$Reason;status='passed'})
}
try {
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.cases=New-QdmPassingCases 34
  $qdmRecord.cases[0].name='resume-validation-is-visible-and-pause-all-keeps-one-owner'
  $qdmRecord.passed=34;$qdmRecord.failed=0;$qdmRecord.binaryUnchangedDuringRun=$true
  $qdmFixtures['engine-acceptance.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.cases=New-QdmPassingCases 18;$qdmRecord.passed=18;$qdmRecord.failed=0;$qdmRecord.skipped=0
  $qdmFixtures['bridge.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.cases=New-QdmPassingCases 2;$qdmRecord.passed=2;$qdmRecord.failed=0;$qdmRecord.ok=$true
  $qdmFixtures['public-https.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.steps=New-QdmPassingCases 13;$qdmRecord.passed=13;$qdmRecord.failed=0;$qdmRecord.status='passed'
  $qdmFixtures['ui\slint-ui-acceptance.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.checks=@(1..10 | ForEach-Object {[pscustomobject]@{name=('check-'+$_);passed=$true}})
  $qdmRecord.passed=$true;$qdmRecord.failed=0;$qdmRecord.status='passed'
  $qdmFixtures['zen-native.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.status='passed';$qdmRecord.cases=@(
    [pscustomobject]@{mode='429';pauseDuringCooldown=$false;status='passed';completedWithinRetryAfterWithoutNewRequests=$true},
    [pscustomobject]@{mode='429';pauseDuringCooldown=$true;status='passed'},
    [pscustomobject]@{mode='reset';pauseDuringCooldown=$false;status='passed'}
  )
  $qdmFixtures['staged-origin-recovery.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport
  $qdmRecord.status='passed';$qdmRecord.passed=3;$qdmRecord.failed=0
  $qdmRecord.cases=@('waiting-headers','waiting-first-body','setup-cost' | ForEach-Object {[pscustomobject]@{name=$_;status='passed'}})
  $qdmFixtures['tail-scheduling.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport;$qdmRecord.result='PASS'
  $qdmFixtures['adaptive-recovery.json']=$qdmRecord | ConvertTo-Json -Depth 8
  $qdmRecord=New-QdmPassingReport;$qdmRecord.status='passed';$qdmRecord.exitCode=0;$qdmRecord.totalTests=5
  $qdmFixtures['native-tests.json']=$qdmRecord | ConvertTo-Json -Depth 8
  foreach($qdmName in $qdmFixtures.Keys){Save-QdmTestFixture $qdmName ($qdmFixtures[$qdmName] | ConvertFrom-Json)}
  $qdmAccepted=@(Assert-QdmReleaseGates -EvidenceDirectory $qdmTestDirectory -BinarySha256 $qdmTestHash)
  if($qdmAccepted.Count -ne 9 -or @($qdmAccepted | Where-Object {$_.reportSha256 -cnotmatch '^[a-f0-9]{64}$'}).Count){throw 'Complete evidence was not accepted with report fingerprints.'}
  $qdmResults.Add([pscustomobject]@{name='complete passing evidence accepted';status='passed'})

  Reject-QdmTestFixture 'engine-acceptance.json' 'old 33-case engine suite' {param($r) $r.cases=@($r.cases | Select-Object -First 33);$r.passed=33}
  Reject-QdmTestFixture 'engine-acceptance.json' 'unfinished engine report' {param($r) $r.PSObject.Properties.Remove('finishedAt')}
  Reject-QdmTestFixture 'engine-acceptance.json' 'missing final failure counter' {param($r) $r.PSObject.Properties.Remove('failed')}
  Reject-QdmTestFixture 'engine-acceptance.json' 'running engine case' {param($r) $r.cases[1].status='running'}
  Reject-QdmTestFixture 'engine-acceptance.json' 'different executable' {param($r) $r.binarySha256='b'*64}
  Reject-QdmTestFixture 'engine-acceptance.json' 'executable changed during test' {param($r) $r.binarySha256After='b'*64}
  Reject-QdmTestFixture 'engine-acceptance.json' 'false executable stability result' {param($r) $r.binaryUnchangedDuringRun=$false}
  Reject-QdmTestFixture 'engine-acceptance.json' 'missing resume ownership regression' {param($r) $r.cases[0].name='some-other-case'}
  Reject-QdmTestFixture 'bridge.json' 'partial bridge suite' {param($r) $r.cases=@($r.cases | Select-Object -First 17);$r.passed=17}
  Reject-QdmTestFixture 'bridge.json' 'skipped bridge case' {param($r) $r.skipped=1}
  Reject-QdmTestFixture 'public-https.json' 'unsuccessful public HTTPS cleanup' {param($r) $r.ok=$false}
  Reject-QdmTestFixture 'ui\slint-ui-acceptance.json' 'partial UI suite' {param($r) $r.steps=@($r.steps | Select-Object -First 12);$r.passed=12}
  Reject-QdmTestFixture 'ui\slint-ui-acceptance.json' 'unfinished UI report' {param($r) $r.status='running'}
  Reject-QdmTestFixture 'zen-native.json' 'false Zen pass result' {param($r) $r.passed=$false}
  Reject-QdmTestFixture 'zen-native.json' 'failed Zen child check' {param($r) $r.checks[0].passed=$false}
  Reject-QdmTestFixture 'zen-native.json' 'partial Zen suite' {param($r) $r.checks=@($r.checks | Select-Object -First 9)}
  Reject-QdmTestFixture 'staged-origin-recovery.json' 'missing accepted-stream rollback proof' {param($r) $r.cases[0].completedWithinRetryAfterWithoutNewRequests=$false}
  Reject-QdmTestFixture 'staged-origin-recovery.json' 'unfinished staged report' {param($r) $r.status='running'}
  Reject-QdmTestFixture 'tail-scheduling.json' 'partial tail scheduling suite' {param($r) $r.cases=@($r.cases | Select-Object -First 2);$r.passed=2}
  Reject-QdmTestFixture 'adaptive-recovery.json' 'unfinished adaptive report' {param($r) $r.result='RUNNING'}
  Reject-QdmTestFixture 'native-tests.json' 'native test process failure' {param($r) $r.exitCode=1}
  Reject-QdmTestFixture 'native-tests.json' 'empty native test run' {param($r) $r.totalTests=0}
  [ordered]@{status='passed';passed=$qdmResults.Count;failed=0;cases=$qdmResults;finishedAt=[DateTime]::UtcNow.ToString('o')} | ConvertTo-Json -Depth 5
} finally {
  if(Test-Path -LiteralPath $qdmTestDirectory){Remove-Item -LiteralPath $qdmTestDirectory -Recurse -Force}
}
