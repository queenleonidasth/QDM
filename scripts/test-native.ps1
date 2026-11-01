param([switch]$DebugBuild,[string]$ReportPath='docs\evidence\slint-idm-20261027\native-tests.json')
$ErrorActionPreference = 'Stop'
$qdmRoot = Split-Path -Parent $PSScriptRoot
$qdmParent = Split-Path -Parent $qdmRoot
$qdmTools = Join-Path $qdmParent '.tools\rust'
if (Test-Path (Join-Path $qdmTools 'cargo\bin\cargo.exe')) {
  $env:CARGO_HOME = Join-Path $qdmTools 'cargo'
  $env:RUSTUP_HOME = Join-Path $qdmTools 'rustup'
  $env:Path = (Join-Path $env:CARGO_HOME 'bin') + ';' + $env:Path
}
$qdmVswhere = Join-Path ${env:ProgramFiles(x86)} 'Microsoft Visual Studio\Installer\vswhere.exe'
$qdmVcvars = 'C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat'
if (Test-Path $qdmVswhere) {
  $qdmVsPath = & $qdmVswhere -latest -products '*' -requires Microsoft.VisualStudio.Component.VC.Tools.x86.x64 -property installationPath
  if ($qdmVsPath) { $qdmVcvars = Join-Path $qdmVsPath 'VC\Auxiliary\Build\vcvars64.bat' }
}
if (-not (Test-Path $qdmVcvars)) { throw 'Install Microsoft C++ Build Tools with the Windows SDK before building QDM.' }
$qdmEnvironment = & $env:ComSpec /d /s /c ('"' + $qdmVcvars + '" >nul && set')
foreach ($qdmLine in $qdmEnvironment) {
  if ($qdmLine -match '^([^=]+)=(.*)$') { [Environment]::SetEnvironmentVariable($matches[1],$matches[2],'Process') }
}
Push-Location $qdmRoot
try {
  $qdmBinary=Join-Path $qdmRoot 'target\release\QDM.exe'
  $qdmBeforeHash=(Get-FileHash -LiteralPath $qdmBinary -Algorithm SHA256).Hash.ToLowerInvariant()
  $qdmReportFull=[IO.Path]::GetFullPath($ReportPath)
  New-Item -ItemType Directory -Force -Path (Split-Path -Parent $qdmReportFull) | Out-Null
  $qdmLog=[IO.Path]::ChangeExtension($qdmReportFull,'.log')
  $qdmStarted=[DateTime]::UtcNow.ToString('o')
  & cargo --version
  & rustc --version
  $ErrorActionPreference='Continue'
  if ($DebugBuild) { & cargo test --locked --bin QDM -- --nocapture 2>&1 | Tee-Object -FilePath $qdmLog } else { & cargo test --release --locked --bin QDM -- --nocapture 2>&1 | Tee-Object -FilePath $qdmLog }
  $qdmCode=$LASTEXITCODE
  $ErrorActionPreference='Stop'
  $qdmText=Get-Content -LiteralPath $qdmLog -Raw
  $qdmMatch=[regex]::Match($qdmText,'test result: ok\. (\d+) passed; 0 failed;')
  $qdmAfterHash=(Get-FileHash -LiteralPath $qdmBinary -Algorithm SHA256).Hash.ToLowerInvariant()
  $qdmOk=($qdmCode -eq 0 -and $qdmMatch.Success -and $qdmBeforeHash -eq $qdmAfterHash)
  [ordered]@{schema=1;suite='Rust native protocol, ownership, rollback and history unit tests';status=$(if($qdmOk){'passed'}else{'failed'});startedAt=$qdmStarted;finishedAt=[DateTime]::UtcNow.ToString('o');binarySha256=$qdmBeforeHash;binarySha256After=$qdmAfterHash;exitCode=$qdmCode;totalTests=$(if($qdmMatch.Success){[int]$qdmMatch.Groups[1].Value}else{0});log=$qdmLog} | ConvertTo-Json | Set-Content -LiteralPath $qdmReportFull -Encoding UTF8
  if (!$qdmOk) { throw "QDM native tests failed: $qdmCode. See $qdmLog" }
} finally { Pop-Location }


