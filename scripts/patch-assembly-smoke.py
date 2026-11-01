from pathlib import Path
r=Path(r'C:\Users\QUEEN\Projects\QDM\native')
p=r/'scripts/run-ui-resume-acceptance.ps1'
s=p.read_text(encoding='utf8')
old="& (Join-Path $root 'tests\\native-ui-smoke.ps1')"
new="$env:QDM_TEST_MODE='1'\n$env:QDM_TEST_ASSEMBLY_DELAY_MS='25'\n& (Join-Path $root 'tests\\native-ui-smoke.ps1')"
assert s.count(old)==1
s=s.replace(old,new,1)
p.write_text(s,encoding='utf8')
p=r/'tests/native-ui-smoke.ps1'
s=p.read_text(encoding='utf8')
old='''  Click-Control 1006
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1007)) } 'completed download Open action' 60000)'''
new='''  Click-Control 1006
  $mergeStage = Wait-Until {
    $state = Read-IsolatedState
    if ($null -eq $state) { return $null }
    $activeJob = @($state.tasks | Where-Object { $_.view.id -eq $savedTask.id } | Select-Object -First 1)
    if ($activeJob.Count -eq 0) { return $null }
    $view = $activeJob[0].view
    if ($view.status -ne 'assembling' -or $view.assembly_total_bytes -le 0) { return $null }
    $progress = Progress-Value
    if ($view.assembly_bytes -gt 0 -and $view.assembly_bytes -lt $view.assembly_total_bytes -and $progress -gt 0 -and $progress -lt 10000) {
      return @{ bytes = $view.assembly_bytes; total = $view.assembly_total_bytes; progress = $progress }
    }
  } 'visible merge progress separate from completed download' 45000
  Step-Passed 'physical file assembly has its own progress bar' @{
      bytes = $mergeStage.bytes; total = $mergeStage.total;
      progress = $mergeStage.progress; screenshot = (Capture-Window '06a-assembling')
  }
  [void](Wait-Until { [QdmUiSmokeNative]::IsWindowEnabled((Control 1007)) } 'completed download Open action' 60000)'''
assert s.count(old)==1
s=s.replace(old,new,1)
p.write_text(s,encoding='utf8')
print('GUI smoke now captures live intermediate assembly bar without touching real user data.')
