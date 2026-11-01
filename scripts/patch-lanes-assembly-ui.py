from pathlib import Path
r=Path(r'C:\Users\QUEEN\Projects\QDM\native\src\ui')
p=r/'lanes.rs';s=p.read_text(encoding='utf8')
def ch(a,b,label):
 global s
 count=s.count(a)
 if count!=1: raise Exception(f'{label}: {count} matches')
 s=s.replace(a,b,1)
ch('if length == 0 { 1.0 }','if length == 0 { 0.0 }','unused slots zero')
ch('self.task.as_ref().map_or(0, |t| t.segments.len())','self.task.as_ref().map_or(0, |t| t.lanes.len())','64 lanes only')
ch('''        label(dc,11,3,(width-280).max(70),23,
            &format!("{}   |   {} active / {} configured",short_name,current,limit),ink,false);''','''        label(dc,11,3,(width-290).max(70),23,
            &format!("{}  |  {}/{} connections  |  {} chunks",
                short_name,current,limit,t.part_count),ink,false);''','connection/chunk header')
ch('''        // Always-visible overview of every connection (two rows, up to 64).
        let n=t.segments.len().min(64);''','''        // Always-visible overview of worker slots, NEVER the dynamic part list.
        let n=t.lanes.len().min(64);''','n lanes')
ch('''            let segment=&t.segments[i];
            let total=segment.end.saturating_sub(segment.start);
            let progress=segment_percent(segment.downloaded_bytes,total);''','''            let lane=&t.lanes[i];
            let progress=segment_percent(lane.current_bytes,lane.current_total);''','overview bytes')
ch('''            fill(dc,left,top,done,6,if segment.active {green}else{saved});''','''            fill(dc,left,top,done,6,if lane.active {green}else{saved});''','overview active')
ch('''        label(dc,11,49,width-22,13,"GREEN = transferred bytes   |   bright = receiving   |   scroll for individual lanes",muted,false);

        let ncols=cols(width);''','''        if t.status == "assembling" {
            let progress=segment_percent(t.assembly_bytes,t.assembly_total_bytes);
            // Physical merging progress, independent of network download at 100%.
            fill(dc,9,29,width-18,28,bg);
            label(dc,11,28,width-22,15,&format!(
                "COMBINING FILE   {:.1}%  |  {} / {}",
                100.0*progress,human(t.assembly_bytes),human(t.assembly_total_bytes)
            ),ink,false);
            fill(dc,11,47,width-22,10,light);
            fill(dc,11,47,((width-22) as f64*progress).round() as i32,10,green);
        } else {
            let legend = if t.lane_history_estimated {
                "Older chunks grouped into 64 lanes for display; historical workers unknown"
            } else {
                "GREEN = downloaded by each connection  |  bright = receiving  |  scroll to lane #64"
            };
            label(dc,11,49,width-22,13,legend,muted,false);
        }

        let ncols=cols(width);''','merge header/legacy')
ch('''        for (i,segment) in t.segments.iter().enumerate() {''','''        for (i,lane) in t.lanes.iter().enumerate() {''','lane grid')
ch('''            let total=segment.end.saturating_sub(segment.start);
            let done=segment.downloaded_bytes.min(total);
            let progress=segment_percent(done,total);
            let state=if total>0 && done>=total {"Done"}
                else if segment.active {"Receiving"} else {"Waiting"};
            // Always report total amount downloaded by this lane, not overall file.
            let first=format!("#{:02}  {}",segment.id,state);
            let second=format!("{}   {:>3.0}%",human(done),progress*100.);''','''            let total=lane.current_total;
            let done=lane.current_bytes.min(total);
            let progress=segment_percent(done,total);
            let state=if lane.active {"Receiving"}
                else if (t.status == "completed" && lane.transferred_bytes>0)
                    || (total>0 && done>=total) {"Done"}
                else if lane.transferred_bytes>0 {"Waiting"} else {"Idle"};
            // This byte count is associated with a stable WORKER slot.
            let first=format!("#{:02}  {}",lane.id,state);
            let second=format!("{}   {:>3.0}%",human(lane.transferred_bytes),progress*100.);''','worker data')
ch('''                    if segment.active {rgb(25,130,60)}else{ink},false);''','''                    if lane.active {rgb(25,130,60)}else{ink},false);''','color')
ch('''                    if segment.active {green}else{saved});''','''                    if lane.active {green}else{saved});''','bar active')
ch('''        assert_eq!(segment_percent(0,0),1.0);''','''        assert_eq!(segment_percent(0,0),0.0);''','unit test')
p.write_text(s,encoding='utf8')
p=r/'windows.rs';s=p.read_text(encoding='utf8')
def change(a,b,label):
 global s
 count=s.count(a)
 if count!=1: raise Exception(f'{label}: {count} matches')
 s=s.replace(a,b,1)
# use stage details in main window even when old notice exists
change('''        let detail = if !self.banner.is_empty() {
            self.banner.clone()
        } else if let Some(t) = &selected {''','''        let detail = if !self.banner.is_empty() {
            self.banner.clone()
        } else if let Some(t) = &selected {
            if t.status == "assembling" {
                let pct = if t.assembly_total_bytes == 0 { 0.0 } else {
                    (t.assembly_bytes as f64 / t.assembly_total_bytes as f64 * 100.0).min(100.0)
                };
                format!("COMBINING FILE  {:.1}%  |  {} / {}\\nWriting and verifying the final output...",
                    pct, bytes(t.assembly_bytes), bytes(t.assembly_total_bytes))
            } else {''','assembly detail prelude')
# append closing brace after detailed format block before else no selected
change('''            format!("{}  •  {}\\n{}", t.filename, count, description)
        } else {''','''            format!("{}  •  {}\\n{}", t.filename, count, description)
            }
        } else {''','assembly detail close')
change('''        let percentage = selected
            .as_ref()
            .and_then(|t| {
                t.total_bytes.map(|total| {''','''        let percentage = selected
            .as_ref()
            .and_then(|t| {
                if t.status == "assembling" {
                    let total = t.assembly_total_bytes;
                    return Some(if total == 0 { 0 } else {
                        (t.assembly_bytes.saturating_mul(10_000) / total).min(10_000)
                    });
                }
                t.total_bytes.map(|total| {''','percentage assembly')
p.write_text(s,encoding='utf8')
print('Main UI uses <=64 worker slots; COMBINING FILE has separate green completion progress.')
