from pathlib import Path
p=Path(r'C:\Users\QUEEN\Projects\QDM\native\src\engine\transfer.rs')
s=p.read_text(encoding='utf8')
def one(a,b,label):
 global s
 n=s.count(a)
 if n!=1: raise Exception(f'{label}: {n} matches')
 s=s.replace(a,b,1)
one('    fn claim(&self, id: &str) -> Result<Claim, Fault> {','    fn claim(&self, id: &str, slot: u8) -> Result<Claim, Fault> {','claim accepts slot')
one('''        let limit = if task.adaptive_limit == 0 { user_limit } else { user_limit.min(task.adaptive_limit as usize) };
        if task.parts.iter().filter(|p| p.active).count() >= limit {''','''        let limit = if task.adaptive_limit == 0 { user_limit } else { user_limit.min(task.adaptive_limit as usize) };
        // A worker owns a fixed slot; dynamically split pieces are not new connections.
        if slot as usize >= limit || task.parts.iter().filter(|p| p.active).count() >= limit {''','cap by slot')
one('''            task.parts[index].active = true;
            let part = task.parts[index].clone();
            task.refresh();''','''            task.ensure_lane_activity();
            task.parts[index].active = true;
            task.parts[index].owner_lane = Some(slot);
            let part = task.parts[index].clone();
            let lane = &mut task.lane_activity[slot as usize];
            lane.current_part_id = Some(part.id);
            lane.current_bytes = part.downloaded;
            lane.current_total = part.len();
            lane.active = true;
            task.refresh();''','claim owner')
one('''                    active: true,
                    ..Part::default()
                };
                task.parts.push(part.clone());
                task.view.dynamic_splits += 1;
                task.refresh();''','''                    active: true,
                    owner_lane: Some(slot),
                    ..Part::default()
                };
                task.ensure_lane_activity();
                let lane = &mut task.lane_activity[slot as usize];
                lane.current_part_id = Some(id);
                lane.current_bytes = 0;
                lane.current_total = part.len();
                lane.active = true;
                task.parts.push(part.clone());
                task.view.dynamic_splits += 1;
                task.refresh();''','split slot owner')
one('''        for _ in 0..64 {
            let engine = self.clone();''','''        for slot in 0..64u8 {
            let engine = self.clone();''','spawn numbered workers')
one('''            workers.spawn(async move { engine.worker(&id, &meta, stop).await });''','''            workers.spawn(async move { engine.worker(&id, slot, &meta, stop).await });''','forward slot')
one('''    async fn worker(&self, id: &str, meta: &Meta, mut stop: Stop) -> Result<(), Fault> {''','''    async fn worker(&self, id: &str, slot: u8, meta: &Meta, mut stop: Stop) -> Result<(), Fault> {''','worker signature')
one('''            match self.claim(id)? {''','''            match self.claim(id, slot)? {''','worker claim')
one('''                            if let Some(part) = task.parts.iter_mut().find(|p| p.id == part.id) {
                                part.active = false;
                                part.reserved = 0;
                            }
                            task.refresh();''','''                            if let Some(piece) = task.parts.iter_mut().find(|p| p.id == part.id) {
                                piece.active = false;
                                piece.reserved = 0;
                                if let Some(lane) = task.lane_activity.get_mut(slot as usize) {
                                    lane.current_bytes = piece.downloaded;
                                    lane.current_total = piece.len();
                                    lane.active = false;
                                }
                            }
                            task.refresh();''','worker cleanup')
one('''                                if written.is_ok() {
                                    part.downloaded += count as u64;
                                }
                                part.reserved = 0;''','''                                if written.is_ok() {
                                    part.downloaded += count as u64;
                                    if let Some(slot) = part.owner_lane {
                                        if let Some(lane) = task.lane_activity.get_mut(slot as usize) {
                                            lane.transferred_bytes = lane.transferred_bytes.saturating_add(count as u64);
                                            lane.current_bytes = part.downloaded;
                                            lane.current_total = part.len();
                                        }
                                    }
                                }
                                part.reserved = 0;''','record real worker bytes')
# merging status, separate from already downloaded network bytes
one('''                task.view.status = "verifying".into();
                task.view.speed_bps = 0;
                task.refresh();''','''                task.view.status = "verifying".into();
                task.view.assembly_bytes = 0;
                task.view.assembly_total_bytes = total;
                task.view.speed_bps = 0;
                task.refresh();''','verify stage')
one('''        self.verify_boundaries(id, &task.parts, stop).await?;
        let directory''','''        self.verify_boundaries(id, &task.parts, stop).await?;
        {
            let mut state = lock(&self.inner.state);
            if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                current.view.status = "assembling".into();
                current.view.assembly_bytes = 0;
                current.view.assembly_total_bytes = total;
                current.view.notice = Some("Combining verified partial files into one output...".into());
            }
        }
        self.changed();
        self.emit();
        let directory''','assemble stage visible')
one('''        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        for part in &task.parts {''','''        let mut hash = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        let mut assembled = 0u64;
        let mut reported = 0u64;
        for part in &task.parts {''','assembly counters')
one('''                hash.update(&buffer[..count]);
                remaining -= count as u64;
            }
        }
        output.flush().await''','''                hash.update(&buffer[..count]);
                remaining -= count as u64;
                assembled = assembled.saturating_add(count as u64);
                // Publish actual output bytes, not the already-finished download ratio.
                // Throttle UI updates to a few per second for large files.
                if assembled.saturating_sub(reported) >= 8 * 1024 * 1024 || assembled == total {
                    {
                        let mut state = lock(&self.inner.state);
                        if let Some(current) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                            current.view.assembly_bytes = assembled.min(total);
                        }
                    }
                    self.changed();
                    reported = assembled;
                }
            }
        }
        output.flush().await''','assembly progress')
p.write_text(s,encoding='utf8')
print('Worker slots fixed to 1..64; dynamic pieces stay independent; assembly progress emitted.')
