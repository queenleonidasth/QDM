$ErrorActionPreference='Stop'
$qdmRoot='C:\Users\QUEEN\Projects\QDM\native'
Set-Location $qdmRoot
$qdmStage=Join-Path $qdmRoot 'release\QDM-0.2.0-portable'
Copy-Item (Join-Path $qdmRoot 'target\release\QDM.exe') (Join-Path $qdmStage 'QDM.exe') -Force
$qdmZip=Join-Path $qdmRoot 'release\QDM-0.2.0-win-x64-portable.zip'
Compress-Archive -LiteralPath $qdmStage -DestinationPath $qdmZip -CompressionLevel Optimal -Force
$qdmManifest=@()
foreach($qdmFile in @((Join-Path $qdmRoot 'target\release\QDM.exe'),(Join-Path $qdmRoot 'release\QDM-0.2.0-win-x64-setup.exe'),$qdmZip)){
 $qdmManifest += [pscustomobject]@{name=[IO.Path]::GetFileName($qdmFile);path=$qdmFile;bytes=(Get-Item $qdmFile).Length;sha256=(Get-FileHash $qdmFile -Algorithm SHA256).Hash}
}
$qdmManifest | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $qdmRoot 'release\manifest.json')
$qdmExtract=Join-Path $env:TEMP ('qdm-portable-verify-'+[Guid]::NewGuid().ToString('N'))
New-Item -ItemType Directory -Path $qdmExtract | Out-Null
try {
 Expand-Archive -LiteralPath $qdmZip -DestinationPath $qdmExtract
 $qdmExe=Join-Path $qdmExtract 'QDM-0.2.0-portable\QDM.exe'
 $qdmSha=(Get-FileHash $qdmExe -Algorithm SHA256).Hash
 if($qdmSha -ne $qdmManifest[0].sha256){throw 'Portable checksum mismatch'}
 $qdmStdout=Join-Path $qdmExtract 'version.txt'
 $qdmCheck=Start-Process -FilePath $qdmExe -ArgumentList '--version' -RedirectStandardOutput $qdmStdout -PassThru -Wait
 $qdmVersion=(Get-Content -Raw $qdmStdout).Trim()
 if($qdmCheck.ExitCode -ne 0 -or $qdmVersion -ne 'QDM 0.2.0'){throw 'Portable version check failed'}
 [ordered]@{verifiedAt=[DateTime]::UtcNow.ToString('o');archive='QDM-0.2.0-win-x64-portable.zip';binarySha256=$qdmSha;version=$qdmVersion;thirdPartyFiles=@(Get-ChildItem (Join-Path $qdmExtract 'QDM-0.2.0-portable\third-party') -Recurse -File).Count;status='passed'} | ConvertTo-Json | Set-Content -Encoding UTF8 (Join-Path $qdmRoot 'docs\evidence\portable-package.json')
} finally {Remove-Item -LiteralPath $qdmExtract -Recurse -Force}
& node scripts\source-hashes.cjs
if($LASTEXITCODE -ne 0){throw 'Source manifest failed'}
$qdmManifest | ConvertTo-Json
exit 0
