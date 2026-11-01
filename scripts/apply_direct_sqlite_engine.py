# Safe anchor-driven code transformation of the QDM 0.3.1 engine to SQLite + direct writes.
from pathlib import Path
root=Path(r'C:\Users\QUEEN\Projects\QDM\native')
p=root/'src/engine/mod.rs'
s=p.read_text(encoding='utf-8')
def once(s,old,new,label):
    n=s.count(old)
    if n!=1: raise RuntimeError(f'{label} count={n}')
    return s.replace(old,new,1)
s=once(s,'mod transfer;\n','mod transfer;\nmod store;\n','module')
s=once(s,'//! QDM\'s original native download engine. Each piece has an exclusive byte end.','//! QDM native engine: independent checked byte ownership in a single positioned-write file.','header')
start=s.index('        fs::create_dir_all(data_dir.join("parts-v2"))')
end=s.index('        let (events, _) = broadcast::channel(16);',start)
s=s[:start]+'''
        fs::create_dir_all(&data_dir).await.map_err(|e| e.to_string())?;
        fs::create_dir_all(&download_dir).await.map_err(|e| e.to_string())?;
        let mut settings = validated_settings(Settings::default(), &download_dir)?;
        let mut tasks = Vec::new();
        let mut warning = None;
        let db = data_dir.join("state-v3.sqlite3");
        let exists = fs::try_exists(&db).await.map_err(|e| e.to_string())?;
        // Import only completed records from v2: the user authorized discarding
        // all old partial formats and failed jobs, but published files remain.
        let saved = if exists {
            store::load(db.clone()).await?
        } else {
            let legacy_path = data_dir.join("state-v2.json");
            if fs::metadata(&legacy_path).await.map(|m| m.len() <= 32 * 1024 * 1024).unwrap_or(false) {
                fs::read(&legacy_path).await.ok()
                    .and_then(|bytes| serde_json::from_slice::<Saved>(&bytes).ok())
            } else { None }
        };
        if let Some(saved) = saved {
            if !matches!(saved.schema_version, 2 | 3) {
                return Err(format!("Unsupported QDM state database schema {}", saved.schema_version));
            }
            settings = validated_settings(saved.settings, &download_dir)?;
            let mut ids = HashSet::new();
            for mut task in saved.tasks.into_iter().take(HISTORY_LIMIT) {
                if Uuid::parse_str(&task.view.id).is_err()
                    || !ids.insert(task.view.id.clone())
                    || validate_url(&task.view.url).is_err()
                    || !Path::new(&task.view.directory).is_absolute()
                { continue; }
                if !exists && task.view.status != "completed" { continue; }
                task.view.filename = safe_filename(&task.view.filename);
                task.view.connections_limit = settings.connections;
                task.view.connections_active = 0;
                task.view.speed_bps = 0;
                task.headers.clear();
                transfer::recover_commit(&mut task).await;
                if !matches!(task.view.status.as_str(), "completed" | "paused" | "queued" | "scheduled" | "error") {
                    task.view.status = "paused".into();
                }
                // The old .part chunks were deliberately not migrated. For
                // imports, keep completed history only and discard stale parts.
                if !exists { task.parts.clear(); }
                for part in &mut task.parts {
                    part.active = false;
                    part.reserved = 0;
                }
                task.parts.sort_by_key(|p| p.start);
                let mut seen = HashSet::new();
                let mut previous_end = 0u64;
                let valid = task.parts.len() <= PART_LIMIT && task.parts.iter().all(|p| {
                    let good = p.start == previous_end && p.end >= p.start
                        && p.id > 0 && p.id <= 1_000_000 && seen.insert(p.id);
                    previous_end = p.end;
                    good
                }) && (task.stream_mode || task.parts.is_empty()
                      || task.view.total_bytes == Some(previous_end));
                if !valid {
                    task.parts.clear();
                    task.validator = Validator::default();
                    task.resource_url = None;
                    task.view.notice = Some("Invalid checkpoint layout was discarded; QDM will download a fresh file.".into());
                }
                if task.view.status != "completed" {
                    let direct = Path::new(&task.view.directory).join(format!(".qdm-{}.partial", task.view.id));
                    let size = fs::metadata(&direct).await.map(|m| m.len()).unwrap_or(0);
                    for part in &mut task.parts {
                        let durable = part.durable.min(part.len());
                        let valid = durable > 0 && size >= part.start.saturating_add(durable)
                            && transfer::check_local_tail(&direct, part.start + durable,
                                durable, part.durable_tail_sha256.as_deref()).await;
                        part.downloaded = if valid { durable } else { 0 };
                        part.durable = part.downloaded;
                    }
                    // Do not truncate a shared file on recovery. Other durable
                    // segments may have valid data beyond this one's boundary.
                }
                for lane in &mut task.lane_activity {
                    lane.active = false;
                    if let Some(pid) = lane.current_part_id {
                        if let Some(part) = task.parts.iter().find(|p| p.id == pid) {
                            lane.current_bytes = part.downloaded;
                            lane.current_total = part.len();
                        }
                    }
                }
                task.next_part_id = task.parts.iter().map(|p| p.id).max().unwrap_or(0) + 1;
                task.refresh();
                tasks.push(task);
            }
            if !exists {
                warning = Some("Imported completed download history; previous .part checkpoints were intentionally discarded.".into());
            }
        }
''' + s[end:]
s=once(s,'        engine.spawn_monitor();','        engine.persist().await?; // Initialize SQLite before scheduling any jobs.\n        engine.spawn_monitor();','init_store')
start=s.index('    fn part_path(&self, id: &str, part: u64) -> PathBuf {')
end=s.index('\n    pub async fn add(',start)
s=s[:start]+'''    // A transfer has ONE temporary file on the same volume as the final output.
    // Independent workers use separate file handles and disjoint absolute offsets.
    fn part_path(&self, id: &str, _part: u64) -> PathBuf {
        let state = lock(&self.inner.state);
        let directory = state.tasks.iter().find(|t| t.view.id == id)
            .map(|t| PathBuf::from(&t.view.directory))
            .unwrap_or_else(|| self.inner.data_dir.clone());
        directory.join(format!(".qdm-{id}.partial"))
    }
''' +s[end:]
s=once(s,'        let directory = self.inner.data_dir.join("parts-v2").join(id);\n        if Uuid::parse_str(id).is_ok() {\n            let _ = fs::remove_dir_all(directory).await;\n        }',
'''        if Uuid::parse_str(id).is_ok() {
            let directory = PathBuf::from(&self.inner.data_dir);
            let direct = directory.join(format!(".qdm-{id}.partial"));
            // Remove only the task-owned temporary file in its configured download dir.
            let actual = self.part_path_after_removal(id);
            let _ = fs::remove_file(actual).await;
            let _ = fs::remove_file(direct).await;
        }''','remove_temp')
