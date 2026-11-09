//! Lean native Win32 UI. The engine lives on its own thread; this thread blocks
//! in GetMessage when idle and repaints only changed native controls.
#![cfg(windows)]

use crate::api::{AddRequest, Command, Settings, Snapshot, TaskView, UiEvent};
#[path = "lanes.rs"]
mod lanes;
use std::{
    cell::RefCell,
    collections::VecDeque,
    mem::{size_of, zeroed},
    path::Path,
    ptr::{null, null_mut},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{Receiver, Sender},
        Arc, Mutex,
    },
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{Com::*, DataExchange::*, LibraryLoader::GetModuleHandleW, Memory::*},
    UI::{Controls::*, HiDpi::*, Input::KeyboardAndMouse::*, Shell::*, WindowsAndMessaging::*},
};

const MAIN_CLASS: &str = "QDM.Native.Main.v2";
const AUX_CLASS: &str = "QDM.Native.Dialog.v2";
const TITLE: &str = "QDM — QUEEN Download Manager";
const WM_STATE: u32 = WM_APP + 1;
const WM_TRAY: u32 = WM_APP + 2;
const ID_URL: usize = 1001;
const ID_ADD: usize = 1002;
const ID_MENU: usize = 1003;
const ID_LIST: usize = 1004;
const ID_PAUSE: usize = 1005;
const ID_RESUME: usize = 1006;
const ID_OPEN: usize = 1007;
const ID_FOLDER: usize = 1008;
const ID_REFRESH: usize = 1009;
const ID_REMOVE: usize = 1010;
const ID_SETTINGS: usize = 1101;
const ID_ADD_ADVANCED: usize = 1102;
const ID_INTEGRATION: usize = 1103;
const ID_PAUSE_ALL: usize = 1104;
const ID_RESUME_ALL: usize = 1105;
const ID_QUIT: usize = 1106;
const ID_SHOW: usize = 1107;
const ID_DETAILS: usize = 1108;
const ID_DOWNLOAD_FOLDER: usize = 1109;
const ID_AUX_SAVE: usize = 1201;
const ID_AUX_CANCEL: usize = 1202;
const ID_AUX_BROWSE: usize = 1203;
const ID_AUX_COPY: usize = 1204;
const ID_AUX_EXTENSIONS: usize = 1205;
const CF_UNICODETEXT_VALUE: u32 = 13;
// Static-control styles are declared in SystemServices in windows-sys. These
// stable Win32 values avoid importing that unrelated feature surface.
const STATIC_LEFT: u32 = 0;
const STATIC_CENTER: u32 = 1;

fn wide(value: &str) -> Vec<u16> {
    value
        .replace('\0', "")
        .encode_utf16()
        .chain(Some(0))
        .collect()
}
fn scale(value: i32, dpi: u32) -> i32 {
    ((value as i64 * dpi as i64 + 48) / 96) as i32
}
fn bytes(value: u64) -> String {
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut n = value as f64;
    let mut i = 0;
    while n >= 1024.0 && i < units.len() - 1 {
        n /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{} {}", value, units[i])
    } else {
        format!("{:.1} {}", n, units[i])
    }
}
fn state_name(status: &str) -> &str {
    match status {
        "queued" => "รอคิว",
        "probing" | "preparing" => "ตรวจสอบไฟล์",
        "downloading" => "กำลังดาวน์โหลด",
        "paused" => "พักไว้",
        "completed" => "เสร็จแล้ว",
        "error" | "failed" => "ต้องดำเนินการ",
        "verifying" => "ตรวจสอบ SHA-256",
        "assembling" | "merging" => "บันทึกไฟล์",
        "retrying" => "กำลังลองใหม่",
        "scheduled" => "ตั้งเวลาไว้",
        "cancelled" | "canceled" => "ยกเลิกแล้ว",
        other => other,
    }
}
fn active(status: &str) -> bool {
    matches!(status, "downloading" | "probing" | "preparing" | "retrying")
}
fn can_resume(status: &str) -> bool {
    matches!(
        status,
        "paused" | "error" | "failed" | "queued" | "scheduled" | "cancelled" | "canceled"
    )
}

#[derive(Default)]
struct Controls {
    url: HWND,
    add: HWND,
    menu: HWND,
    list: HWND,
    lanes: HWND,
    pause: HWND,
    resume: HWND,
    open: HWND,
    folder: HWND,
    refresh: HWND,
    remove: HWND,
    monitor: HWND,
    progress: HWND,
    detail: HWND,
    footer: HWND,
    empty: HWND,
}
#[derive(Default)]
struct Mailbox {
    latest: Option<Snapshot>,
    events: VecDeque<UiEvent>,
}
struct SettingsFields {
    connections: HWND,
    concurrent: HWND,
    speed: HWND,
    directory: HWND,
    close_to_tray: HWND,
}
enum AuxKind {
    Settings(SettingsFields),
    Refresh {
        id: String,
        entry: HWND,
    },
    Add {
        urls: HWND,
        hash: HWND,
        start: HWND,
    },
    Integration {
        token: String,
        extension_path: String,
    },
    Details { id: String, segments: HWND },
}
struct Aux {
    hwnd: HWND,
    kind: AuxKind,
    dpi: u32,
    font: HFONT,
}
fn segment_monitor_text(task: &TaskView) -> String {
    let mut output = format!("{} connections | {} active | {} bytes downloaded\r\n",
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
        output.push_str(&format!("{:>3} [{}] {:>3}%  {:>10} / {:<10}  {:<10}  [{:>12} .. {:>12}]\r\n",
            segment.id, bar, percentage, bytes(transferred), bytes(size), state, segment.start, segment.end.saturating_sub(1)));
    }
    output
}

struct App {
    hwnd: HWND,
    instance: HINSTANCE,
    font: HFONT,
    dpi: u32,
    controls: Controls,
    tx: Sender<Command>,
    snapshot: Snapshot,
    mailbox: Arc<Mutex<Mailbox>>,
    posted: Arc<AtomicBool>,
    alive: Arc<AtomicBool>,
    rows: Vec<(String, [String; 5])>,
    aux: Option<Aux>,
    tray: bool,
    quitting: bool,
    hidden_once: bool,
    banner: String,
    detail_text: String,
    footer_text: String,
    taskbar_created: u32,
}

