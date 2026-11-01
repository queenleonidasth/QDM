$ErrorActionPreference='Stop'
# Load only the two file-inventory functions; never execute the installer.
$qdmInstallerScript=Join-Path (Split-Path -Parent $PSScriptRoot) 'scripts\install-slint-verified.ps1'
$qdmTokens=$null;$qdmParseErrors=$null
$qdmAst=[Management.Automation.Language.Parser]::ParseFile($qdmInstallerScript,[ref]$qdmTokens,[ref]$qdmParseErrors)
if($qdmParseErrors.Count){throw ($qdmParseErrors | Out-String)}
foreach($qdmFunctionName in @('Get-QdmOutputFingerprint','Compare-QdmCompletedOutputs')){
  $qdmDefinitions=@($qdmAst.FindAll({param($qdmNode) $qdmNode -is [Management.Automation.Language.FunctionDefinitionAst]},$false) | Where-Object {$_.Name -eq $qdmFunctionName})
  if($qdmDefinitions.Count -ne 1){throw ('Expected one inventory function: '+$qdmFunctionName)}
  . ([scriptblock]::Create($qdmDefinitions[0].Extent.Text))
}
$qdmTestDirectory=Join-Path $env:TEMP ('qdm-install-output-test-'+[Guid]::NewGuid().ToString('N'))
[void][IO.Directory]::CreateDirectory($qdmTestDirectory)
$qdmTestCount=0
function Assert-QdmOutputTest([bool]$Condition,[string]$Name){
  if(!$Condition){throw ('FAIL '+$Name)}
  $script:qdmTestCount++;Write-Output ('PASS '+$Name)
}
try {
  $qdmTestFile=Join-Path $qdmTestDirectory 'existing.bin'
  [IO.File]::WriteAllText($qdmTestFile,'before')
  $qdmFingerprint=Get-QdmOutputFingerprint -Path $qdmTestFile
  $qdmKnownHash=(Get-FileHash -LiteralPath $qdmTestFile -Algorithm SHA256).Hash.ToLowerInvariant()
  Assert-QdmOutputTest ($qdmFingerprint.present -and $qdmFingerprint.length -eq 6 -and $qdmFingerprint.sha256 -ceq $qdmKnownHash) 'Existing output records exact length and SHA-256'
  $qdmBefore=[pscustomobject]@{taskId='fixture';filename='existing.bin';path=$qdmTestFile;before=$qdmFingerprint}
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBefore))
  Assert-QdmOutputTest ($qdmCompared.Count -eq 1 -and $qdmCompared[0].status -eq 'verified_unchanged') 'Unchanged existing output is verified'
  [IO.File]::WriteAllText($qdmTestFile,'change')
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBefore))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'changed_after_install') 'Same-length content change fails verification'
  [IO.File]::WriteAllText($qdmTestFile,'new')
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBefore))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'changed_after_install') 'Length change fails verification'
  [IO.File]::Delete($qdmTestFile)
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBefore))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'missing_after_install') 'A previously existing output becoming missing fails verification'
  $qdmMissing=Get-QdmOutputFingerprint -Path $qdmTestFile
  Assert-QdmOutputTest (!$qdmMissing.present -and $null -eq $qdmMissing.sha256 -and $null -eq $qdmMissing.length) 'Preexisting absence has no invented length or hash'
  $qdmBeforeMissing=[pscustomobject]@{taskId='missing-fixture';filename='existing.bin';path=$qdmTestFile;before=$qdmMissing}
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBeforeMissing))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'preexisting_missing') 'Continued preexisting absence is explicitly reported'
  [IO.File]::WriteAllText($qdmTestFile,'new')
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBeforeMissing))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'preexisting_missing_now_present') 'A newly appeared file is not called a preserved original'
  $qdmNoPath=Get-QdmOutputFingerprint -Path $null
  Assert-QdmOutputTest (!$qdmNoPath.present -and $null -eq $qdmNoPath.sha256) 'Missing history path is explicit absence'
  [IO.File]::Delete($qdmTestFile)
  [void][IO.Directory]::CreateDirectory($qdmTestFile)
  $qdmCompared=@(Compare-QdmCompletedOutputs -Before @($qdmBefore))
  Assert-QdmOutputTest ($qdmCompared[0].status -eq 'unreadable_after_install' -and $qdmCompared[0].error) 'A directory replacing an output is a read failure, not preexisting absence'
  $qdmRejected=$false
  try {$null=Get-QdmOutputFingerprint -Path $qdmTestFile} catch {$qdmRejected=$true}
  Assert-QdmOutputTest $qdmRejected 'Before-install read failures are not classified as missing files'
  Assert-QdmOutputTest (@(Compare-QdmCompletedOutputs -Before @()).Count -eq 0) 'An empty completed history remains an empty comparison'
  Write-Output (ConvertTo-Json -Compress -InputObject ([ordered]@{status='passed';passed=$qdmTestCount;failed=0}))
} finally {
  if(Test-Path -LiteralPath $qdmTestDirectory){Remove-Item -LiteralPath $qdmTestDirectory -Recurse -Force}
}
