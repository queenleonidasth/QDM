//! Low-overhead owner-painted lane monitor embedded in QDM's main window.
//! This is a native Win32 child window (no WebView or timer); it only repaints
//! after engine snapshots, scrolling or a resize.
use crate::api::TaskView;
use std::{ffi::c_void, mem::{size_of,zeroed}, ptr::{null,null_mut}};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    UI::{Controls::SetScrollInfo, WindowsAndMessaging::*},
};

const CLASS: &str = "QDM.Native.LaneGrid.v1";
const WM_LANES_DATA: u32 = WM_APP + 42;
const HEADER_H: i32 = 63;
const ROW_H: i32 = 43;
const GAP: i32 = 6;

fn w(s: &str) -> Vec<u16> { s.encode_utf16().chain(Some(0)).collect() }
fn rgb(r: u8, g: u8, b: u8) -> COLORREF {
    (r as u32) | ((g as u32) << 8) | ((b as u32) << 16)
}
fn human(bytes: u64) -> String {
    if bytes < 1024 { return format!("{bytes} B"); }
    let mut x = bytes as f64;
    let mut i = 0;
    let units = ["B","KiB","MiB","GiB","TiB"];
    while x >= 1024.0 && i < 4 { x /= 1024.0; i += 1; }
    format!("{x:.1} {}", units[i])
}
fn cols(width: i32) -> usize {
    if width >= 800 { 4 } else if width >= 550 { 3 }
    else if width >= 330 { 2 } else { 1 }
}
fn segment_percent(downloaded: u64, length: u64) -> f64 {
    if length == 0 { 0.0 } else { (downloaded as f64 / length as f64).clamp(0.0,1.0) }
}
#[derive(Default)]
struct LaneGrid {
    task: Option<TaskView>,
    scroll: i32,
}
impl LaneGrid {
    fn num(&self) -> usize { self.task.as_ref().map_or(0, |t| t.lanes.len()) }
    fn max_scroll(&self, width: i32, height: i32) -> i32 {
        let total_rows = (self.num() + cols(width) - 1) / cols(width);
        let content = total_rows as i32 * ROW_H + GAP;
        (content - (height - HEADER_H).max(0)).max(0)
    }
}
unsafe fn rect_fill(dc: HDC, rc: RECT, color: COLORREF) {
    let brush = CreateSolidBrush(color);
    if !brush.is_null() {
        FillRect(dc, &rc, brush);
        DeleteObject(brush as HGDIOBJ);
    }
}
unsafe fn fill(dc: HDC, x: i32,y: i32, width:i32,height:i32,color:COLORREF) {
    if width <= 0 || height <= 0 { return; }
    rect_fill(dc, RECT{left:x,top:y,right:x+width,bottom:y+height}, color);
}
unsafe fn label(dc: HDC, x:i32,y:i32,width:i32,height:i32,content:&str,color:COLORREF,right:bool) {
    if width <= 0 || height <= 0 { return; }
    SetBkMode(dc, TRANSPARENT as i32);
    SetTextColor(dc, color);
    let mut box_rect = RECT{left:x,top:y,right:x+width,bottom:y+height};
    DrawTextW(dc,w(content).as_ptr(),-1,&mut box_rect,
        DT_SINGLELINE|DT_VCENTER|DT_END_ELLIPSIS|DT_NOPREFIX|
        if right { DT_RIGHT } else { DT_LEFT });
}
unsafe fn update_scroll(hwnd: HWND, pane: &mut LaneGrid) {
    let mut rc: RECT = zeroed();
    GetClientRect(hwnd,&mut rc);
    let width = (rc.right-rc.left).max(1);
    let height = (rc.bottom-rc.top).max(1);
    pane.scroll = pane.scroll.clamp(0,pane.max_scroll(width,height));
    let total_rows = (pane.num() + cols(width) - 1) / cols(width);
    let content = (total_rows as i32 * ROW_H + GAP).max(1);
    let mut info = SCROLLINFO{
        cbSize:size_of::<SCROLLINFO>() as u32,
        fMask:SIF_RANGE|SIF_PAGE|SIF_POS,
        nMin:0,
        nMax:content-1,
        nPage:(height-HEADER_H).max(1) as u32,
        nPos:pane.scroll,
        nTrackPos:0,
    };
    SetScrollInfo(hwnd,SB_VERT,&mut info,1);
}
unsafe fn render(hwnd: HWND, pane: &LaneGrid) {
    let mut ps: PAINTSTRUCT = zeroed();
    let target = BeginPaint(hwnd,&mut ps);
    if target.is_null() { return; }
    let mut rect:RECT=zeroed();
    GetClientRect(hwnd,&mut rect);
    let width=(rect.right-rect.left).max(1);
    let height=(rect.bottom-rect.top).max(1);

    let memory=CreateCompatibleDC(target);
    let bmp=if memory.is_null(){null_mut()}else{CreateCompatibleBitmap(target,width,height)};
    let double_buffer=!memory.is_null()&&!bmp.is_null();
    let dc=if double_buffer {memory}else{target};
    let old_bmp=if double_buffer {SelectObject(memory,bmp as HGDIOBJ)}else{null_mut()};
    let font=GetStockObject(DEFAULT_GUI_FONT);
    let old_font=if !font.is_null(){SelectObject(dc,font)}else{null_mut()};

    let bg=rgb(250,252,250);
    let muted=rgb(112,128,119);
    let ink=rgb(43,61,49);
    let green=rgb(39,171,85);
    let saved=rgb(91,185,116);
    let light=rgb(229,237,230);
    fill(dc,0,0,width,height,bg);
    fill(dc,0,0,width,1,light);

    if let Some(t)=pane.task.as_ref() {
        let current=t.connections_active;
        let limit=t.connections_limit;
        let short_name=if t.filename.chars().count()>38 {
            format!("{}...",t.filename.chars().take(34).collect::<String>())
        }else{t.filename.clone()};
        label(dc,11,3,(width-290).max(70),23,
            &format!("{}  |  {}/{} connections  |  {} chunks",
                short_name,current,limit,t.part_count),ink,false);
        label(dc,(width-278).max(130),3,267,23,
            &format!("{} / {}",human(t.downloaded_bytes),
                t.total_bytes.map(human).unwrap_or_else(||"?".into())),muted,true);
        // Always-visible overview of worker slots, NEVER the dynamic part list.
        let n=t.lanes.len().min(128);
        let per_row=32usize;
        for i in 0..n {
            let row=(i/per_row) as i32;
            let col=(i%per_row) as i32;
            let left=11+col*(width-22)/32;
            let right=11+(col+1)*(width-22)/32-2;
            let tile=(right-left).max(3);
            let lane=&t.lanes[i];
            let progress=segment_percent(lane.current_bytes,lane.current_total);
            let top=29+row*10;
            fill(dc,left,top,tile,6,light);
            let done=(progress*tile as f64).round() as i32;
            fill(dc,left,top,done,6,if lane.active {green}else{saved});
        }
        let legend_top=29+((n.max(1) as i32+per_row as i32-1)/per_row as i32)*10+2;
        if t.status == "assembling" {
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
                "Older chunks grouped into 128 lanes for display; historical workers unknown"
            } else {
                "GREEN = downloaded by each connection  |  bright = receiving  |  scroll to lane #128"
            };
            label(dc,11,legend_top,width-22,13,legend,muted,false);
        }

        let ncols=cols(width);
        let cw=((width-22) - (ncols as i32-1)*GAP).max(4)/ncols as i32;
        let view_bottom=height;
        for (i,lane) in t.lanes.iter().enumerate() {
            let row=(i/ncols) as i32;
            let col=(i%ncols) as i32;
            let left=9+col*(cw+GAP);
            let top=HEADER_H+row*ROW_H-pane.scroll;
            if top+ROW_H<HEADER_H || top>view_bottom {continue;}
            let cell_w=cw.max(5);
            let clip_top=top.max(HEADER_H);
            if clip_top < top+ROW_H-2 {
                fill(dc,left,clip_top,cell_w,top+ROW_H-2-clip_top,rgb(244,248,245));
            }
            let total=lane.current_total;
            let done=lane.current_bytes.min(total);
            let progress=segment_percent(done,total);
            let state=if lane.active {"Receiving"}
                else if (t.status == "completed" && lane.transferred_bytes>0)
                    || (total>0 && done>=total) {"Done"}
                else if lane.transferred_bytes>0 {"Waiting"} else {"Idle"};
            // This byte count is associated with a stable WORKER slot.
            let first=format!("#{:02}  {}",lane.id,state);
            let second=format!("{}   {:>3.0}%",human(lane.transferred_bytes),progress*100.);
            if top>=HEADER_H {
                label(dc,left+5,top+1,(cell_w*53/100).max(25),19,&first,
                    if lane.active {rgb(25,130,60)}else{ink},false);
                label(dc,left+cell_w*52/100,top+1,(cell_w*48/100-5).max(10),19,
                    &second,ink,true);
            }
            let bar_x=left+5;
            let bar_y=top+25;
            let bar_w=cell_w-10;
            if bar_y>=HEADER_H && bar_y+9<=height {
                fill(dc,bar_x,bar_y,bar_w,9,light);
                fill(dc,bar_x,bar_y,(bar_w as f64*progress).round() as i32,9,
                    if lane.active {green}else{saved});
            }
            if top+ROW_H-2>=HEADER_H && top+ROW_H-2<height {
                fill(dc,left,top+ROW_H-2,cell_w,1,light);
            }
        }
    } else {
        label(dc,12,10,width-24,30,"Connection activity will appear here automatically",ink,false);
        label(dc,12,43,width-24,22,"Add a URL above. Each lane will show its downloaded bytes and a green progress bar.",muted,false);
    }
    if !old_font.is_null(){SelectObject(dc,old_font);}
    if double_buffer {
        BitBlt(target,0,0,width,height,dc,0,0,SRCCOPY);
        SelectObject(memory,old_bmp);
        DeleteObject(bmp as HGDIOBJ);
        DeleteDC(memory);
    } else if !memory.is_null(){DeleteDC(memory);}
    EndPaint(hwnd,&ps);
}
unsafe extern "system" fn lane_proc(hwnd:HWND,msg:u32,wp:WPARAM,lp:LPARAM)->LRESULT {
    if msg==WM_NCCREATE {
        let c=&*(lp as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd,GWLP_USERDATA,c.lpCreateParams as isize);
        return DefWindowProcW(hwnd,msg,wp,lp);
    }
    let ptr=GetWindowLongPtrW(hwnd,GWLP_USERDATA) as *mut LaneGrid;
    if ptr.is_null(){return DefWindowProcW(hwnd,msg,wp,lp);}
    let pane=&mut *ptr;
    match msg {
        WM_LANES_DATA=>{
            pane.task=if lp==0 {None}else{Some((*(lp as *const TaskView)).clone())};
            update_scroll(hwnd,pane);
            InvalidateRect(hwnd,null(),0);
            0
        }
        WM_SIZE=>{
            update_scroll(hwnd,pane);
            InvalidateRect(hwnd,null(),0);
            0
        }
        WM_VSCROLL=>{
            let mut rc:RECT=zeroed();
            GetClientRect(hwnd,&mut rc);
            let page=(rc.bottom-rc.top-HEADER_H).max(1);
            let action=(wp&0xffff) as i32;
            let mut track:SCROLLINFO=zeroed();
            track.cbSize=size_of::<SCROLLINFO>() as u32;
            track.fMask=SIF_TRACKPOS;
            GetScrollInfo(hwnd,SB_VERT,&mut track);
            pane.scroll=match action {
                SB_LINEUP=>pane.scroll-ROW_H,
                SB_LINEDOWN=>pane.scroll+ROW_H,
                SB_PAGEUP=>pane.scroll-page,
                SB_PAGEDOWN=>pane.scroll+page,
                SB_TOP=>0,
                SB_BOTTOM=>i32::MAX,
                SB_THUMBPOSITION|SB_THUMBTRACK=>track.nTrackPos,
                _=>pane.scroll,
            };
            update_scroll(hwnd,pane);
            InvalidateRect(hwnd,null(),0);
            0
        }
        WM_MOUSEWHEEL=>{
            let delta=((wp>>16) as u16 as i16) as i32;
            pane.scroll-=delta/120*ROW_H*3;
            update_scroll(hwnd,pane);
            InvalidateRect(hwnd,null(),0);
            0
        }
        WM_ERASEBKGND=>1,
        WM_PAINT=>{render(hwnd,pane);0}
        WM_NCDESTROY=>{
            SetWindowLongPtrW(hwnd,GWLP_USERDATA,0);
            drop(Box::from_raw(ptr));
            DefWindowProcW(hwnd,msg,wp,lp)
        }
        _=>DefWindowProcW(hwnd,msg,wp,lp),
    }
}
pub(super) unsafe fn register(instance:HINSTANCE)->Result<(),String>{
    let name=w(CLASS);
    let cls=WNDCLASSEXW {
        cbSize:size_of::<WNDCLASSEXW>() as u32,
        style:CS_HREDRAW|CS_VREDRAW,
        lpfnWndProc:Some(lane_proc),
        hInstance:instance,
        hCursor:LoadCursorW(null_mut(),IDC_ARROW),
        lpszClassName:name.as_ptr(),
        ..zeroed()
    };
    if RegisterClassExW(&cls)==0{return Err("Could not register QDM connection panel".into());}
    Ok(())
}
pub(super) unsafe fn create(parent:HWND,instance:HINSTANCE)->HWND {
    let data=Box::into_raw(Box::new(LaneGrid::default()));
    let hwnd=CreateWindowExW(
        0,w(CLASS).as_ptr(),w("").as_ptr(),
        WS_CHILD|WS_VISIBLE|WS_VSCROLL|WS_CLIPSIBLINGS,
        0,0,100,100,parent,1040usize as HMENU,instance,data as *mut c_void);
    if hwnd.is_null(){drop(Box::from_raw(data));}
    hwnd
}
pub(super) unsafe fn update(hwnd:HWND,task:Option<&TaskView>){
    if hwnd.is_null(){return;}
    let pointer=task.map_or(0,|value|value as *const TaskView as isize);
    SendMessageW(hwnd,WM_LANES_DATA,0,pointer);
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test] fn columns_adapt() {
        assert_eq!(cols(920),4);
        assert_eq!(cols(720),3);
        assert_eq!(cols(430),2);
        assert_eq!(cols(290),1);
    }
    #[test] fn progress_never_overflows() {
        assert_eq!(segment_percent(0,0),0.0);
        assert_eq!(segment_percent(25,100),.25);
        assert_eq!(segment_percent(150,100),1.0);
    }
}
