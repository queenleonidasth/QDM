from pathlib import Path
p=Path(r'C:\Users\QUEEN\Projects\QDM\native\src\ui\windows.rs')
s=p.read_text(encoding='utf8')
def one(src,old,new,key):
  num=src.count(old)
  if num!=1: raise RuntimeError(f'{key}: {num} matches')
  return src.replace(old,new,1)
s=one(s,'use crate::api::{AddRequest, Command, Settings, Snapshot, TaskView, UiEvent};',
'''use crate::api::{AddRequest, Command, Settings, Snapshot, TaskView, UiEvent};
#[path = "lanes.rs"]
mod lanes;''','module')
s=one(s,'    list: HWND,\n','    list: HWND,\n    lanes: HWND,\n','lanes control')
s=one(s,'        let mailbox = Arc::new(Mutex::new(Mailbox::default()));',
'''        lanes::register(instance)?;
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));''','class registration')
s=one(s,'            940,\n            560,','            1020,\n            740,','default window size')
s=one(s,'        self.controls.monitor = self.button(self.hwnd, "ท่อ / Connections", ID_DETAILS);\n','','remove extra button')
s=one(s,'        self.controls.progress =\n','        self.controls.lanes = lanes::create(self.hwnd, self.instance);\n        self.controls.progress =\n','create panel')
s=one(s,'            self.controls.monitor,\n','','font remove monitor')
s=one(s,'            (self.controls.monitor, 136),\n','','remove monitor layout')
s=one(s,'        let list_h = (height - 202).max(100);',
'''        // A compact task list leaves the large central area for all lane
        // progress bars. The lane grid scrolls independently of the list.
        let list_h = ((height - 260) / 4).clamp(102, 166);''','list height')
s=one(s,'''        self.place(self.controls.progress, 20, 110 + list_h + 9, list_w, 5);
        self.place(self.controls.detail, 20, 110 + list_h + 23, list_w, 38);
        self.place(self.controls.footer, 20, height - 24, list_w, 20);''',
'''        let bottom = height - 80;
        let lanes_top = 110 + list_h + 12;
        let lane_height = (bottom - 12 - lanes_top).max(30);
        self.place(self.controls.lanes, 20, lanes_top, list_w, lane_height);
        self.place(self.controls.progress, 20, bottom, list_w, 5);
        self.place(self.controls.detail, 20, bottom + 10, list_w, 37);
        self.place(self.controls.footer, 20, height - 24, list_w, 20);''','panel layout')
s=one(s,'''    unsafe fn update_selection(&mut self) {
        let selected = self.selected().cloned();''',
'''    unsafe fn update_selection(&mut self) {
        // A running download is displayed by default, without requiring a click.
        let shown = self.selected()
            .or_else(|| self.snapshot.downloads.iter().find(|t| active(&t.status)))
            .or_else(|| self.snapshot.downloads.first());
        lanes::update(self.controls.lanes, shown);
        let selected = self.selected().cloned();''','pane automatic update')
s=one(s,'        EnableWindow(self.controls.monitor, selected.is_some() as i32);\n','','remove old monitor enabled')
# No buttons are needed to expose the lanes on main. Detailed modal still exists from context.
p.write_text(s,encoding='utf8')
print('Updated native UI layout, automatic per-lane main dashboard')
