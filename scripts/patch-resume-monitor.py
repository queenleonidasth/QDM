from pathlib import Path
root=Path(r'C:\Users\QUEEN\Projects\QDM\native\src')
mod=root/'engine/mod.rs'
transfer=root/'engine/transfer.rs'
ui=root/'ui/windows.rs'
def replace_one(s,old,new,tag):
    n=s.count(old)
    if n!=1: raise RuntimeError(f'{tag}: expected 1 match, found {n}')
    return s.replace(old,new,1)
s=mod.read_text(encoding='utf-8')
s=replace_one(s,'    sampled_at: Option<Instant>,\n','    sampled_at: Option<Instant>,\n    #[serde(skip)]\n    adaptive_limit: u8,\n','task runtime cap')
mod.write_text(s,encoding='utf-8')
s=transfer.read_text(encoding='utf-8')
s=replace_one(s,'''        let limit = state.settings.connections as usize;
        let task = state''','''        let user_limit = state.settings.connections as usize;
        let task = state''','claim settings')
s=replace_one(s,'''        if task.parts.iter().all(|p| p.downloaded >= p.len()) {
            return Ok(Claim::Done);
        }
        if task.parts.iter().filter(|p| p.active).count() >= limit {''','''        if task.parts.iter().all(|p| p.downloaded >= p.len()) {
            return Ok(Claim::Done);
        }
        let limit = if task.adaptive_limit == 0 { user_limit } else { user_limit.min(task.adaptive_limit as usize) };
        if task.parts.iter().filter(|p| p.active).count() >= limit {''','claim cap')
s=replace_one(s,'''                match self.run_ranges(id, &meta, task_stop).await {
                    Ok(()) => {}
                    Err(Fault::Restart(reason)) => {
                        self.clean_parts(id, Some(&reason)).await?;
                        self.run_stream(id, None, &mut stop).await?;
                    }
                    Err(error) => return Err(error),
                }''','''                // HTTP 429 commonly indicates a per-origin connection limit.
                // Keep the durable parts and progressively decrease concurrency
                // instead of failing the whole task or discarding progress.
                let mut backoffs = 0u8;
                loop {
                    match self.run_ranges(id, &meta, task_stop.clone()).await {
                        Ok(()) => break,
                        Err(Fault::Retry(reason, delay)) if reason.contains("HTTP 429") && backoffs < 6 => {
                            backoffs += 1;
                            let new_limit = {
                                let mut state = lock(&self.inner.state);
                                let global = state.settings.connections;
                                let task = state.tasks.iter_mut().find(|t| t.view.id == id)
                                    .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
                                let previous = if task.adaptive_limit == 0 { global } else { task.adaptive_limit };
                                let new_limit = (previous / 2).max(1);
                                task.adaptive_limit = new_limit;
                                task.view.notice = Some(format!("Server HTTP 429: reducing connections to {new_limit} and retrying safely"));
                                new_limit
                            };
                            self.changed();
                            // Checkpointed pieces are retained and revalidated by ranged requests.
                            self.delay_retry(id, backoffs, &format!("HTTP 429; {new_limit} connections"), delay, &mut stop).await?;
                        }
                        Err(Fault::Restart(reason)) => {
                            self.clean_parts(id, Some(&reason)).await?;
                            self.run_stream(id, None, &mut stop).await?;
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }''','429 adaptive retry')
transfer.write_text(s,encoding='utf-8')
s=ui.read_text(encoding='utf-8')
s=replace_one(s,'''    Details,
}''','''    Details { id: String, segments: HWND },
}''','details kind')
s=replace_one(s,'''        self.snapshot = snapshot;
        if rebuild {''','''        // Refresh the per-connection monitor on every engine snapshot.
        if let Some(aux) = self.aux.as_ref() {
            if let AuxKind::Details { id, segments } = &aux.kind {
                if let Some(task) = snapshot.downloads.iter().find(|t| &t.id == id) {
                    let text = segment_monitor_text(task);
                    if get_text(*segments) != text {
                        SetWindowTextW(*segments, wide(&text).as_ptr());
                    }
                }
            }
        }
        self.snapshot = snapshot;
        if rebuild {''','detail live refresh')
start=s.index('    unsafe fn details(&mut self, task: &TaskView) {')
end=s.index('    unsafe fn aux_action(&mut self, id: usize) {',start)
segment_fn='''    unsafe fn details(&mut self, task: &TaskView) {
        let Some(hwnd) = self.begin_aux("QDM - Connection Monitor", 850, 720) else { return; };
        let summary = format!(
            "{} | {} | {} / {} | {} active / {} configured | {:.1} MiB/s",
            task.filename,
            state_name(&task.status),
            bytes(task.downloaded_bytes),
            task.total_bytes.map(bytes).unwrap_or_else(|| "?".into()),
            task.connections_active,
            task.connections_limit,
            task.speed_bps as f64 / 1_048_576.0
        );
        let headline = self.label(hwnd, &summary, 1700);
        self.place(headline, 18, 14, 806, 42);
        let monitor = self.edit(
            hwnd,
            &segment_monitor_text(task),
            1702,
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_READONLY as u32 | WS_VSCROLL
        );
        self.place(monitor, 18, 62, 806, 566);
        let hint = self.label(hwnd, "Each connection: start-end | transferred | progress | state. Live updates while the download runs.", 1703);
        self.place(hint, 18, 636, 806, 28);
        let close = self.button(hwnd, "Close", ID_AUX_CANCEL);
        self.place(close, 722, 668, 102, 32);
        self.aux = Some(Aux {
            hwnd, dpi: self.dpi, font: null_mut(),
            kind: AuxKind::Details { id: task.id.clone(), segments: monitor },
        });
        self.finish_aux(hwnd, monitor);
    }

'''
s=s[:start]+segment_fn+s[end:]
# function before App struct, no borrow dependencies
marker='struct App {\n'
fmt='''fn segment_monitor_text(task: &TaskView) -> String {
    let mut output = format!("{} connections | {} active | {} bytes downloaded\\\\r\\\\n",
        task.segments.len(), task.connections_active, task.downloaded_bytes);
    if task.segments.is_empty() {
        output.push_str("Waiting for connection information...");
    }
    for segment in &task.segments {
        let size = segment.end.saturating_sub(segment.start);
        let transferred = segment.downloaded_bytes.min(size);
        let percentage = if size == 0 { 100 } else { ((transferred as f64 / size as f64) * 100.0).round() as u32 };
        let filled = (percentage.min(100) as usize * 18) / 100;
        let bar = format!("{}{}", "#".repeat(filled), "-".repeat(18-filled));
        let state = if size > 0 && transferred >= size { "done" } else if segment.active { "receiving" } else { "waiting" };
        output.push_str(&format!("{:>3} [{}] {:>3}%  {:>10} / {:<10}  {:<10}  [{:>12} .. {:>12}]\\\\r\\\\n",
            segment.id, bar, percentage, bytes(transferred), bytes(size), state, segment.start, segment.end.saturating_sub(1)));
    }
    output
}

'''
s=replace_one(s,marker,fmt+marker,'monitor function')
ui.write_text(s,encoding='utf-8')
print('PATCHED ENGINE AND NATIVE UI')
