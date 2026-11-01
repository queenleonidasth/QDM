from pathlib import Path
root=Path(r'C:\Users\QUEEN\Projects\QDM\native')
p=root/'src/engine/transfer.rs'
s=p.read_text(encoding='utf-8')
def once(old,new,label):
    global s
    n=s.count(old)
    if n!=1 and not (label=='checkpoint_position' and n==2): raise RuntimeError(f'{label}: found {n} occurrences')
    s=s.replace(old,new,1)
once('pub(super) async fn check_local_tail(path: &Path, size: u64, expected: Option<&str>) -> bool {',
     'pub(super) async fn check_local_tail(path: &Path, absolute_end: u64, size: u64, expected: Option<&str>) -> bool {',
     'tail_signature')
once('file.seek(SeekFrom::Start(size - len)).await.ok()?;',
     'file.seek(SeekFrom::Start(absolute_end - len)).await.ok()?;',
     'tail_seek')
once('s.starts_with(&format!(".qdm-{}-", task.view.id)) && s.ends_with(".assembling")',
     's == format!(".qdm-{}.partial", task.view.id)',
     'pending_recover_name')
once('''        fs::create_dir_all(self.inner.data_dir.join("parts-v2").join(id))
            .await
            .map_err(disk)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(self.part_path(id, 1))''',
     '''        // A fallback stream owns the same single temporary file, starting
        // from byte zero. This never reuses suspect Range fragments.
        let path = self.part_path(id, 1);
        fs::create_dir_all(path.parent().unwrap()).await.map_err(disk)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)''',
     'stream_open')
once('file.seek(SeekFrom::Start(position - source.start))',
     'file.seek(SeekFrom::Start(position))',
     'boundary_seek')
