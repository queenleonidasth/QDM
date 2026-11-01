from pathlib import Path
r=Path(r'C:\Users\QUEEN\Projects\QDM\native\src')
def sub(s,a,b,label):
 if s.count(a)!=1: raise Exception(f'{label}: {s.count(a)} matches')
 return s.replace(a,b,1)
p=r/'engine/mod.rs';s=p.read_text(encoding='utf-8')
s=sub(s,'    adaptive_limit: u8,\n','    adaptive_limit: u8,\n    #[serde(skip)]\n    adaptive_last_change: Option<Instant>,\n    #[serde(skip)]\n    adaptive_healthy_bytes: u64,\n','fields')
s=sub(s,'                    adaptive_limit: 0,\n','                    adaptive_limit: 0,\n                    adaptive_last_change: None,\n                    adaptive_healthy_bytes: 0,\n','initializer')
p.write_text(s,encoding='utf-8')
p=r/'engine/transfer.rs';s=p.read_text(encoding='utf-8')
s=sub(s,'''                                task.adaptive_limit = new_limit;
                                task.view.notice''','''                                task.adaptive_limit = new_limit;
                                task.adaptive_last_change = Some(Instant::now());
                                task.adaptive_healthy_bytes = 0;
                                task.view.notice''','429 reset')
s=sub(s,'''            task.view.network_bytes = task.view.network_bytes.saturating_add(bytes);
        }
    }

    async fn throttle''','''            task.view.network_bytes = task.view.network_bytes.saturating_add(bytes);
            let configured = state.settings.connections; 
        }
    }

    async fn throttle''','intermediate') if False else s
s=sub(s,'''        if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
            task.view.network_bytes = task.view.network_bytes.saturating_add(bytes);
        }
    }

    async fn throttle''','''        let configured = state.settings.connections;
        if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
            task.view.network_bytes = task.view.network_bytes.saturating_add(bytes);
            if task.view.status == "downloading" && task.adaptive_limit > 0
                && task.adaptive_limit < configured {
                task.adaptive_healthy_bytes = task.adaptive_healthy_bytes.saturating_add(bytes);
                let elapsed = task.adaptive_last_change
                    .get_or_insert_with(Instant::now).elapsed();
                // Additive increase after sustained good traffic. A renewed
                // HTTP 429 halves the limit immediately (multiplicative decrease).
                if elapsed >= Duration::from_secs(20)
                    && task.adaptive_healthy_bytes >= 12 * 1024 * 1024 {
                    let step = (task.adaptive_limit / 4).max(1);
                    task.adaptive_limit = task.adaptive_limit.saturating_add(step).min(configured);
                    task.adaptive_last_change = Some(Instant::now());
                    task.adaptive_healthy_bytes = 0;
                    task.view.notice = Some(format!(
                        "Stable transfer: testing {} connections (up to {})",
                        task.adaptive_limit, configured));
                    self.inner.work_changed.notify_waiters();
                }
            }
        }
    }

    async fn throttle''','ramp')
p.write_text(s,encoding='utf-8')
p=r/'ui/windows.rs';s=p.read_text(encoding='utf-8')
s=s.replace(r'\\r\\n',r'\r\n') # ensure Win32 edit actual CRLF
s=sub(s,'    remove: HWND,\n','    remove: HWND,\n    monitor: HWND,\n','monitor control')
s=sub(s,'        self.controls.remove = self.button(self.hwnd,','        self.controls.monitor = self.button(self.hwnd, "ท่อ / Connections", ID_DETAILS);\n        self.controls.remove = self.button(self.hwnd,','create monitor')
s=sub(s,'            self.controls.remove,\n            self.controls.detail,','            self.controls.remove,\n            self.controls.monitor,\n            self.controls.detail,','font monitor')
s=sub(s,'            (self.controls.remove, 96),\n','            (self.controls.remove, 96),\n            (self.controls.monitor, 136),\n','layout monitor')
s=sub(s,'        EnableWindow(\n            self.controls.resume,','        EnableWindow(self.controls.monitor, selected.is_some() as i32);\n        EnableWindow(\n            self.controls.resume,','enable monitor')
# Text clarity + monospace edit font optional later
p.write_text(s,encoding='utf-8')
print('patched automatic recovery and visible monitor')
