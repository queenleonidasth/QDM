param(
  [string]$Binary = '',
  [string]$EvidenceDirectory = 'docs\evidence\slint-idm-20261027',
  [string]$OutputDirectory = '',
  [switch]$SkipInstaller
)
$ErrorActionPreference='Stop'
$qdmRoot=Split-Path -Parent $PSScriptRoot
if(!$Binary){$Binary=Join-Path $qdmRoot 'target\release\QDM.exe'}
$Binary=[IO.Path]::GetFullPath($Binary)
$qdmVersionMatch=[regex]::Match((Get-Content -LiteralPath (Join-Path $qdmRoot 'Cargo.toml') -Raw), '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"\s*$')
if(!$qdmVersionMatch.Success){throw 'Could not determine the package version from Cargo.toml.'}
$qdmVersion=$qdmVersionMatch.Groups[1].Value
$qdmRelease=if($OutputDirectory){[IO.Path]::GetFullPath($OutputDirectory)}else{Join-Path $qdmRoot 'release'}
$qdmStage=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-portable')
$qdmZip=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-win-x64-portable.zip')
$qdmSetup=Join-Path $qdmRelease ('QDM-'+$qdmVersion+'-win-x64-setup.exe')
$qdmManifestPath=Join-Path $qdmRelease ('manifest-'+$qdmVersion+'.json')
$qdmEvidence=if([IO.Path]::IsPathRooted($EvidenceDirectory)){[IO.Path]::GetFullPath($EvidenceDirectory)}else{Join-Path $qdmRoot $EvidenceDirectory}
$qdmHash=(Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant()
# Package only completed, passing evidence for this exact executable.
. (Join-Path $PSScriptRoot 'release-gates.ps1')
$qdmAcceptedGates=@(Assert-QdmReleaseGates -EvidenceDirectory $qdmEvidence -BinarySha256 $qdmHash)
foreach($qdmOutput in @($qdmStage,$qdmZip,$qdmSetup,$qdmManifestPath)){
  if(Test-Path -LiteralPath $qdmOutput){throw ('Refusing to overwrite existing packaging output: '+$qdmOutput)}
}
New-Item -ItemType Directory -Path $qdmStage -Force | Out-Null
Copy-Item -LiteralPath $Binary -Destination (Join-Path $qdmStage 'QDM.exe') -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'README.md') -Destination $qdmStage -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'extensions') -Destination (Join-Path $qdmStage 'extensions') -Recurse -Force
Copy-Item -LiteralPath (Join-Path $qdmRoot 'docs\third-party') -Destination (Join-Path $qdmStage 'third-party') -Recurse -Force
if(!$SkipInstaller){
  $qdmNsis=Join-Path $env:LOCALAPPDATA 'electron-builder\Cache\nsis-3.0.4.1\nsis-3.0.4.1-1mx3n\Bin\makensis.exe'
  if(!(Test-Path -LiteralPath $qdmNsis)){throw 'The verified NSIS compiler is unavailable.'}
  Push-Location (Join-Path $qdmRoot 'scripts')
  # installer.nsi is UTF-8 without a BOM. Unicode output does not select the
  # source-file codepage; specify it before the script argument explicitly.
  try { & $qdmNsis /V2 /INPUTCHARSET UTF8 "/DQDM_VERSION=$qdmVersion" "/DQDM_BINARY=$Binary" "/DQDM_SETUP=$qdmSetup" installer.nsi; if($LASTEXITCODE -ne 0){throw 'NSIS packaging failed.'} }
  finally {Pop-Location}
  if((Get-FileHash -LiteralPath $Binary -Algorithm SHA256).Hash.ToLowerInvariant() -ne $qdmHash){throw 'Executable changed during installer packaging.'}
}
# Upstream license archives can carry pre-1980 timestamps. ZIP's DOS date
# range is narrower; normalize only the owned staging copy, never the source.
Get-ChildItem -LiteralPath $qdmStage -Recurse -Force | Where-Object {$_.LastWriteTime.Year -lt 1980 -or $_.LastWriteTime.Year -gt 2107} | ForEach-Object {$_.LastWriteTime=[DateTime]::new(1980,1,1,12,0,0)}
Compress-Archive -LiteralPath $qdmStage -DestinationPath $qdmZip -CompressionLevel Optimal -Force
$qdmManifest=@()
$qdmPackagePaths=@($Binary,$qdmZip)
if(!$SkipInstaller){$qdmPackagePaths+=$qdmSetup}
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
$qdmManifest | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $qdmManifestPath -Encoding UTF8
$qdmManifest | ConvertTo-Json -Depth 4