/// Starts the native UI on the calling thread. The caller owns engine shutdown
/// and should join its engine thread after this function returns.
pub fn run(command_tx: Sender<Command>, event_rx: Receiver<UiEvent>) -> Result<(), String> {
    unsafe {
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let com = CoInitializeEx(
            null(),
            (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32,
        );
        let mut common = INITCOMMONCONTROLSEX {
            dwSize: size_of::<INITCOMMONCONTROLSEX>() as u32,
            dwICC: ICC_LISTVIEW_CLASSES | ICC_PROGRESS_CLASS | ICC_STANDARD_CLASSES,
        };
        if InitCommonControlsEx(&mut common) == 0 {
            return Err("Windows common controls could not be initialized".into());
        }
        let instance = GetModuleHandleW(null());
        let icon = {
            let q = LoadIconW(instance, 101usize as *const u16);
            if q.is_null() {
                LoadIconW(null_mut(), IDI_APPLICATION)
            } else {
                q
            }
        };
        for (name, proc) in [
            (
                MAIN_CLASS,
                Some(main_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            ),
            (
                AUX_CLASS,
                Some(aux_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            ),
        ] {
            let class_name = wide(name);
            let class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: proc,
                hInstance: instance,
                hIcon: icon,
                hIconSm: icon,
                hCursor: LoadCursorW(null_mut(), IDC_ARROW),
                hbrBackground: (COLOR_WINDOW + 1) as HBRUSH,
                lpszClassName: class_name.as_ptr(),
                ..zeroed()
            };
            if RegisterClassExW(&class) == 0 {
                return Err(format!(
                    "Cannot register QDM window (Windows error {})",
                    GetLastError()
                ));
            }
        }
        lanes::register(instance)?;
        let mailbox = Arc::new(Mutex::new(Mailbox::default()));
        let posted = Arc::new(AtomicBool::new(false));
        let alive = Arc::new(AtomicBool::new(true));
        let mut app = Box::new(RefCell::new(App {
            hwnd: null_mut(),
            instance,
            font: null_mut(),
            dpi: 96,
            controls: Controls::default(),
            tx: command_tx,
            snapshot: Snapshot::default(),
            mailbox: mailbox.clone(),
            posted: posted.clone(),
            alive: alive.clone(),
            rows: Vec::new(),
            aux: None,
            tray: false,
            quitting: false,
            hidden_once: false,
            banner: String::new(),
            detail_text: String::new(),
            footer_text: String::new(),
            taskbar_created: RegisterWindowMessageW(wide("TaskbarCreated").as_ptr()),
        }));
        let hwnd = CreateWindowExW(
            WS_EX_APPWINDOW,
            wide(MAIN_CLASS).as_ptr(),
            wide(TITLE).as_ptr(),
            WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            1020,
            740,
            null_mut(),
            null_mut(),
            instance,
            (&mut *app as *mut RefCell<App>).cast(),
        );
        if hwnd.is_null() {
            return Err(format!(
                "Cannot create QDM window (Windows error {})",
                GetLastError()
            ));
        }
        crate::platform::tag_window(hwnd);
        let dpi = GetDpiForWindow(hwnd).max(96);
        if dpi != 96 {
            SetWindowPos(
                hwnd,
                null_mut(),
                0,
                0,
                scale(940, dpi),
                scale(560, dpi),
                SWP_NOMOVE | SWP_NOZORDER | SWP_NOACTIVATE,
            );
        }
        let hwnd_value = hwnd as usize;
        std::thread::Builder::new()
            .name("qdm-ui-events".into())
            .spawn(move || {
                while let Ok(event) = event_rx.recv() {
                    if !alive.load(Ordering::Acquire) {
                        break;
                    }
                    if let Ok(mut queue) = mailbox.lock() {
                        match event {
                            UiEvent::State(snapshot) => queue.latest = Some(snapshot),
                            other => {
                                if queue.events.len() == 64 {
                                    queue.events.pop_front();
                                }
                                queue.events.push_back(other);
                            }
                        }
                    }
                    if !posted.swap(true, Ordering::AcqRel) {
                        PostMessageW(hwnd_value as HWND, WM_STATE, 0, 0);
                    }
                }
            })
            .map_err(|e| format!("Cannot start UI event receiver: {e}"))?;
        ShowWindow(hwnd, SW_SHOW);
        UpdateWindow(hwnd);
        let initial_focus = app.borrow().controls.url;
        SetFocus(initial_focus);
        let mut message: MSG = zeroed();
        loop {
            let received = GetMessageW(&mut message, null_mut(), 0, 0);
            if received <= 0 {
                if received < 0 {
                    return Err(format!("Windows message loop failed: {}", GetLastError()));
                }
                break;
            }
            let mut handled = false;
            if message.message == WM_KEYDOWN {
                if let Ok(mut state) = app.try_borrow_mut() {
                    handled = state.key(&message);
                    state.drain_deferred_events();
                }
            }
            // IsDialogMessage can synchronously dispatch button commands. Do
            // not retain a RefCell borrow while Windows invokes our wndproc.
            if !handled {
                let target = {
                    let state = app.borrow();
                    state.aux.as_ref().map_or(state.hwnd, |a| a.hwnd)
                };
                handled = IsDialogMessageW(target, &mut message) != 0;
            }
            if !handled {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
        app.borrow().alive.store(false, Ordering::Release);
        {
            let mut state = app.borrow_mut();
            state.remove_tray();
            if !state.font.is_null() {
                DeleteObject(state.font);
                state.font = null_mut();
            }
        }
        UnregisterClassW(wide(MAIN_CLASS).as_ptr(), instance);
        UnregisterClassW(wide(AUX_CLASS).as_ptr(), instance);
        if com >= 0 {
            CoUninitialize();
        }
        Ok(())
    }
}

unsafe extern "system" fn main_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CTLCOLORSTATIC {
        let dc = wparam as HDC;
        SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
        SetBkColor(dc, GetSysColor(COLOR_WINDOW));
        return GetSysColorBrush(COLOR_WINDOW) as LRESULT;
    }
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        let state = &*(create.lpCreateParams as *const RefCell<App>);
        state.borrow_mut().hwnd = hwnd;
        // Default WM_NCCREATE processing stores the CreateWindowEx caption
        // and initializes the native nonclient window state.
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    if message == WM_DESTROY {
        PostQuitMessage(0);
        return 0;
    }
    let pointer = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *const RefCell<App>;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    // Native controls synchronously notify their parent during updates. Skip
    // those reentrant notifications rather than creating aliased Rust borrows.
    let Ok(mut app) = (*pointer).try_borrow_mut() else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    if message == app.taskbar_created {
        app.create_tray();
        return 0;
    }
    let result = match message {
        WM_CREATE => {
            app.initialize();
            0
        }
        WM_SIZE => {
            app.layout();
            0
        }
        WM_GETMINMAXINFO => {
            let limits = &mut *(lparam as *mut MINMAXINFO);
            limits.ptMinTrackSize.x = scale(780, app.dpi);
            limits.ptMinTrackSize.y = scale(420, app.dpi);
            0
        }
        WM_DPICHANGED => {
            app.dpi = (wparam & 0xffff) as u32;
            app.update_font();
            let rect = &*(lparam as *const RECT);
            SetWindowPos(
                hwnd,
                null_mut(),
                rect.left,
                rect.top,
                rect.right - rect.left,
                rect.bottom - rect.top,
                SWP_NOZORDER | SWP_NOACTIVATE,
            );
            app.layout();
            0
        }
        WM_COMMAND => {
            app.action(wparam & 0xffff);
            0
        }
        WM_NOTIFY => {
            let notification = &*(lparam as *const NMHDR);
            if notification.hwndFrom == app.controls.list {
                if notification.code == LVN_ITEMCHANGED {
                    let item = &*(lparam as *const NMLISTVIEW);
                    if item.uChanged & LVIF_STATE != 0
                        && (item.uOldState ^ item.uNewState) & LVIS_SELECTED != 0
                    {
                        app.banner.clear();
                    }
                    app.update_selection();
                }
                if notification.code == NM_DBLCLK {
                    if let Some(task) = app.selected().cloned() {
                        if task.status == "completed" {
                            app.send(Command::OpenFile(task.id));
                        } else {
                            app.details(&task);
                        }
                    }
                }
            }
            0
        }
        WM_CONTEXTMENU => {
            if wparam as HWND == app.controls.list {
                app.context_menu();
            }
            0
        }
        WM_STATE => {
            app.receive_events();
            0
        }
        WM_TRAY => {
            match lparam as u32 {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => app.show(),
                WM_RBUTTONUP | WM_CONTEXTMENU => app.tray_menu(),
                _ => {}
            }
            0
        }
        WM_CLOSE => {
            if app.aux.is_some() {
                if let Some(aux) = &app.aux {
                    SetForegroundWindow(aux.hwnd);
                }
                return 0;
            }
            if !app.quitting && app.snapshot.settings.close_to_tray && app.tray {
                ShowWindow(hwnd, SW_HIDE);
                if !app.hidden_once {
                    app.hidden_once = true;
                    app.tray_notice(
                        "QDM ยังทำงานอยู่",
                        "ดาวน์โหลดต่อในพื้นหลัง เปิดคืนจากไอคอน QDM หรือทางลัดบนเดสก์ท็อป",
                    );
                }
            } else {
                app.quit();
            }
            0
        }
        WM_QUERYENDSESSION => {
            app.send(Command::Quit);
            1
        }
        _ => {
            // DefWindowProc may synchronously send WM_CLOSE/WM_SIZE or enter
            // the native move/size loop. Those callbacks must be able to
            // borrow App (including the real titlebar Close-to-tray path).
            drop(app);
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
    };
    app.drain_deferred_events();
    result
}

unsafe extern "system" fn aux_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    if message == WM_CTLCOLORSTATIC {
        let dc = wparam as HDC;
        SetTextColor(dc, GetSysColor(COLOR_WINDOWTEXT));
        SetBkColor(dc, GetSysColor(COLOR_WINDOW));
        return GetSysColorBrush(COLOR_WINDOW) as LRESULT;
    }
    if message == WM_NCCREATE {
        let create = &*(lparam as *const CREATESTRUCTW);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, create.lpCreateParams as isize);
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let parent = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as HWND;
    if parent.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let pointer = GetWindowLongPtrW(parent, GWLP_USERDATA) as *const RefCell<App>;
    if pointer.is_null() {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    }
    let Ok(mut app) = (*pointer).try_borrow_mut() else {
        return DefWindowProcW(hwnd, message, wparam, lparam);
    };
    let result = match message {
        WM_COMMAND => {
            app.aux_action(wparam & 0xffff);
            0
        }
        WM_DPICHANGED => {
            app.resize_aux((wparam & 0xffff) as u32, &*(lparam as *const RECT));
            0
        }
        WM_CLOSE => {
            app.close_aux();
            0
        }
        _ => {
            drop(app);
            return DefWindowProcW(hwnd, message, wparam, lparam);
        }
    };
    app.drain_deferred_events();
    result
}

impl App {
    unsafe fn drain_deferred_events(&mut self) {
        // Native menus, message boxes and folder pickers run nested message
        // loops. A state notification received during a borrowed callback is
        // deferred; consume it when the outer callback returns so a coalesced
        // wakeup can never be lost.
        if !self.quitting && self.posted.load(Ordering::Acquire) {
            self.receive_events();
        }
    }

    unsafe fn control(
        &self,
        parent: HWND,
        class: &str,
        text: &str,
        style: u32,
        ex: u32,
        id: usize,
    ) -> HWND {
        let handle = CreateWindowExW(
            ex,
            wide(class).as_ptr(),
            wide(text).as_ptr(),
            WS_CHILD | WS_VISIBLE | style,
            0,
            0,
            1,
            1,
            parent,
            id as HMENU,
            self.instance,
            null(),
        );
        if !handle.is_null() && !self.font.is_null() {
            SendMessageW(handle, WM_SETFONT, self.font as usize, 0);
        }
        handle
    }
    unsafe fn button(&self, parent: HWND, text: &str, id: usize) -> HWND {
        self.control(
            parent,
            "BUTTON",
            text,
            WS_TABSTOP | BS_PUSHBUTTON as u32,
            0,
            id,
        )
    }
    unsafe fn edit(&self, parent: HWND, value: &str, id: usize, extra: u32) -> HWND {
        let control = self.control(
            parent,
            "EDIT",
            value,
            WS_TABSTOP | ES_AUTOHSCROLL as u32 | extra,
            WS_EX_CLIENTEDGE,
            id,
        );
        SendMessageW(control, EM_SETLIMITTEXT, 32_760, 0);
        control
    }
    unsafe fn label(&self, parent: HWND, value: &str, id: usize) -> HWND {
        self.control(parent, "STATIC", value, STATIC_LEFT, 0, id)
    }
    unsafe fn place(&self, handle: HWND, x: i32, y: i32, w: i32, h: i32) {
        MoveWindow(
            handle,
            scale(x, self.dpi),
            scale(y, self.dpi),
            scale(w.max(1), self.dpi),
            scale(h.max(1), self.dpi),
            1,
        );
    }
    unsafe fn initialize(&mut self) {
        self.dpi = GetDpiForWindow(self.hwnd).max(96);
        self.update_font();
        self.controls.url = self.edit(self.hwnd, "", ID_URL, 0);
        SendMessageW(
            self.controls.url,
            EM_SETCUEBANNER,
            0,
            wide("วางลิงก์ HTTP หรือ HTTPS").as_ptr() as isize,
        );
        self.controls.add = self.button(self.hwnd, "ดาวน์โหลด", ID_ADD);
        self.controls.menu = self.button(self.hwnd, "⋯", ID_MENU);
        self.controls.pause = self.button(self.hwnd, "พัก", ID_PAUSE);
        self.controls.resume = self.button(self.hwnd, "โหลดต่อ", ID_RESUME);
        self.controls.open = self.button(self.hwnd, "เปิดไฟล์", ID_OPEN);
        self.controls.folder = self.button(self.hwnd, "โฟลเดอร์", ID_FOLDER);
        self.controls.refresh = self.button(self.hwnd, "เปลี่ยนลิงก์", ID_REFRESH);
        self.controls.remove = self.button(self.hwnd, "ลบรายการ", ID_REMOVE);
        self.controls.list = self.control(
            self.hwnd,
            "SysListView32",
            "",
            WS_TABSTOP | LVS_REPORT | LVS_SINGLESEL | LVS_SHOWSELALWAYS,
            WS_EX_CLIENTEDGE,
            ID_LIST,
        );
        SendMessageW(
            self.controls.list,
            LVM_SETEXTENDEDLISTVIEWSTYLE,
            0,
            (LVS_EX_FULLROWSELECT | LVS_EX_DOUBLEBUFFER | LVS_EX_LABELTIP) as isize,
        );
        SendMessageW(self.controls.list, LVM_SETBKCOLOR, 0, 0x00ff_ffff);
        SendMessageW(self.controls.list, LVM_SETTEXTBKCOLOR, 0, 0x00ff_ffff);
        for (index, label) in ["ไฟล์", "ความคืบหน้า", "ความเร็ว", "การเชื่อมต่อ", "สถานะ"]
            .iter()
            .enumerate()
        {
            let mut title = wide(label);
            let mut col: LVCOLUMNW = zeroed();
            col.mask = LVCF_TEXT | LVCF_WIDTH | LVCF_FMT;
            col.cx = 100;
            col.pszText = title.as_mut_ptr();
            col.fmt = LVCFMT_LEFT;
            SendMessageW(
                self.controls.list,
                LVM_INSERTCOLUMNW,
                index,
                (&col as *const LVCOLUMNW) as isize,
            );
        }
        self.controls.lanes = lanes::create(self.hwnd, self.instance);
        self.controls.progress =
            self.control(self.hwnd, "msctls_progress32", "", PBS_SMOOTH, 0, 1020);
        SendMessageW(self.controls.progress, PBM_SETRANGE32, 0, 10_000);
        self.controls.detail = self.label(self.hwnd, "เลือกไฟล์เพื่อพัก โหลดต่อ หรือดูรายละเอียด", 1021);
        self.controls.footer = self.label(self.hwnd, "พร้อมดาวน์โหลด", 1022);
        self.controls.empty = self.control(
            self.hwnd,
            "STATIC",
            "เพิ่มลิงก์ด้านบนเพื่อเริ่มดาวน์โหลด",
            STATIC_CENTER | WS_DISABLED,
            0,
            1023,
        );
        self.create_tray();
        self.layout();
        self.update_selection();
        SetFocus(self.controls.url);
    }
    unsafe fn update_font(&mut self) {
        let font = make_font(self.dpi);
        if font.is_null() {
            return;
        }
        let previous = self.font;
        self.font = font;
        for c in [
            self.controls.url,
            self.controls.add,
            self.controls.menu,
            self.controls.list,
            self.controls.pause,
            self.controls.resume,
            self.controls.open,
            self.controls.folder,
            self.controls.refresh,
            self.controls.remove,
            self.controls.detail,
            self.controls.footer,
            self.controls.empty,
        ] {
            if !c.is_null() {
                SendMessageW(c, WM_SETFONT, font as usize, 1);
            }
        }
        if !previous.is_null() {
            DeleteObject(previous);
        }
    }
    unsafe fn layout(&self) {
        let mut r: RECT = zeroed();
        GetClientRect(self.hwnd, &mut r);
        let width = ((r.right - r.left) as i64 * 96 / self.dpi as i64) as i32;
        let height = ((r.bottom - r.top) as i64 * 96 / self.dpi as i64) as i32;
        self.place(self.controls.url, 20, 20, width - 202, 34);
        self.place(self.controls.add, width - 170, 20, 106, 34);
        self.place(self.controls.menu, width - 54, 20, 34, 34);
        let mut x = 20;
        for (h, w) in [
            (self.controls.pause, 64),
            (self.controls.resume, 86),
            (self.controls.open, 84),
            (self.controls.folder, 90),
            (self.controls.refresh, 104),
            (self.controls.remove, 96),
        ] {
            self.place(h, x, 68, w, 29);
            x += w + 8;
        }
        // A compact task list leaves the large central area for all lane
        // progress bars. The lane grid scrolls independently of the list.
        let list_h = ((height - 260) / 4).clamp(102, 166);
        let list_w = width - 40;
        self.place(self.controls.list, 20, 110, list_w, list_h);
        let widths = [(list_w - 475).max(200), 98, 110, 100, 145];
        for (i, w) in widths.iter().enumerate() {
            SendMessageW(
                self.controls.list,
                LVM_SETCOLUMNWIDTH,
                i,
                scale(*w, self.dpi) as isize,
            );
        }
        self.place(
            self.controls.empty,
            60,
            110 + list_h / 2 - 12,
            width - 120,
            30,
        );
        let bottom = height - 80;
        let lanes_top = 110 + list_h + 12;
        let lane_height = (bottom - 12 - lanes_top).max(30);
        self.place(self.controls.lanes, 20, lanes_top, list_w, lane_height);
        self.place(self.controls.progress, 20, bottom, list_w, 5);
        self.place(self.controls.detail, 20, bottom + 10, list_w, 37);
        self.place(self.controls.footer, 20, height - 24, list_w, 20);
    }
    fn send(&mut self, command: Command) {
        if self.tx.send(command).is_err() {
            self.banner = "ระบบดาวน์โหลดหยุดทำงาน กรุณาปิดแล้วเปิด QDM ใหม่".into();
            unsafe {
                self.update_selection();
            }
        }
    }
    unsafe fn selected_index(&self) -> Option<usize> {
        let i = SendMessageW(
            self.controls.list,
            LVM_GETNEXTITEM,
            usize::MAX,
            LVNI_SELECTED as isize,
        );
        if i < 0 {
            None
        } else {
            Some(i as usize)
        }
    }
    unsafe fn selected(&self) -> Option<&TaskView> {
        self.selected_index()
            .and_then(|i| self.snapshot.downloads.get(i))
    }
    unsafe fn receive_events(&mut self) {
        let (latest, events) = {
            let Ok(mut q) = self.mailbox.lock() else {
                return;
            };
            self.posted.store(false, Ordering::Release);
            (q.latest.take(), q.events.drain(..).collect::<Vec<_>>())
        };
        if let Some(snapshot) = latest {
            self.apply_snapshot(snapshot);
        }
        for event in events {
            match event {
                UiEvent::Error(text) | UiEvent::Notice(text) => {
                    self.banner = text;
                    self.update_selection();
                }
                UiEvent::Integration {
                    port,
                    token,
                    extension_path,
                } => self.integration(port, token, extension_path),
                UiEvent::State(_) => {}
            }
        }
    }
    unsafe fn apply_snapshot(&mut self, snapshot: Snapshot) {
        let selected = self.selected().map(|t| t.id.clone())
            .or_else(|| snapshot.downloads.iter().find(|t| active(&t.status)).map(|t| t.id.clone()))
            .or_else(|| snapshot.downloads.first().map(|t| t.id.clone()));
        let rebuild = self.rows.len() != snapshot.downloads.len()
            || self
                .rows
                .iter()
                .zip(&snapshot.downloads)
                .any(|(a, b)| a.0 != b.id);
        if rebuild {
            SendMessageW(self.controls.list, WM_SETREDRAW, 0, 0);
            SendMessageW(self.controls.list, LVM_DELETEALLITEMS, 0, 0);
            self.rows.clear();
        }
        for (index, task) in snapshot.downloads.iter().enumerate() {
            let progress = if task.status == "completed" {
                "100%".to_string()
            } else if let Some(total) = task.total_bytes {
                if total > 0 {
                    format!(
                        "{:.1}%",
                        (task.downloaded_bytes as f64 / total as f64 * 100.0).min(100.0)
                    )
                } else {
                    "0%".into()
                }
            } else {
                bytes(task.downloaded_bytes)
            };
            let row = [
                task.filename.clone(),
                progress,
                if task.speed_bps > 0 {
                    format!("{}/s", bytes(task.speed_bps))
                } else {
                    "—".into()
                },
                if active(&task.status) {
                    format!("{} / {}", task.connections_active, task.connections_limit)
                } else {
                    "—".into()
                },
                state_name(&task.status).to_string(),
            ];
            if rebuild {
                let mut text = wide(&row[0]);
                let mut item: LVITEMW = zeroed();
                item.mask = LVIF_TEXT;
                item.iItem = index as i32;
                item.pszText = text.as_mut_ptr();
                SendMessageW(
                    self.controls.list,
                    LVM_INSERTITEMW,
                    0,
                    (&item as *const LVITEMW) as isize,
                );
            }
            for (col, value) in row.iter().enumerate() {
                if rebuild || self.rows[index].1[col] != *value {
                    let mut text = wide(value);
                    let mut item: LVITEMW = zeroed();
                    item.iSubItem = col as i32;
                    item.pszText = text.as_mut_ptr();
                    SendMessageW(
                        self.controls.list,
                        LVM_SETITEMTEXTW,
                        index,
                        (&item as *const LVITEMW) as isize,
                    );
                }
            }
            if rebuild {
                self.rows.push((task.id.clone(), row));
            } else {
                self.rows[index].1 = row;
            }
        }
        // Refresh the per-connection monitor on every engine snapshot.
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
        if rebuild {
            if let Some(id) = selected {
                if let Some(i) = self.snapshot.downloads.iter().position(|t| t.id == id) {
                    let mut item: LVITEMW = zeroed();
                    item.stateMask = LVIS_SELECTED | LVIS_FOCUSED;
                    item.state = LVIS_SELECTED | LVIS_FOCUSED;
                    SendMessageW(
                        self.controls.list,
                        LVM_SETITEMSTATE,
                        i,
                        (&item as *const LVITEMW) as isize,
                    );
                }
            }
            SendMessageW(self.controls.list, WM_SETREDRAW, 1, 0);
            InvalidateRect(self.controls.list, null(), 1);
        }
        ShowWindow(
            self.controls.empty,
            if self.snapshot.downloads.is_empty() {
                SW_SHOW
            } else {
                SW_HIDE
            },
        );
        self.update_selection();
    }
    unsafe fn update_selection(&mut self) {
        // A running download is displayed by default, without requiring a click.
        let shown = self.selected()
            .or_else(|| self.snapshot.downloads.iter().find(|t| active(&t.status)))
            .or_else(|| self.snapshot.downloads.first());
        lanes::update(self.controls.lanes, shown);
        let selected = self.selected().cloned();
        EnableWindow(
            self.controls.pause,
            selected
                .as_ref()
                .is_some_and(|t| active(&t.status) || t.status == "queued") as i32,
        );
        EnableWindow(
            self.controls.resume,
            selected.as_ref().is_some_and(|t| can_resume(&t.status)) as i32,
        );
        EnableWindow(
            self.controls.open,
            selected
                .as_ref()
                .is_some_and(|t| t.status == "completed" && t.file_path.is_some())
                as i32,
        );
        EnableWindow(self.controls.folder, selected.is_some() as i32);
        EnableWindow(self.controls.remove, selected.is_some() as i32);
        EnableWindow(
            self.controls.refresh,
            selected.as_ref().is_some_and(|t| t.status != "completed") as i32,
        );
        let detail = if !self.banner.is_empty() {
            self.banner.clone()
        } else if let Some(t) = &selected {
            if t.status == "assembling" {
                let pct = if t.assembly_total_bytes == 0 { 0.0 } else {
                    (t.assembly_bytes as f64 / t.assembly_total_bytes as f64 * 100.0).min(100.0)
                };
                format!("COMBINING FILE  {:.1}%  |  {} / {}\nWriting and verifying the final output...",
                    pct, bytes(t.assembly_bytes), bytes(t.assembly_total_bytes))
            } else {
            let count = if let Some(total) = t.total_bytes {
                format!("{} / {}", bytes(t.downloaded_bytes), bytes(total))
            } else {
                bytes(t.downloaded_bytes)
            };
            let description = t
                .error
                .as_ref()
                .or(t.notice.as_ref())
                .cloned()
                .unwrap_or_else(|| {
                    if t.status == "completed" {
                        t.file_path.clone().unwrap_or_default()
                    } else {
                        format!(
                            "สูงสุด {} การเชื่อมต่อ  •  แบ่งช่วงเพิ่ม {} ครั้ง",
                            t.connections_limit, t.dynamic_splits
                        )
                    }
                });
            format!("{}  •  {}\n{}", t.filename, count, description)
            }
        } else {
            "เลือกไฟล์เพื่อพัก โหลดต่อ หรือดูรายละเอียด".into()
        };
        if self.detail_text != detail {
            SetWindowTextW(self.controls.detail, wide(&detail).as_ptr());
            self.detail_text = detail;
        }
        let percentage = selected
            .as_ref()
            .and_then(|t| {
                if t.status == "assembling" {
                    let total = t.assembly_total_bytes;
                    return Some(if total == 0 { 0 } else {
                        (t.assembly_bytes.saturating_mul(10_000) / total).min(10_000)
                    });
                }
                t.total_bytes.map(|total| {
                    if total == 0 {
                        0
                    } else {
                        (t.downloaded_bytes.saturating_mul(10_000) / total).min(10_000)
                    }
                })
            })
            .unwrap_or(0);
        SendMessageW(self.controls.progress, PBM_SETPOS, percentage as usize, 0);
        let footer = if self.snapshot.active_count > 0 {
            format!(
                "{} รายการ  •  กำลังโหลด {}  •  {}/s",
                self.snapshot.downloads.len(),
                self.snapshot.active_count,
                bytes(self.snapshot.total_speed_bps)
            )
        } else {
            format!(
                "{} รายการ  •  สูงสุด {} การเชื่อมต่อต่อไฟล์",
                self.snapshot.downloads.len(),
                self.snapshot.settings.connections
            )
        };
        if self.footer_text != footer {
            SetWindowTextW(self.controls.footer, wide(&footer).as_ptr());
            self.footer_text = footer;
        }
    }
    unsafe fn action(&mut self, id: usize) {
        if id == ID_URL {
            return;
        }
        self.banner.clear();
        let selected = self.selected().cloned();
        match id {
            ID_ADD => {
                let value = get_text(self.controls.url);
                if self.add_urls(&value, None, true) {
                    SetWindowTextW(self.controls.url, wide("").as_ptr());
                }
            }
            ID_MENU => self.main_menu(),
            ID_SETTINGS => self.settings(),
            ID_ADD_ADVANCED => self.advanced_add(),
            ID_INTEGRATION => self.send(Command::ShowIntegration),
            ID_DOWNLOAD_FOLDER => self.send(Command::OpenDownloadFolder),
            ID_PAUSE_ALL => self.send(Command::PauseAll),
            ID_RESUME_ALL => self.send(Command::ResumeAll),
            ID_QUIT => self.quit(),
            ID_SHOW => self.show(),
            ID_DETAILS => {
                if let Some(task) = selected {
                    self.details(&task)
                }
            }
            ID_PAUSE => {
                if let Some(t) = selected {
                    self.send(Command::Pause(t.id));
                }
            }
            ID_RESUME => {
                if let Some(t) = selected {
                    self.send(Command::Resume(t.id));
                }
            }
            ID_OPEN => {
                if let Some(t) = selected {
                    self.send(Command::OpenFile(t.id));
                }
            }
            ID_FOLDER => {
                if let Some(t) = selected {
                    self.send(Command::RevealFile(t.id));
                }
            }
            ID_REMOVE => {
                if let Some(t) = selected {
                    self.send(Command::Remove(t.id));
                }
            }
            ID_REFRESH => {
                if let Some(t) = selected {
                    self.refresh(&t);
                }
            }
            _ => {}
        }
        self.update_selection();
    }
    unsafe fn add_urls(&mut self, input: &str, hash: Option<String>, start: bool) -> bool {
        let urls = input
            .lines()
            .flat_map(|line| line.split_whitespace())
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>();
        if urls.is_empty() {
            self.banner = "วางลิงก์ดาวน์โหลดก่อนค่ะ".into();
            self.update_selection();
            SetFocus(self.controls.url);
            return false;
        }
        if urls.len() > 200 {
            self.error("เพิ่มได้ไม่เกิน 200 ลิงก์ต่อครั้ง");
            return false;
        }
        if hash.is_some() && urls.len() != 1 {
            self.error("ค่า SHA-256 ใช้กับการเพิ่มหนึ่งไฟล์ต่อครั้ง");
            return false;
        }
        if urls.iter().any(|url| {
            !url.to_ascii_lowercase().starts_with("http://")
                && !url.to_ascii_lowercase().starts_with("https://")
        }) {
            self.error("ลิงก์ดาวน์โหลดต้องเริ่มด้วย http:// หรือ https://");
            return false;
        }
        if let Some(h) = &hash {
            if h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
                self.error("SHA-256 ต้องมีอักขระฐานสิบหก 64 ตัว");
                return false;
            }
        }
        for url in urls {
            self.send(Command::Add(AddRequest {
                url: url.to_string(),
                expected_sha256: hash.clone(),
                start,
                ..Default::default()
            }));
        }
        true
    }
    unsafe fn key(&mut self, message: &MSG) -> bool {
        let key = message.wParam as u16;
        let ctrl = (GetKeyState(VK_CONTROL as i32) as u16 & 0x8000) != 0;
        if self.aux.is_some() {
            if key == VK_ESCAPE {
                self.close_aux();
                return true;
            }
            if key == VK_RETURN && ctrl {
                self.aux_action(ID_AUX_SAVE);
                return true;
            }
            if ctrl && key == b'A' as u16 {
                let focus = GetFocus();
                let mut class = [0u16; 32];
                let len =
                    GetClassNameW(focus, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
                if String::from_utf16_lossy(&class[..len]).eq_ignore_ascii_case("Edit") {
                    SendMessageW(focus, EM_SETSEL, 0, -1);
                    return true;
                }
            }
            return false;
        }
        if ctrl && key == b'N' as u16 {
            SetFocus(self.controls.url);
            SendMessageW(self.controls.url, EM_SETSEL, 0, -1);
            return true;
        }
        if ctrl && key == b'A' as u16 && GetFocus() == self.controls.url {
            SendMessageW(self.controls.url, EM_SETSEL, 0, -1);
            return true;
        }
        if key == VK_RETURN && GetFocus() == self.controls.url {
            self.action(ID_ADD);
            return true;
        }
        if GetFocus() == self.controls.list {
            if key == VK_DELETE {
                self.action(ID_REMOVE);
                return true;
            }
            if key == VK_SPACE {
                if let Some(t) = self.selected() {
                    if active(&t.status) || t.status == "queued" {
                        self.action(ID_PAUSE);
                    } else if can_resume(&t.status) {
                        self.action(ID_RESUME);
                    }
                }
                return true;
            }
            if key == VK_RETURN {
                self.action(ID_DETAILS);
                return true;
            }
            if ctrl && key == b'R' as u16 {
                self.action(ID_REFRESH);
                return true;
            }
        }
        false
    }
    unsafe fn main_menu(&mut self) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (id, label) in [
            (ID_SETTINGS, "ตั้งค่า…"),
            (ID_ADD_ADVANCED, "เพิ่มหลายลิงก์ / SHA-256…"),
            (ID_INTEGRATION, "เชื่อมต่อเบราว์เซอร์…"),
            (ID_DOWNLOAD_FOLDER, "เปิดโฟลเดอร์ดาวน์โหลด"),
        ] {
            AppendMenuW(menu, MF_STRING, id, wide(label).as_ptr());
        }
        AppendMenuW(menu, MF_SEPARATOR, 0, null());
        for (id, label) in [
            (ID_PAUSE_ALL, "พักทั้งหมด"),
            (ID_RESUME_ALL, "โหลดต่อทั้งหมด"),
            (ID_QUIT, "ออกจาก QDM"),
        ] {
            AppendMenuW(menu, MF_STRING, id, wide(label).as_ptr());
        }
        let mut r: RECT = zeroed();
        GetWindowRect(self.controls.menu, &mut r);
        let picked = TrackPopupMenu(
            menu,
            TPM_RETURNCMD | TPM_RIGHTALIGN,
            r.right,
            r.bottom,
            0,
            self.hwnd,
            null(),
        );
        DestroyMenu(menu);
        if picked != 0 {
            self.action(picked as usize);
        }
    }
    unsafe fn context_menu(&mut self) {
        if self.selected().is_none() {
            return;
        }
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (id, label) in [
            (ID_PAUSE, "พัก"),
            (ID_RESUME, "โหลดต่อ"),
            (ID_REFRESH, "เปลี่ยนลิงก์…"),
            (ID_OPEN, "เปิดไฟล์"),
            (ID_FOLDER, "เปิดโฟลเดอร์"),
            (ID_DETAILS, "รายละเอียด / SHA-256…"),
            (ID_REMOVE, "ลบรายการ"),
        ] {
            let control = match id {
                ID_PAUSE => self.controls.pause,
                ID_RESUME => self.controls.resume,
                ID_REFRESH => self.controls.refresh,
                ID_OPEN => self.controls.open,
                _ => null_mut(),
            };
            let flags = MF_STRING
                | if control.is_null() || IsWindowEnabled(control) != 0 {
                    0
                } else {
                    MF_GRAYED
                };
            AppendMenuW(menu, flags, id, wide(label).as_ptr());
        }
        let mut p: POINT = zeroed();
        GetCursorPos(&mut p);
        let picked = TrackPopupMenu(menu, TPM_RETURNCMD, p.x, p.y, 0, self.hwnd, null());
        DestroyMenu(menu);
        if picked != 0 {
            self.action(picked as usize);
        }
    }
    unsafe fn show(&self) {
        if IsIconic(self.hwnd) != 0 {
            ShowWindow(self.hwnd, SW_RESTORE);
        } else {
            ShowWindow(self.hwnd, SW_SHOW);
        }
        SetForegroundWindow(self.hwnd);
    }
    unsafe fn quit(&mut self) {
        self.quitting = true;
        self.close_aux();
        self.remove_tray();
        self.send(Command::Quit);
        DestroyWindow(self.hwnd);
    }
    unsafe fn create_tray(&mut self) {
        let mut n: NOTIFYICONDATAW = zeroed();
        n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = self.hwnd;
        n.uID = 1;
        n.uFlags = NIF_ICON | NIF_MESSAGE | NIF_TIP;
        n.uCallbackMessage = WM_TRAY;
        n.hIcon = LoadIconW(self.instance, 101usize as *const u16);
        if n.hIcon.is_null() {
            n.hIcon = LoadIconW(null_mut(), IDI_APPLICATION);
        }
        let tip = wide(TITLE);
        let len = tip.len().min(n.szTip.len());
        n.szTip[..len].copy_from_slice(&tip[..len]);
        self.tray = Shell_NotifyIconW(NIM_ADD, &n) != 0;
    }
    unsafe fn remove_tray(&mut self) {
        if self.tray {
            let mut n: NOTIFYICONDATAW = zeroed();
            n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
            n.hWnd = self.hwnd;
            n.uID = 1;
            Shell_NotifyIconW(NIM_DELETE, &n);
            self.tray = false;
        }
    }
    unsafe fn tray_notice(&self, title: &str, message: &str) {
        let mut n: NOTIFYICONDATAW = zeroed();
        n.cbSize = size_of::<NOTIFYICONDATAW>() as u32;
        n.hWnd = self.hwnd;
        n.uID = 1;
        n.uFlags = NIF_INFO;
        n.dwInfoFlags = NIIF_INFO | NIIF_NOSOUND;
        let title = wide(title);
        let text = wide(message);
        let a = title.len().min(n.szInfoTitle.len() - 1);
        let b = text.len().min(n.szInfo.len() - 1);
        n.szInfoTitle[..a].copy_from_slice(&title[..a]);
        n.szInfo[..b].copy_from_slice(&text[..b]);
        Shell_NotifyIconW(NIM_MODIFY, &n);
    }
    unsafe fn tray_menu(&mut self) {
        let menu = CreatePopupMenu();
        if menu.is_null() {
            return;
        }
        for (id, label) in [
            (ID_SHOW, "เปิด QDM"),
            (ID_PAUSE_ALL, "พักทั้งหมด"),
            (ID_RESUME_ALL, "โหลดต่อทั้งหมด"),
            (ID_QUIT, "ออกจาก QDM"),
        ] {
            AppendMenuW(menu, MF_STRING, id, wide(label).as_ptr());
        }
        let mut p: POINT = zeroed();
        GetCursorPos(&mut p);
        SetForegroundWindow(self.hwnd);
        let result = TrackPopupMenu(menu, TPM_RETURNCMD, p.x, p.y, 0, self.hwnd, null());
        DestroyMenu(menu);
        PostMessageW(self.hwnd, WM_NULL, 0, 0);
        if result != 0 {
            self.action(result as usize);
        }
    }
    unsafe fn error(&self, message: &str) {
        MessageBoxW(
            self.aux.as_ref().map_or(self.hwnd, |a| a.hwnd),
            wide(message).as_ptr(),
            wide("QDM").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
    unsafe fn begin_aux(&mut self, title: &str, width: i32, height: i32) -> Option<HWND> {
        if let Some(aux) = &self.aux {
            SetForegroundWindow(aux.hwnd);
            return None;
        }
        let mut parent: RECT = zeroed();
        GetWindowRect(self.hwnd, &mut parent);
        let w = scale(width, self.dpi);
        let h = scale(height, self.dpi);
        let hwnd = CreateWindowExW(
            WS_EX_DLGMODALFRAME,
            wide(AUX_CLASS).as_ptr(),
            wide(title).as_ptr(),
            WS_POPUP | WS_CAPTION | WS_SYSMENU | WS_CLIPCHILDREN,
            parent.left + (parent.right - parent.left - w) / 2,
            parent.top + (parent.bottom - parent.top - h) / 2,
            w,
            h,
            self.hwnd,
            null_mut(),
            self.instance,
            self.hwnd.cast(),
        );
        if hwnd.is_null() {
            self.error("ไม่สามารถเปิดหน้าต่างได้");
            return None;
        }
        EnableWindow(self.hwnd, 0);
        Some(hwnd)
    }
    unsafe fn finish_aux(&mut self, hwnd: HWND, focus: HWND) {
        if let Some(aux) = &mut self.aux {
            aux.font = make_font(aux.dpi);
            let context = ResizeChildren {
                parent: hwnd,
                old_dpi: aux.dpi,
                new_dpi: aux.dpi,
                font: aux.font,
            };
            EnumChildWindows(
                hwnd,
                Some(resize_child),
                &context as *const ResizeChildren as isize,
            );
        }
        ShowWindow(hwnd, SW_SHOW);
        SetForegroundWindow(hwnd);
        SetFocus(focus);
    }
    unsafe fn resize_aux(&mut self, dpi: u32, rect: &RECT) {
        let Some(aux) = &mut self.aux else {
            return;
        };
        let font = make_font(dpi);
        let context = ResizeChildren {
            parent: aux.hwnd,
            old_dpi: aux.dpi,
            new_dpi: dpi,
            font,
        };
        SetWindowPos(
            aux.hwnd,
            null_mut(),
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            SWP_NOZORDER | SWP_NOACTIVATE,
        );
        EnumChildWindows(
            aux.hwnd,
            Some(resize_child),
            &context as *const ResizeChildren as isize,
        );
        if !font.is_null() {
            if !aux.font.is_null() {
                DeleteObject(aux.font);
            }
            aux.font = font;
        }
        aux.dpi = dpi;
    }
    unsafe fn close_aux(&mut self) {
        if let Some(aux) = self.aux.take() {
            DestroyWindow(aux.hwnd);
            if !aux.font.is_null() {
                DeleteObject(aux.font);
            }
            EnableWindow(self.hwnd, 1);
            SetForegroundWindow(self.hwnd);
            SetFocus(self.controls.url);
        }
    }
    unsafe fn settings(&mut self) {
        let Some(hwnd) = self.begin_aux("ตั้งค่า QDM", 610, 424) else {
            return;
        };
        let s = &self.snapshot.settings;
        let l1 = self.label(hwnd, "การเชื่อมต่อต่อไฟล์", 1301);
        self.place(l1, 24, 22, 260, 22);
        let connections = self.control(
            hwnd,
            "COMBOBOX",
            "",
            WS_TABSTOP | CBS_DROPDOWNLIST as u32 | WS_VSCROLL,
            0,
            1302,
        );
        self.place(connections, 300, 17, 270, 220);
        let mut values = vec![1u8, 4, 8, 16, 32, 64];
        if !values.contains(&s.connections) {
            values.push(s.connections);
            values.sort_unstable();
        }
        for (v, i) in values.iter().zip(0..) {
            let text = wide(&format!("{} การเชื่อมต่อ", v));
            SendMessageW(connections, CB_ADDSTRING, 0, text.as_ptr() as isize);
            if *v == s.connections {
                SendMessageW(connections, CB_SETCURSEL, i, 0);
            }
        }
        let l2 = self.label(hwnd, "ไฟล์ที่ดาวน์โหลดพร้อมกัน", 1303);
        self.place(l2, 24, 72, 260, 22);
        let concurrent = self.edit(hwnd, &s.max_concurrent.to_string(), 1304, ES_NUMBER as u32);
        self.place(concurrent, 300, 66, 270, 30);
        let l3 = self.label(hwnd, "จำกัดความเร็วรวม (KiB/s)", 1305);
        self.place(l3, 24, 118, 260, 22);
        let speed = self.edit(hwnd, &s.speed_limit_kib.to_string(), 1306, ES_NUMBER as u32);
        self.place(speed, 300, 112, 270, 30);
        let hint = self.label(hwnd, "0 = ไม่จำกัดความเร็ว", 1307);
        self.place(hint, 300, 147, 270, 22);
        let l4 = self.label(hwnd, "โฟลเดอร์สำหรับงานใหม่", 1308);
        self.place(l4, 24, 183, 540, 22);
        let directory = self.edit(hwnd, &s.download_dir, 1309, 0);
        self.place(directory, 24, 210, 448, 32);
        let browse = self.button(hwnd, "เลือก…", ID_AUX_BROWSE);
        self.place(browse, 482, 210, 88, 32);
        let close_to_tray = self.control(
            hwnd,
            "BUTTON",
            "ทำงานต่อในถาดระบบเมื่อปิดหน้าต่าง",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            0,
            1310,
        );
        self.place(close_to_tray, 24, 262, 546, 26);
        SendMessageW(
            close_to_tray,
            BM_SETCHECK,
            if s.close_to_tray {
                BST_CHECKED as usize
            } else {
                BST_UNCHECKED as usize
            },
            0,
        );
        let note = self.label(hwnd, "จำนวนการเชื่อมต่อจริงขึ้นอยู่กับไฟล์และเซิร์ฟเวอร์", 1311);
        self.place(note, 24, 302, 546, 24);
        let save = self.button(hwnd, "บันทึก", ID_AUX_SAVE);
        self.place(save, 370, 344, 96, 32);
        let cancel = self.button(hwnd, "ยกเลิก", ID_AUX_CANCEL);
        self.place(cancel, 476, 344, 94, 32);
        self.aux = Some(Aux {
            hwnd,
            dpi: self.dpi,
            font: null_mut(),
            kind: AuxKind::Settings(SettingsFields {
                connections,
                concurrent,
                speed,
                directory,
                close_to_tray,
            }),
        });
        self.finish_aux(hwnd, connections);
    }
    unsafe fn refresh(&mut self, task: &TaskView) {
        let Some(hwnd) = self.begin_aux("เปลี่ยนลิงก์ดาวน์โหลด", 650, 260)
        else {
            return;
        };
        let title = self.label(hwnd, &task.filename, 1401);
        self.place(title, 24, 22, 594, 24);
        let label = self.label(hwnd, "ลิงก์ใหม่", 1402);
        self.place(label, 24, 60, 594, 22);
        let entry = self.edit(hwnd, &task.url, 1403, 0);
        self.place(entry, 24, 86, 594, 32);
        let hint = self.label(hwnd, "QDM จะตรวจข้อมูลต้นทางก่อนนำส่วนที่ดาวน์โหลดไว้กลับมาใช้", 1404);
        self.place(hint, 24, 132, 594, 40);
        let save = self.button(hwnd, "เปลี่ยนแล้วโหลดต่อ", ID_AUX_SAVE);
        self.place(save, 346, 184, 160, 32);
        let cancel = self.button(hwnd, "ยกเลิก", ID_AUX_CANCEL);
        self.place(cancel, 516, 184, 102, 32);
        self.aux = Some(Aux {
            hwnd,
            dpi: self.dpi,
            font: null_mut(),
            kind: AuxKind::Refresh {
                id: task.id.clone(),
                entry,
            },
        });
        self.finish_aux(hwnd, entry);
        SendMessageW(entry, EM_SETSEL, 0, -1);
    }
    unsafe fn advanced_add(&mut self) {
        let Some(hwnd) = self.begin_aux("เพิ่มดาวน์โหลด", 650, 420)
        else {
            return;
        };
        let label = self.label(hwnd, "ลิงก์ดาวน์โหลด — หนึ่งลิงก์ต่อบรรทัด", 1501);
        self.place(label, 24, 20, 594, 24);
        let urls = self.edit(
            hwnd,
            &get_text(self.controls.url),
            1502,
            ES_MULTILINE as u32 | ES_AUTOVSCROLL as u32 | ES_WANTRETURN as u32 | WS_VSCROLL,
        );
        self.place(urls, 24, 50, 594, 154);
        let label = self.label(hwnd, "SHA-256 ที่คาดหวัง (ไม่บังคับ / ใช้กับหนึ่งไฟล์)", 1503);
        self.place(label, 24, 222, 594, 24);
        let hash = self.edit(hwnd, "", 1504, 0);
        self.place(hash, 24, 251, 594, 32);
        SendMessageW(hash, EM_SETLIMITTEXT, 64, 0);
        let start = self.control(
            hwnd,
            "BUTTON",
            "เริ่มดาวน์โหลดทันที",
            WS_TABSTOP | BS_AUTOCHECKBOX as u32,
            0,
            1505,
        );
        self.place(start, 24, 299, 594, 26);
        SendMessageW(start, BM_SETCHECK, BST_CHECKED as usize, 0);
        let save = self.button(hwnd, "เพิ่มดาวน์โหลด", ID_AUX_SAVE);
        self.place(save, 374, 344, 134, 32);
        let cancel = self.button(hwnd, "ยกเลิก", ID_AUX_CANCEL);
        self.place(cancel, 518, 344, 100, 32);
        self.aux = Some(Aux {
            hwnd,
            dpi: self.dpi,
            font: null_mut(),
            kind: AuxKind::Add { urls, hash, start },
        });
        self.finish_aux(hwnd, urls);
    }
    unsafe fn integration(&mut self, port: u16, token: String, extension_path: String) {
        self.close_aux();
        let Some(hwnd) = self.begin_aux("เชื่อมต่อเบราว์เซอร์", 650, 332)
        else {
            return;
        };
        let hint = self.label(
            hwnd,
            "โหลดส่วนขยาย QDM แล้วกรอกพอร์ตและรหัสจับคู่ในหน้าตั้งค่าของส่วนขยาย",
            1601,
        );
        self.place(hint, 24, 22, 594, 46);
        let port_label = self.label(hwnd, &format!("พอร์ต: {port}"), 1602);
        self.place(port_label, 24, 80, 594, 24);
        let label = self.label(hwnd, "รหัสจับคู่", 1603);
        self.place(label, 24, 118, 594, 22);
        let secret = self.edit(hwnd, &token, 1604, ES_READONLY as u32 | ES_PASSWORD as u32);
        self.place(secret, 24, 146, 456, 32);
        let copy = self.button(hwnd, "คัดลอกรหัส", ID_AUX_COPY);
        self.place(copy, 490, 146, 128, 32);
        let extensions = self.button(hwnd, "เปิดโฟลเดอร์ส่วนขยาย", ID_AUX_EXTENSIONS);
        self.place(extensions, 24, 202, 230, 32);
        let close = self.button(hwnd, "ปิด", ID_AUX_CANCEL);
        self.place(close, 518, 258, 100, 32);
        self.aux = Some(Aux {
            hwnd,
            dpi: self.dpi,
            font: null_mut(),
            kind: AuxKind::Integration {
                token,
                extension_path,
            },
        });
        self.finish_aux(hwnd, copy);
    }
    unsafe fn details(&mut self, task: &TaskView) {
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

    unsafe fn aux_action(&mut self, id: usize) {
        if id == ID_AUX_CANCEL {
            self.close_aux();
            return;
        }
        let Some(aux) = &self.aux else {
            return;
        };
        if id == ID_AUX_BROWSE {
            if let AuxKind::Settings(fields) = &aux.kind {
                let owner = aux.hwnd;
                let entry = fields.directory;
                if let Some(path) = choose_folder(owner) {
                    SetWindowTextW(entry, wide(&path).as_ptr());
                }
            }
            return;
        }
        if id == ID_AUX_COPY {
            if let AuxKind::Integration { token, .. } = &aux.kind {
                if !copy_text(aux.hwnd, token) {
                    self.error("คัดลอกรหัสไม่สำเร็จ กรุณาลองอีกครั้ง");
                }
            }
            return;
        }
        if id == ID_AUX_EXTENSIONS {
            if let AuxKind::Integration { extension_path, .. } = &aux.kind {
                shell_open(aux.hwnd, extension_path);
            }
            return;
        }
        if id != ID_AUX_SAVE {
            return;
        }
        match &aux.kind {
            AuxKind::Settings(fields) => {
                let selected = get_text(fields.connections)
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .parse::<u8>();
                let concurrent = get_text(fields.concurrent).trim().parse::<u8>();
                let speed = get_text(fields.speed).trim().parse::<u64>();
                let directory = get_text(fields.directory).trim().to_string();
                if !selected.as_ref().is_ok_and(|n| (1..=64).contains(n)) {
                    self.error("เลือกจำนวนการเชื่อมต่อระหว่าง 1–64");
                    return;
                }
                if !concurrent.as_ref().is_ok_and(|n| (1..=8).contains(n)) {
                    self.error("จำนวนไฟล์พร้อมกันต้องอยู่ระหว่าง 1–8");
                    return;
                }
                let Ok(speed) = speed else {
                    self.error("ระบุความเร็วเป็นตัวเลข ตั้ง 0 เพื่อไม่จำกัด");
                    return;
                };
                if directory.is_empty() || !Path::new(&directory).is_absolute() {
                    self.error("เลือกโฟลเดอร์ดาวน์โหลดที่เป็นพาธเต็ม");
                    return;
                }
                let mut settings: Settings = self.snapshot.settings.clone();
                settings.connections = selected.unwrap_or(8);
                settings.max_concurrent = concurrent.unwrap_or(2);
                settings.speed_limit_kib = speed;
                settings.download_dir = directory;
                settings.close_to_tray =
                    SendMessageW(fields.close_to_tray, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                self.send(Command::UpdateSettings(settings));
                self.close_aux();
            }
            AuxKind::Refresh { id, entry } => {
                let url = get_text(*entry).trim().to_string();
                if !url.to_ascii_lowercase().starts_with("http://")
                    && !url.to_ascii_lowercase().starts_with("https://")
                {
                    self.error("ลิงก์ต้องเริ่มด้วย http:// หรือ https://");
                    return;
                }
                let id = id.clone();
                self.send(Command::ReplaceUrl { id, url });
                self.close_aux();
            }
            AuxKind::Add { urls, hash, start } => {
                let urls = get_text(*urls);
                let hash = get_text(*hash).trim().to_string();
                let start = SendMessageW(*start, BM_GETCHECK, 0, 0) == BST_CHECKED as isize;
                if self.add_urls(
                    &urls,
                    if hash.is_empty() {
                        None
                    } else {
                        Some(hash.to_ascii_lowercase())
                    },
                    start,
                ) {
                    self.close_aux();
                    SetWindowTextW(self.controls.url, wide("").as_ptr());
                }
            }
            _ => self.close_aux(),
        }
    }
}

unsafe fn make_font(dpi: u32) -> HFONT {
    CreateFontW(
        -(((11 * dpi + 36) / 72) as i32),
        0,
        0,
        0,
        FW_NORMAL as i32,
        0,
        0,
        0,
        DEFAULT_CHARSET as u32,
        OUT_DEFAULT_PRECIS as u32,
        CLIP_DEFAULT_PRECIS as u32,
        CLEARTYPE_QUALITY as u32,
        DEFAULT_PITCH as u32,
        wide("Segoe UI").as_ptr(),
    )
}

struct ResizeChildren {
    parent: HWND,
    old_dpi: u32,
    new_dpi: u32,
    font: HFONT,
}
unsafe extern "system" fn resize_child(hwnd: HWND, data: LPARAM) -> i32 {
    let context = &*(data as *const ResizeChildren);
    if GetParent(hwnd) != context.parent {
        return 1;
    }
    if context.old_dpi != context.new_dpi {
        let mut rect: RECT = zeroed();
        GetWindowRect(hwnd, &mut rect);
        let mut origin = POINT {
            x: rect.left,
            y: rect.top,
        };
        ScreenToClient(context.parent, &mut origin);
        let mut height = rect.bottom - rect.top;
        let mut class = [0u16; 32];
        let len = GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
        if String::from_utf16_lossy(&class[..len]).eq_ignore_ascii_case("ComboBox") {
            let mut dropped: RECT = zeroed();
            if SendMessageW(
                hwnd,
                CB_GETDROPPEDCONTROLRECT,
                0,
                &mut dropped as *mut RECT as isize,
            ) != 0
            {
                height = dropped.bottom - dropped.top;
            }
        }
        let factor =
            |v: i32| ((v as i64 * context.new_dpi as i64) / context.old_dpi.max(1) as i64) as i32;
        MoveWindow(
            hwnd,
            factor(origin.x),
            factor(origin.y),
            factor(rect.right - rect.left),
            factor(height),
            1,
        );
    }
    if !context.font.is_null() {
        SendMessageW(hwnd, WM_SETFONT, context.font as usize, 1);
    }
    1
}

unsafe fn get_text(hwnd: HWND) -> String {
    let len = GetWindowTextLengthW(hwnd).max(0) as usize;
    let mut buffer = vec![0u16; len + 1];
    let read = GetWindowTextW(hwnd, buffer.as_mut_ptr(), buffer.len() as i32).max(0) as usize;
    String::from_utf16_lossy(&buffer[..read])
}
unsafe fn shell_open(owner: HWND, path: &str) {
    let result = ShellExecuteW(
        owner,
        wide("open").as_ptr(),
        wide(path).as_ptr(),
        null(),
        null(),
        SW_SHOWNORMAL,
    );
    if result as usize <= 32 {
        MessageBoxW(
            owner,
            wide("ไม่สามารถเปิดตำแหน่งนี้ได้").as_ptr(),
            wide("QDM").as_ptr(),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}
unsafe fn choose_folder(owner: HWND) -> Option<String> {
    let mut display = [0u16; 260];
    let title = wide("เลือกโฟลเดอร์ดาวน์โหลด");
    let info = BROWSEINFOW {
        hwndOwner: owner,
        pszDisplayName: display.as_mut_ptr(),
        lpszTitle: title.as_ptr(),
        ulFlags: BIF_RETURNONLYFSDIRS | BIF_NEWDIALOGSTYLE,
        ..zeroed()
    };
    let list = SHBrowseForFolderW(&info);
    if list.is_null() {
        return None;
    }
    let mut path = [0u16; 32_768];
    let success = SHGetPathFromIDListEx(list, path.as_mut_ptr(), path.len() as u32, 0);
    CoTaskMemFree(list.cast());
    if success == 0 {
        None
    } else {
        Some(String::from_utf16_lossy(
            &path[..path.iter().position(|c| *c == 0).unwrap_or(path.len())],
        ))
    }
}
unsafe fn copy_text(owner: HWND, text: &str) -> bool {
    let value = wide(text);
    let memory = GlobalAlloc(GMEM_MOVEABLE, value.len() * 2);
    if memory.is_null() {
        return false;
    }
    let destination = GlobalLock(memory);
    if destination.is_null() {
        GlobalFree(memory);
        return false;
    }
    std::ptr::copy_nonoverlapping(value.as_ptr(), destination.cast::<u16>(), value.len());
    GlobalUnlock(memory);
    if OpenClipboard(owner) == 0 {
        GlobalFree(memory);
        return false;
    }
    if EmptyClipboard() == 0 {
        CloseClipboard();
        GlobalFree(memory);
        return false;
    }
    let ok = !SetClipboardData(CF_UNICODETEXT_VALUE, memory).is_null();
    CloseClipboard();
    if !ok {
        GlobalFree(memory);
    }
    ok
}
