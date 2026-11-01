param(
  [string]$Binary = '',
  [string]$EvidenceDirectory = 'docs\evidence\slint-idm-20261027',
  [switch]$SkipInstaller
)
$ErrorActionPreference='Stop'
$qdmRoot=Split-Path -Parent $PSScriptRoot
if(!$Binary){$Binary=Join-Path $qdmRoot 'target\release\QDM.exe'}
$Binary=[IO.Path]::GetFullPath($Binary)
$qdmVersion='0.3.0'
$qdmRelease=Join-Path $qdmRoot 'release'
$qdmStage=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-portable')
$qdmEvidence=Join-Path $qdmRoot $EvidenceDirectory
$qdmHash=(Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant()
# Package only completed, passing evidence for this exact executable.
. (Join-Path $PSScriptRoot 'release-gates.ps1')
$qdmAcceptedGates=@(Assert-QdmReleaseGates -EvidenceDirectory $qdmEvidence -BinarySha256 $qdmHash)
if(Test-Path -LiteralPath $qdmStage){Remove-Item -LiteralPath $qdmStage -Recurse -Force}
New-Item -ItemType Directory -Path $qdmStage -Force | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $qdmStage 'QDM.exe') -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'README.md') -Destination $qdmStage -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'extensions') -Destination (Join-Path $qdmStage 'extensions') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'docs\third-party') -Destination (Join-Path $qdmStage 'third-party') -Recurse -Force
if(!$SkipInstaller){
  $qdmNsis=Join-Path $env:LOCALAPPDATA 'electron-builder\Cache\nsis-3.0.4.1\nsis-3.0.4.1-1mx3n\Bin\makensis.exe'
  if(!(Test-Path -LiteralPath $qdmNsis)){throw 'The verified NSIS compiler is unavailable.'}
  if((Get-FileHash -LiteralPath (Join-Path $qdmRoot 'target\release\QDM.exe')).Hash.ToLowerInvariant() -ne $qdmHash){throw 'NSIS source binary differs from accepted binary.'}
  Push-Location (Join-Path $qdmRoot 'scripts')
  # installer.nsi is UTF-8 without a BOM. Unicode output does not select the
  # source-file codepage; specify it before the script argument explicitly.
  try { & $qdmNsis /V2 /INPUTCHARSET UTF8 installer.nsi; if($LASTEXITCODE -ne 0){throw 'NSIS packaging failed.'} }
  finally {Pop-Location}
}
$qdmZip=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-win-x64-portable.zip')
# Upstream license archives can carry pre-1980 timestamps. ZIP's DOS date
# range is narrower; normalize only the owned staging copy, never the source.
Get-ChildItem -LiteralPath $qdmStage -Recurse -Force | Where-Object {$_.LastWriteTime.Year -lt 1980 -or $_.LastWriteTime.Year -gt 2107} | ForEach-Object {$_.LastWriteTime=[DateTime]::new(1980,1,1,12,0,0)}
Compress-Archive -LiteralPath $qdmStage -DestinationPath $qdmZip -CompressionLevel Optimal -Force
$qdmManifest=@()
$qdmPackagePaths=@($Binary,$qdmZip)
if(!$SkipInstaller){$qdmPackagePaths+=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-win-x64-setup.exe')}
foreach($qdmFile in $qdmPackagePaths){
  $qdmManifest += [pscustomobject]@{name=[IO.Path]::GetFileName($qdmFile);bytes=(Get-Item -LiteralPath $qdmFile).Length;sha256=(Get-FileHash -LiteralPath $qdmFile -Algorithm SHA256).Hash.ToLowerInvariant()}
}
$qdmCheckDir=Join-Path $env:TEMP ('qdm-slint-package-'+[Guid]::NewGuid().ToString('N'))
try {
  Expand-Archive -LiteralPath $qdmZip -DestinationPath $qdmCheckDir
  $qdmCheckExe=Join-Path $qdmCheckDir ('QDM-'+$qdmVersion+'-portable\QDM.exe')
  if((Get-FileHash -LiteralPath $qdmCheckExe).Hash.ToLowerInvariant() -ne $qdmHash){throw 'Portable extracted executable hash differs.'}
  $qdmVersionPath=Join-Path $qdmCheckDir 'version.txt'
  $qdmVersionProc=Start-Process -FilePath $qdmCheckExe -ArgumentList '--version' -RedirectStandardOutput $qdmVersionPath -PassThru -Wait
  $qdmVersionText=(Get-Content -LiteralPath $qdmVersionPath -Raw).Trim()
  if($qdmVersionProc.ExitCode -ne 0 -or $qdmVersionText -ne ('QDM '+$qdmVersion)){throw 'Portable version check failed.'}
  [ordered]@{status='passed';version=$qdmVersionText;binarySha256=$qdmHash;verifiedAt=[DateTime]::UtcNow.ToString('o');nsisInputCharset=$(if($SkipInstaller){$null}else{'UTF8'});releaseGates=$qdmAcceptedGates;artifacts=$qdmManifest} | ConvertTo-Json -Depth 6 | Set-Content -LiteralPath (Join-Path $qdmEvidence 'packaging.json') -Encoding UTF8
} finally {if(Test-Path -LiteralPath $qdmCheckDir){Remove-Item -LiteralPath $qdmCheckDir -Recurse -Force}}
$qdmManifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath (Join-Path $qdmRelease 'manifest-0.3.0.json') -Encoding UTF8
$qdmManifest | ConvertTo-Json -Depth 4
