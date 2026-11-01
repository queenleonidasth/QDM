from pathlib import Path
root=Path(r'C:\Users\QUEEN\Projects\QDM\native')
p=root/'src/ui/slint_ui.rs'
s=p.read_text(encoding='utf-8')
def rep(old,new):
    global s
    if old not in s: raise ValueError(old[:90])
    s=s.replace(old,new,1)
rep('let stage_indeterminate = task.status == "verifying" || task.assembly_total_bytes == 0;',
    'let stage_indeterminate = task.assembly_total_bytes == 0;')
rep('format!("MERGE {:.0}%", stage_progress * 100.0)', 'format!("CHECK {:.0}%", stage_progress * 100.0)')
rep('ui.set_stage_indeterminate(task.status == "verifying" || task.assembly_total_bytes == 0);',
    'ui.set_stage_indeterminate(task.assembly_total_bytes == 0);')
rep('ui.set_stage_label(if task.status == "verifying" { "VERIFYING PARTS" } else { "COMBINING FILE" }.into());',
    'ui.set_stage_label(if task.assembly_total_bytes == 0 { "VERIFYING SAVED DATA" } else { "CHECKING SHA-256" }.into());')
rep('ui.set_stage_bytes(if task.status == "verifying" { "Checking saved data".into() }', 'ui.set_stage_bytes(if task.assembly_total_bytes == 0 { "Checking saved data".into() }')
rep('else if task.assembly_total_bytes == 0 { "Preparing file assembly".into() }\n            else {', 'else {')
p.write_text(s,encoding='utf-8')
print('Status UI now displays true direct-file SHA verification progress')
