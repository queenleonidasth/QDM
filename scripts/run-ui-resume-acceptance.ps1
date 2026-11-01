$ErrorActionPreference = 'Stop'
$root='C:\Users\QUEEN\Projects\QDM\native'
$exe=Join-Path $root 'target\release\QDM.exe'
$node='C:\Program Files\nodejs\node.exe'
if(-not (Test-Path $exe)){throw "Missing exe: $exe"}
if(-not (Test-Path $node)){throw "Missing Node binary: $node"}
$env:QDM_TEST_MODE='1'
$env:QDM_TEST_ASSEMBLY_DELAY_MS='25'
& (Join-Path $root 'tests\native-ui-smoke.ps1') -Binary $exe -NodeBinary $node -OutputDirectory (Join-Path $root 'docs\evidence\resume-monitor-ui')
if($LASTEXITCODE -ne 0){exit $LASTEXITCODE}