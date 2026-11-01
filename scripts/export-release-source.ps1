param([string]$EvidenceDirectory='docs\evidence\slint-idm-20261027')
$ErrorActionPreference='Stop'
$qdmRoot=Split-Path -Parent $PSScriptRoot
$qdmRelease=Join-Path $qdmRoot 'release'
$qdmEvidence=Join-Path $qdmRoot $EvidenceDirectory
$qdmStage=Join-Path $env:TEMP ('qdm-source-export-'+[Guid]::NewGuid().ToString('N'))
$qdmSource=Join-Path $qdmStage 'QDM-0.3.0-source'
New-Item -ItemType Directory -Force $qdmSource,$qdmRelease | Out-Null
try {
  foreach($qdmDirectory in @('src','ui','tests','scripts','resources','extensions','.cargo')){
    $qdmPath=Join-Path $qdmRoot $qdmDirectory
    if(Test-Path -LiteralPath $qdmPath){Copy-Item -LiteralPath $qdmPath -Destination (Join-Path $qdmSource $qdmDirectory) -Recurse -Force}
  }
  foreach($qdmFile in @('Cargo.toml','Cargo.lock','build.rs','README.md','LICENSE','LICENSE.md')){
    $qdmPath=Join-Path $qdmRoot $qdmFile
    if(Test-Path -LiteralPath $qdmPath){Copy-Item -LiteralPath $qdmPath -Destination $qdmSource -Force}
  }
  $qdmDocs=Join-Path $qdmSource 'docs'
  New-Item -ItemType Directory -Force $qdmDocs | Out-Null
  Copy-Item -LiteralPath (Join-Path $qdmRoot 'docs\third-party') -Destination (Join-Path $qdmDocs 'third-party') -Recurse -Force
  foreach($qdmFile in @('idm-reverse-engineering-0.3.0.md','idm-reverse-engineering-evidence-0.3.0.json','ui-motion-review-0.3.0.md','ui-motion-review-0.3.0.json','release-report-0.3.0.md','release-results-0.3.0.json','release-artifact-privacy-scan-0.3.0.json')){
    $qdmPath=Join-Path $qdmRoot ('docs\'+$qdmFile)
    if(Test-Path -LiteralPath $qdmPath){Copy-Item -LiteralPath $qdmPath -Destination $qdmDocs -Force}
  }
  # Compact evidence companions make the source report's relative links work.
  # Full raw histories and reviewed media are in the separate evidence ZIP.
  $qdmFinalDocs=Join-Path $qdmDocs 'evidence\final'
  New-Item -ItemType Directory -Force $qdmFinalDocs | Out-Null
  foreach($qdmFile in @(
    'engine-acceptance.json','native-tests.json','native-tests.log','bridge.json',
    'public-https.json','ui\slint-ui-acceptance.json','zen-native.json',
    'staged-origin-recovery.json','adaptive-recovery.json','legacy-history.json',
    'tail-scheduling.json','tail-before.json','render-final.json',
    'existing-completed-file-location.json','idm-preflight-blocker.json',
    'resources-idm-idle-final.json','alpine-run6-tail-trace.json',
    'idm-connection-settings-final.json',
    'benchmark-after-idm-dialog-smoke.json',
    'benchmark-alpine-before-tail.json','benchmark-bom-rollback.json',
    'benchmark-alpine-tail-fixed.json','benchmark-alpine-release.json',
    'benchmark-32-capped-release.json','benchmark-32-fast-release.json',
    'benchmark-64-capped-release.json','alpine-tail-fixed-diagnostic.json',
    'alpine-tail-fixed-diagnostic.trace.jsonl','installed-release.json',
    'upgrade-history-verification.json','packaging.json',
    'installer-and-release-gate-tests.log'
  )){
    $qdmPath=Join-Path $qdmEvidence $qdmFile
    if(Test-Path -LiteralPath $qdmPath){
      $qdmDestination=Join-Path $qdmFinalDocs $qdmFile
      New-Item -ItemType Directory -Force (Split-Path -Parent $qdmDestination) | Out-Null
      Copy-Item -LiteralPath $qdmPath -Destination $qdmDestination -Force
    }
  }
  Copy-Item -LiteralPath (Join-Path $qdmEvidence 'motion-final\motion-capture.json') -Destination (Join-Path $qdmFinalDocs 'motion-capture.json') -Force
  # Only the explicit source allowlist is exported. It excludes toolchains,
  # compiled dependencies, proprietary IDM analysis files, live browser profiles,
  # pairing tokens, the user's history, and all downloaded payloads.
  $qdmZip=Join-Path $qdmRelease 'QDM-0.3.0-source.zip'
  Get-ChildItem -LiteralPath $qdmSource -Recurse -Force | Where-Object {$_.LastWriteTime.Year -lt 1980 -or $_.LastWriteTime.Year -gt 2107} | ForEach-Object {$_.LastWriteTime=[DateTime]::new(1980,1,1,12,0,0)}
  Compress-Archive -LiteralPath $qdmSource -DestinationPath $qdmZip -CompressionLevel Optimal -Force
  $qdmEvidenceZip=Join-Path $qdmRelease 'QDM-0.3.0-validation-evidence.zip'
  # Retain the raw test reports and their hashes. Export only the final,
  # visually reviewed media; older working captures remain on this machine.
  $qdmEvidenceStage=Join-Path $qdmStage 'QDM-0.3.0-validation-evidence'
  Copy-Item -LiteralPath $qdmEvidence -Destination $qdmEvidenceStage -Recurse -Force
  $qdmReviewedMedia=@(
    'motion-final\01-live-main.png','motion-final\02-settings.png',
    'motion-final\03-connections.png','motion-final\04-paused.png',
    'motion-final\05-finished-filter.png','motion-final\06-live-main.png',
    'motion-final\07-completed.png','motion-final\portrait-motion.mp4',
    'installed-final.png'
  )
  $qdmExcludedMedia=@(Get-ChildItem -LiteralPath $qdmEvidenceStage -File -Recurse | Where-Object {
    $_.Extension.ToLowerInvariant() -in @('.png','.jpg','.jpeg','.webp','.bmp','.gif','.mp4','.webm','.mov','.mkv','.avi')
  } | Where-Object {$_.FullName.Substring($qdmEvidenceStage.Length+1) -notin $qdmReviewedMedia})
  $qdmExcludedNames=@($qdmExcludedMedia | ForEach-Object {$_.FullName.Substring($qdmEvidenceStage.Length+1)})
  $qdmExcludedMedia | Remove-Item -Force
  [ordered]@{schema=1;exportedAt=[DateTime]::UtcNow.ToString('o');rawReportsUnchanged=$true;reviewedMediaAllowlist=$qdmReviewedMedia;excludedSupersededOrUnreviewedMedia=$qdmExcludedNames;note='Raw working captures remain in the original evidence folder. Their paths may still be referenced by historical reports.'} | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $qdmEvidenceStage 'export-inventory.json') -Encoding UTF8
  Compress-Archive -LiteralPath $qdmEvidenceStage -DestinationPath $qdmEvidenceZip -CompressionLevel Optimal -Force
  @($qdmZip,$qdmEvidenceZip)|ForEach-Object {[pscustomobject]@{name=[IO.Path]::GetFileName($_);path=$_;bytes=(Get-Item -LiteralPath $_).Length;sha256=(Get-FileHash -LiteralPath $_ -Algorithm SHA256).Hash.ToLowerInvariant()}}|ConvertTo-Json -Depth 3
} finally {
  if(Test-Path -LiteralPath $qdmStage){Remove-Item -LiteralPath $qdmStage -Recurse -Force}
}