# Need accurate actual path before we remove task from state - cache before retain
s=once(s,'    pub async fn remove(&self, id: &str) -> Result<(), String> {\n        self.pause(id).await?;',
'''    pub async fn remove(&self, id: &str) -> Result<(), String> {
        self.pause(id).await?;
        let temporary = self.part_path(id, 0);''','remove_snapshot')
s=once(s,'''            let directory = PathBuf::from(&self.inner.data_dir);
            let direct = directory.join(format!(".qdm-{id}.partial"));
            // Remove only the task-owned temporary file in its configured download dir.
            let actual = self.part_path_after_removal(id);
            let _ = fs::remove_file(actual).await;
            let _ = fs::remove_file(direct).await;''',
'''            let _ = fs::remove_file(temporary).await;''','remove_cleanup')
start=s.index('        let bytes = serde_json::to_vec(&saved)')
end=s.index('\n    }\n\n    fn spawn_monitor',start)
s=s[:start]+'''        store::save(self.inner.data_dir.join("state-v3.sqlite3"), saved).await
''' +s[end:]
s=once(s,'schema_version: 2,\n                settings:', 'schema_version: 3,\n                settings:','save_schema')
p.write_text(s,encoding='utf-8')
m=root/'src/main.rs'
t=m.read_text(encoding='utf-8')
t=once(t,'let first_run = !data.join("state-v2.json").exists();',
    'let first_run = !data.join("state-v3.sqlite3").exists() && !data.join("state-v2.json").exists();',
    'first_run')
m.write_text(t,encoding='utf-8')
print('QDM mod.rs and main.rs migrated to SQLite and direct path mapping; source updated')
