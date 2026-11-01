from pathlib import Path
r=Path(r'C:\Users\QUEEN\Projects\QDM\native\src')
def change(src, old, new, label):
 count=src.count(old)
 if count!=1: raise RuntimeError(f'{label}: {count} matches')
 return src.replace(old,new,1)
p=r/'api.rs';s=p.read_text(encoding='utf8')
mark='#[derive(Debug, Clone, Serialize, Deserialize, Default)]\n#[serde(default)]\npub struct TaskView {'
insert='''#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LaneView {
    /// Stable, one-based connection slot (never a dynamic part/chunk identifier).
    pub id: u8,
    /// Actual bytes written by this worker, accumulated across its assigned pieces.
    pub transferred_bytes: u64,
    /// Progress of the most recently assigned piece.
    pub current_bytes: u64,
    pub current_total: u64,
    pub current_part_id: Option<u64>,
    pub active: bool,
}

'''
s=change(s,mark,insert+mark,'LaneView struct')
s=change(s,'    pub segments: Vec<SegmentView>,\n','''    /// Raw chunks retained for resume and diagnostics; can exceed 64.
    pub segments: Vec<SegmentView>,
    /// Actual connection worker slots, always capped by configured connections (<=64).
    pub lanes: Vec<LaneView>,
    pub part_count: u32,
    pub lane_history_estimated: bool,
    /// Bytes physically written into the assembled output file.
    pub assembly_bytes: u64,
    pub assembly_total_bytes: u64,
''','TaskView fields')
p.write_text(s,encoding='utf8')
p=r/'engine/mod.rs';s=p.read_text(encoding='utf8')
s=change(s,'use crate::api::{AddRequest, SegmentView, Settings, Snapshot, TaskView};','use crate::api::{AddRequest, LaneView, SegmentView, Settings, Snapshot, TaskView};','api import')
# Part owner is durable and carries attribution for new segments.
s=change(s,'    id: u64,\n    start: u64,\n    end: u64,\n','    id: u64,\n    start: u64,\n    end: u64,\n    #[serde(default)]\n    owner_lane: Option<u8>,\n','part assignment')
mark='#[derive(Debug, Clone, Serialize, Deserialize)]\nstruct Task {'
insert='''#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct LaneRecord {
    /// Cumulative bytes accepted to disk by this worker slot.
    transferred_bytes: u64,
    current_bytes: u64,
    current_total: u64,
    current_part_id: Option<u64>,
    active: bool,
}

'''
s=change(s,mark,insert+mark,'LaneRecord')
s=change(s,'    parts: Vec<Part>,\n','    parts: Vec<Part>,\n    #[serde(default)]\n    lane_activity: Vec<LaneRecord>,\n','Task lanes')
s=change(s,'    fn refresh(&mut self) {\n','''    fn ensure_lane_activity(&mut self) {
        if !self.lane_activity.is_empty() {
            self.lane_activity.resize(64, LaneRecord::default());
            return;
        }
        let mut slots = vec![LaneRecord::default(); 64];
        // Existing installations used raw part IDs as connection IDs; old
        // partial bytes are summarized without inventing historical worker IDs.
        let old_bytes = self.parts.iter().any(|p| p.downloaded > 0);
        self.view.lane_history_estimated = old_bytes;
        if old_bytes {
            for p in &self.parts {
                let idx = (p.id.saturating_sub(1) as usize) % 64;
                slots[idx].transferred_bytes = slots[idx].transferred_bytes.saturating_add(p.downloaded);
                slots[idx].current_bytes = slots[idx].current_bytes.saturating_add(p.downloaded);
                slots[idx].current_total = slots[idx].current_total.saturating_add(p.len());
            }
        }
        self.lane_activity = slots;
    }
    fn refresh(&mut self) {
''','Task impl header')
s=change(s,'        self.view.connections_active = self.parts.iter().filter(|p| p.active).count().min(64) as u8;\n','''        self.view.part_count = self.parts.len() as u32;
        self.view.connections_active = self.parts.iter().filter(|p| p.active).count().min(64) as u8;
''','part count')
needle='''            .collect();
    }
}

#[derive(Serialize, Deserialize)]'''
replacement='''            .collect();
        let limit = if self.stream_mode { 1 } else { self.view.connections_limit.clamp(1, 64) as usize };
        if !self.lane_activity.is_empty() {
            self.view.lanes = (0..limit).map(|index| {
                let slot = self.lane_activity.get(index).cloned().unwrap_or_default();
                LaneView {
                    id: (index + 1) as u8,
                    transferred_bytes: slot.transferred_bytes,
                    current_bytes: slot.current_bytes,
                    current_total: slot.current_total,
                    current_part_id: slot.current_part_id,
                    active: slot.active,
                }
            }).collect();
        } else if !self.parts.is_empty() {
            // Legacy jobs can contain hundreds of dynamically split CHUNKS.
            // Group their already-saved bytes into <=64 display buckets.
            // This is explicitly marked approximate, not historical connections.
            let mut lanes: Vec<LaneView> = (0..limit).map(|n| LaneView { id: (n + 1) as u8, ..LaneView::default() }).collect();
            for part in &self.parts {
                let index = part.owner_lane.map(|v| v as usize).unwrap_or(part.id.saturating_sub(1) as usize) % limit;
                let lane = &mut lanes[index];
                lane.transferred_bytes = lane.transferred_bytes.saturating_add(part.downloaded);
                lane.current_bytes = lane.current_bytes.saturating_add(part.downloaded);
                lane.current_total = lane.current_total.saturating_add(part.len());
                lane.active |= part.active;
            }
            self.view.lane_history_estimated = self.parts.iter().any(|p| p.downloaded > 0);
            self.view.lanes = lanes;
        } else {
            self.view.lanes.clear();
        }
    }
}

#[derive(Serialize, Deserialize)]'''
s=change(s,needle,replacement,'refresh lanes')
s=change(s,'                    parts: Vec::new(),\n','                    parts: Vec::new(),\n                    lane_activity: Vec::new(),\n','Task add initializer')
# lane activity reset after restart so UI does not mistake dead sockets for active
s=change(s,'                    for part in &mut task.parts {\n                        part.active = false;','''                    for lane in &mut task.lane_activity {
                        lane.active = false;
                        if let Some(pid) = lane.current_part_id {
                            if let Some(part) = task.parts.iter().find(|p| p.id == pid) {
                                lane.current_bytes = part.downloaded;
                                lane.current_total = part.len();
                            }
                        }
                    }
                    for part in &mut task.parts {
                        part.active = false;''','restart clear active')
p.write_text(s,encoding='utf8')
print('Added lane data model, maintained raw chunk list and legacy history migration.')