# Reimplement finalize: no .part assembly or double-write; hashing from exactly one
# offset-indexed temporary file and committing it by exclusive same-volume rename.
a=s.index('    async fn finalize(&self, id: &str, stop: &mut Stop) -> Result<(), Fault> {')
b=s.index('    fn claim(&self, id: &str, slot: u8)',a)
replacement='''    async fn finalize(&self, id: &str, stop: &mut Stop) -> Result<(), Fault> {
        stop.check()?;
        let mut task = self.task(id).map_err(Fault::Fatal)?;
        task.parts.sort_by_key(|p| p.start);
        let total = task.view.total_bytes
            .ok_or_else(|| Fault::Fatal("The completed length is unavailable".into()))?;
        let mut position = 0;
        for part in &task.parts {
            if part.start != position || part.downloaded != part.len() {
                return Err(Fault::Fatal("The shared-file range layout is incomplete".into()));
            }
            position = part.end;
        }
        if position != total || task.parts.is_empty() {
            return Err(Fault::Fatal("The received length does not match the origin".into()));
        }
        {
            let mut state = lock(&self.inner.state);
            if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                current.view.status = "verifying".into();
                current.view.assembly_bytes = 0;
                current.view.assembly_total_bytes = total;
                current.view.speed_bps = 0;
                current.view.notice = Some("Checking the directly written file and SHA-256...".into());
                current.refresh();
            }
        }
        self.changed();
        self.emit();
        self.verify_boundaries(id, &task.parts, stop).await?;
        self.verify_and_publish(id, &task, total, stop).await
    }

    async fn verify_and_publish(
        &self, id: &str, task: &Task, total: u64, stop: &mut Stop
    ) -> Result<(), Fault> {
        let engine = self.clone();
        let owned_id = id.to_string();
        let partial = self.part_path(id, 0);
        let source = partial.clone();
        let verify_stop = stop.clone();
        let test_delay_ms = if std::env::var("QDM_TEST_MODE").ok().as_deref() == Some("1") {
            std::env::var("QDM_TEST_ASSEMBLY_DELAY_MS").ok()
                .and_then(|v| v.parse::<u64>().ok()).unwrap_or(0).min(30)
        } else { 0 };
        // One sequential READ computes SHA-256, no additional output copy.
        // The original file remains unpublished until the digest is accepted.
        let sha256 = tokio::task::spawn_blocking(move || {
            use std::io::Read;
            let mut file = std::fs::OpenOptions::new().read(true).write(true)
                .open(&source).map_err(disk)?;
            if file.metadata().map_err(disk)?.len() != total {
                return Err(Fault::Fatal("The directly written file has an unexpected length".into()));
            }
            file.sync_all().map_err(disk)?;
            let mut hasher = Sha256::new();
            let mut buffer = vec![0; 1024 * 1024];
            let mut hashed = 0u64;
            let mut reported = 0u64;
            loop {
                verify_stop.check()?;
                let count = file.read(&mut buffer).map_err(disk)?;
                if count == 0 { break; }
                hasher.update(&buffer[..count]);
                hashed += count as u64;
                if test_delay_ms > 0 {
                    std::thread::sleep(Duration::from_millis(test_delay_ms));
                }
                if hashed - reported >= 4 * 1024 * 1024 || hashed == total {
                    {
                        let mut state = lock(&engine.inner.state);
                        if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == owned_id) {
                            current.view.assembly_bytes = hashed.min(total);
                        }
                    }
                    engine.changed();
                    reported = hashed;
                }
            }
            if hashed != total { return Err(Fault::Fatal("File changed while verifying".into())); }
            verify_stop.check()?;
            Ok(format!("{:x}", hasher.finalize()))
        }).await.map_err(|e| Fault::Fatal(format!("Direct file verification stopped: {e}")))??;
        if let Some(expected) = task.view.expected_sha256.as_deref() {
            if expected != sha256 {
                return Err(Fault::Fatal(format!(
                    "SHA-256 verification failed. Expected {expected}, received {sha256}. The final file was not published."
                )));
            }
        }
        stop.check()?;
        let mut published = None;
        for suffix in 0..100_000u32 {
            let filename = collision_filename(&task.view.filename, suffix);
            let path = Path::new(&task.view.directory).join(&filename);
            if fs::try_exists(&path).await.map_err(disk)? { continue; }
            {
                let mut state = lock(&self.inner.state);
                if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                    current.pending_output = Some(PendingOutput {
                        path: path.to_string_lossy().into_owned(),
                        temporary_path: partial.to_string_lossy().into_owned(),
                        filename: filename.clone(),
                        size: total,
                        sha256: sha256.clone(),
                    });
                }
            }
            self.changed();
            self.persist().await.map_err(Fault::Fatal)?;
            // Explicit no-clobber same-volume rename. DB commit record exists
            // before publication, and recover_commit reconciles a crash.
            match promote_exclusive(&partial, &path).await {
                Ok(()) => { published = Some((path, filename)); break; }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    let mut state = lock(&self.inner.state);
                    if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                        current.pending_output = None;
                    }
                    return Err(disk(error));
                }
            }
        }
        let Some((path, filename)) = published else {
            return Err(Fault::Fatal("Too many files already use this filename".into()));
        };
        {
            let mut state = lock(&self.inner.state);
            if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                current.view.filename = filename;
                current.view.file_path = Some(path.to_string_lossy().into_owned());
                current.view.sha256 = Some(sha256);
                current.view.status = "completed".into();
                current.view.completed_at = Some(now_ms());
                current.view.error = None;
                current.view.notice = None;
                current.view.downloaded_bytes = total;
                current.view.assembly_bytes = total;
                current.view.speed_bps = 0;
                current.pending_output = None;
                if current.view.expected_sha256.is_some() {
                    current.view.integrity = "sha256_verified".into();
                }
                current.refresh();
            }
        }
        self.changed();
        self.persist().await.map_err(Fault::Fatal)?;
        self.emit();
        trace_transfer("direct-published", || serde_json::json!({"task":id,"bytes":total}));
        Ok(())
    }

'''
s=s[:a]+replacement+s[b:]
# Writers use independent file handles but absolute offsets into a single file.
once('''        // Discard a partially completed OS write after an I/O failure; only
        // complete writes are represented by downloaded.
        file.set_len(part.downloaded).await.map_err(disk)?;
        file.sync_data().await.map_err(disk)?;''',
     '''        // Never truncate the shared file: other workers own distant offsets.
        // OS synchronization precedes the persisted SQLite checkpoint.
        file.sync_data().await.map_err(disk)?;''',
     'checkpoint_no_trunc')
