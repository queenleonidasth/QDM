param([switch]$DebugBuild)
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
  & cargo --version
  & rustc --version
  if ($DebugBuild) { & cargo build --locked --bin http-diagnostic } else { & cargo build --release --locked --bin http-diagnostic }
  if ($LASTEXITCODE -ne 0) { throw "QDM build failed: $LASTEXITCODE" }
} finally { Pop-Location }