once('''        file.seek(SeekFrom::Start(part.downloaded - length))''',
     '''        file.seek(SeekFrom::Start(part.start + part.downloaded - length))''',
     'checkpoint_tail')
once('''        file.seek(SeekFrom::Start(part.downloaded))
            .await
            .map_err(disk)?;''',
     '''        file.seek(SeekFrom::Start(part.start + part.downloaded))
            .await
            .map_err(disk)?;''',
     'checkpoint_position')
once('''        file.set_len(part.downloaded).await.map_err(disk)?;
        file.seek(SeekFrom::Start(part.downloaded))''',
     '''        file.seek(SeekFrom::Start(part.start + part.downloaded))''',
     'worker_no_trunc')
once('file.seek(SeekFrom::Start(wire_start - part.start))',
     'file.seek(SeekFrom::Start(wire_start))',
     'resume_overlap_seek')
# One more identical seek inside piece_attempt: no global find because checkpoint already rewritten.
once('''            file.seek(SeekFrom::Start(part.downloaded))
                .await
                .map_err(disk)?;''',
     '''            file.seek(SeekFrom::Start(part.start + part.downloaded))
                .await
                .map_err(disk)?;''',
     'resume_write_seek')
once('''        let directory = self.inner.data_dir.join("parts-v2").join(id);
        match fs::remove_dir_all(&directory).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(disk(error)),
        }
        fs::create_dir_all(&directory).await.map_err(disk)?;''',
     '''        let partial = self.part_path(id, 0);
        match fs::remove_file(&partial).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(disk(error)),
        }''','cleanup_direct')
once('''        fs::create_dir_all(self.inner.data_dir.join("parts-v2").join(id))
            .await
            .map_err(disk)?;
        {
            let mut state = lock(&self.inner.state);
            // Large initial pieces let accepted streams continue productively''',
     '''        // Prepare one sparse, absolutely addressed temporary output on the
        // final file's volume. No extra disk-space copy is needed at completion.
        let partial = self.part_path(id, 0);
        if let Some(parent) = partial.parent() {
            fs::create_dir_all(parent).await.map_err(disk)?;
        }
        let new_file = !fs::try_exists(&partial).await.map_err(disk)?;
        if new_file {
            let file = fs::OpenOptions::new().read(true).write(true)
                .create_new(true).open(&partial).await.map_err(disk)?;
            #[cfg(windows)]
            {
                // Best effort NTFS sparse hint prevents allocating blank
                // segments before a worker actually receives their data.
                let p = partial.clone();
                let _ = tokio::task::spawn_blocking(move || {
                    use std::os::windows::io::AsRawHandle;
                    #[link(name = "kernel32")]
                    extern "system" {
                        fn DeviceIoControl(handle: *mut core::ffi::c_void, control: u32,
                            input: *mut core::ffi::c_void, input_len: u32,
                            output: *mut core::ffi::c_void, output_len: u32,
                            returned: *mut u32, overlapped: *mut core::ffi::c_void) -> i32;
                    }
                    if let Ok(f) = std::fs::OpenOptions::new().write(true).open(p) {
                        let mut returned = 0u32;
                        unsafe {
                            DeviceIoControl(f.as_raw_handle(), 0x900c4,
                                std::ptr::null_mut(), 0, std::ptr::null_mut(), 0,
                                &mut returned, std::ptr::null_mut());
                        }
                    }
                }).await;
            }
            file.set_len(total).await.map_err(disk)?;
            file.sync_data().await.map_err(disk)?;
        } else {
            let file = fs::OpenOptions::new().write(true).open(&partial).await.map_err(disk)?;
            if file.metadata().await.map_err(disk)?.len() != total {
                file.set_len(total).await.map_err(disk)?;
                file.sync_data().await.map_err(disk)?;
            }
        }
        {
            let mut state = lock(&self.inner.state);
            // Large initial pieces let accepted streams continue productively''',
     'ensure_shared')
once('file.seek(SeekFrom::Start(offset)).await.map_err(disk)?;',
     'file.seek(SeekFrom::Start(part.start + offset)).await.map_err(disk)?;',
     'validate_saved_offset')
p.write_text(s,encoding='utf-8')
print('Direct-write shared-file worker, pause, checkpoint and hash-only publish edited.')
