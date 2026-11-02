//! Slint presentation layer. Network, hashing and persistence stay on the engine
//! threads. A bounded mailbox coalesces snapshots; stable models preserve scroll,
//! selection and interaction animations while telemetry changes.
#![cfg(windows)]

use crate::api::{normalize_queue_name, AddRequest, Command, QueueConfig, Settings, Snapshot, TaskView, UiEvent, MAX_QUEUE_COUNT, MAX_SPEED_LIMIT_KIB};
use crate::clipboard::{display_name, ClipboardWatcher};
use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use serde::{Deserialize, Serialize};
use slint::{ComponentHandle, Model, ModelRc, SharedString, Timer, TimerMode, VecModel};
use std::{
    cell::{Cell, RefCell},
    collections::{BTreeMap, HashMap, HashSet, VecDeque},
    hash::{Hash, Hasher},
    path::{Path, PathBuf},
    rc::Rc,
    sync::{mpsc::{Receiver, Sender}, Arc, Mutex},
    time::{Duration, Instant},
};
use windows_sys::Win32::{
    Foundation::{GlobalFree, HWND, RECT},
    Graphics::{
        Dwm::DwmSetWindowAttribute,
        Gdi::{GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST},
    },
    System::{DataExchange::*, Memory::*},
    UI::WindowsAndMessaging::*,
};

slint::include_modules!();

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
struct Appearance { reduced_motion: bool }

#[derive(Default)]
struct Mailbox { latest: Option<Snapshot>, events: Vec<UiEvent>, disconnected: bool }

struct ViewState {
    snapshot: Snapshot,
    speed_history: HashMap<String, VecDeque<u64>>,
    last_speed_sample: Option<Instant>,
    selected: String,
    checked: HashSet<String>,
    filter: i32,
    // None until the initial engine state is loaded; avoid treating history as newly arrived.
    known_ids: Option<HashSet<String>>,
    toast_until: Option<Instant>,
    preferences: PathBuf,
    // Phase 4 clipboard link detection: pure decision state plus the last observed
    // OS clipboard change counter used to debounce text reads.
    clipboard: ClipboardWatcher,
    clipboard_seq: u64,
    // Phase 2 incremental render: the display order only changes with membership or
    // filter, so we hash it and refresh only rows whose render signature moved.
    order_key: u64,
    row_sig: Vec<u64>,
    row_avail: Vec<bool>,
}

fn human(value: u64) -> String {
    let mut n = value as f64;
    let mut index = 0;
    let units = ["B", "KiB", "MiB", "GiB", "TiB"];
    while n >= 1024. && index < units.len() - 1 { n /= 1024.; index += 1; }
    if index == 0 { format!("{value} B") } else { format!("{n:.1} {}", units[index]) }
}
fn fraction(done: u64, total: u64) -> f32 {
    if total == 0 { 0. } else { (done as f64 / total as f64).clamp(0., 1.) as f32 }
}
fn running(status: &str) -> bool {
    matches!(status, "queued" | "downloading" | "preparing" | "probing" | "assembling" | "verifying")
}
fn resumable_status(status: &str) -> bool {
    matches!(status, "paused" | "error" | "failed" | "scheduled")
}
// The Active view includes paused and recoverable tasks so their per-file Resume stays reachable.
fn visible_in_filter(status: &str, filter: i32) -> bool {
    match filter {
        1 => running(status) || resumable_status(status),
        2 => status == "completed",
        _ => true,
    }
}
fn status_label(status: &str) -> &'static str {
    match status {
        "downloading" => "DOWNLOADING", "queued" => "QUEUED", "paused" => "PAUSED",
        "completed" => "Complete", "assembling" | "merging" => "COMBINING FILE",
        "verifying" => "VERIFYING", "error" | "failed" => "NEEDS ATTENTION",
        "scheduled" => "SCHEDULED", "preparing" | "probing" => "CONNECTING", _ => "READY",
    }
}
fn selected_notice(task: &TaskView) -> &str {
    if let Some(error) = task.error.as_deref() { return error; }
    match task.notice.as_deref() {
        // Older completed records retained this assembly-phase message.
        // Hide only that obsolete presentation; leave history and warnings intact.
        Some("Combining verified partial files into one output...") if task.status == "completed" => "",
        Some(notice) => notice,
        None => "",
    }
}
fn eta(task: &TaskView) -> String {
    if task.status == "completed" { return "Complete".into(); }
    let Some(total) = task.total_bytes else { return "Time remaining —".into(); };
    if task.speed_bps == 0 || task.status != "downloading" { return "Time remaining —".into(); }
    let seconds = total.saturating_sub(task.downloaded_bytes).div_ceil(task.speed_bps);
    if seconds >= 3600 { format!("{}h {}m left", seconds / 3600, (seconds % 3600) / 60) }
    else if seconds >= 60 { format!("{}m {}s left", seconds / 60, seconds % 60) }
    else { format!("{seconds}s left") }
}
fn host(url: &str) -> String {
    url::Url::parse(url).ok().and_then(|u| u.host_str().map(str::to_owned)).unwrap_or_else(|| "Remote server".into())
}

fn sync_model<T: Clone + PartialEq + 'static>(model: &VecModel<T>, rows: Vec<T>) {
    while model.row_count() > rows.len() { model.remove(model.row_count() - 1); }
    for (index, row) in rows.into_iter().enumerate() {
        if index == model.row_count() { model.push(row); }
        else if model.row_data(index).as_ref() != Some(&row) { model.set_row_data(index, row); }
    }
}

fn newest_first(downloads: &[TaskView]) -> Vec<&TaskView> {
    let mut ordered: Vec<(usize, &TaskView)> = downloads.iter().enumerate().collect();
    // Created at is monotonic milliseconds. For exact ties, the later input entry wins.
    ordered.sort_unstable_by(|(left_index, left), (right_index, right)| {
        right.created_at.cmp(&left.created_at).then_with(|| right_index.cmp(left_index))
    });
    ordered.into_iter().map(|(_, task)| task).collect()
}

// Detect new engine task identities, not changes in progress or list iteration order.
// Covers links captured from browsers/native integrations as well as direct UI additions.
fn newest_unseen_id(
    known_ids: Option<&HashSet<String>>,
    tasks: &[TaskView],
) -> Option<String> {
    let known_ids = known_ids?; // first snapshot establishes a baseline; don't relabel history
    tasks.iter().enumerate()
        .filter(|(_, task)| !task.id.is_empty() && !known_ids.contains(&task.id))
        .max_by_key(|(index, task)| (task.created_at, *index))
        .map(|(_, task)| task.id.clone())
}

fn rate_sparkline(samples: Option<&VecDeque<u64>>) -> String {
    let Some(samples) = samples else { return String::new(); };
    if samples.len() < 2 { return String::new(); }
    let peak = samples.iter().copied().max().unwrap_or(0).max(1) as f64;
    let count = samples.len();
    samples.iter().enumerate().map(|(i, value)| {
        let x = i as f64 * 100.0 / (count - 1) as f64;
        let y = 34.0 - (*value as f64 / peak) * 32.0;
        format!("{} {:.2} {:.2}", if i == 0 { "M" } else { "L" }, x, y)
    }).collect::<Vec<_>>().join(" ")
}

// A row's rendered presentation depends only on these fields. Building the
// DownloadItem is relatively expensive (several String allocations), so we hash the
// inputs and rebuild only rows whose signature actually moved on a telemetry tick.
fn render_signature(task: &TaskView, checked: bool, file_available: bool) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    task.filename.hash(&mut hasher);
    task.url.hash(&mut hasher);
    task.status.hash(&mut hasher);
    task.downloaded_bytes.hash(&mut hasher);
    task.total_bytes.hash(&mut hasher);
    task.speed_bps.hash(&mut hasher);
    task.assembly_bytes.hash(&mut hasher);
    task.assembly_total_bytes.hash(&mut hasher);
    checked.hash(&mut hasher);
    file_available.hash(&mut hasher);
    hasher.finish()
}

fn build_item(task: &TaskView, checked: bool, file_available: bool) -> DownloadItem {
    let progress = task.total_bytes.map(|n| fraction(task.downloaded_bytes, n)).unwrap_or(0.);
    let stage_visible = matches!(task.status.as_str(), "assembling" | "merging" | "verifying");
    let stage_indeterminate = task.assembly_total_bytes == 0;
    let stage_progress = fraction(task.assembly_bytes, task.assembly_total_bytes);
    DownloadItem {
        id: task.id.clone().into(), name: task.filename.clone().into(),
        subtitle: format!("{} · {}{}", host(&task.url), human(task.downloaded_bytes), if task.speed_bps > 0 { format!(" · {}/s", human(task.speed_bps)) } else { String::new() }).into(),
        status: status_label(&task.status).into(), progress,
        progress_text: task.total_bytes.map(|_| format!("{:.1}%", progress * 100.)).unwrap_or_else(|| "Size unknown".into()).into(),
        active: running(&task.status), can_pause: running(&task.status), can_resume: resumable_status(&task.status), completed: task.status == "completed", checked, file_available, failed: matches!(task.status.as_str(), "error" | "failed"),
        stage_visible, stage_indeterminate, stage_progress,
        stage_text: if stage_indeterminate { "VERIFYING".into() }
            else { format!("CHECK {:.0}%", stage_progress * 100.0).into() },
    }
}

fn render(ui: &QdmWindow, state: &mut ViewState, jobs: &VecModel<DownloadItem>, lanes: &VecModel<LaneItem>) {
    let snapshot = &state.snapshot;
    if !snapshot.downloads.iter().any(|task| task.id == state.selected) {
        state.selected = snapshot.downloads.iter().filter(|t| running(&t.status))
            .max_by_key(|t| t.created_at)
            .or_else(|| snapshot.downloads.iter().max_by_key(|t| t.created_at))
            .map(|t| t.id.clone()).unwrap_or_default();
    }
    state.checked.retain(|id| snapshot.downloads.iter().any(|t| t.id == *id && t.status == "completed"));
    ui.set_checked_count(state.checked.len() as i32);
    ui.set_transfer_active(snapshot.downloads.iter().any(|t| matches!(t.status.as_str(), "downloading" | "preparing" | "probing" | "connecting" | "assembling" | "merging" | "verifying")));
    ui.set_selected_id(state.selected.clone().into());
    if state.last_speed_sample.map_or(true, |t| t.elapsed() >= Duration::from_secs(1)) {
        state.last_speed_sample = Some(Instant::now());
        let known: HashSet<_> = snapshot.downloads.iter().map(|t| t.id.as_str()).collect();
        state.speed_history.retain(|id, _| known.contains(id.as_str()));
        for task in &snapshot.downloads {
            // Keep independent samples per transfer; selecting a different
            // file must not show the aggregate speed graph of other jobs.
            if task.status == "downloading" || state.speed_history.contains_key(&task.id) {
                let series = state.speed_history.entry(task.id.clone()).or_default();
                if series.len() == 60 { series.pop_front(); }
                series.push_back(task.speed_bps);
            }
        }
    }
    ui.set_speed_path(rate_sparkline(state.speed_history.get(&state.selected)).into());
    ui.set_aggregate_speed(format!("{}/s", human(snapshot.total_speed_bps)).into());
    ui.set_active_label(snapshot.active_count.to_string().into());
    ui.set_completed_label(snapshot.downloads.iter().filter(|t| t.status == "completed").count().to_string().into());
    ui.set_total_label(snapshot.downloads.len().to_string().into());
    ui.set_connection_label(snapshot.settings.connections.to_string().into());
    ui.set_download_directory(snapshot.settings.download_dir.clone().into());
    let ordered: Vec<&TaskView> = newest_first(&snapshot.downloads).into_iter().filter(|task| match state.filter {
        1 => visible_in_filter(&task.status, 1), 2 => visible_in_filter(&task.status, 2), _ => true,
    }).collect();
    let order_key = {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        (state.filter, ordered.len()).hash(&mut hasher);
        for task in &ordered { task.id.hash(&mut hasher); }
        hasher.finish()
    };
    if order_key != state.order_key || jobs.row_count() != ordered.len() {
        // Membership, filter or window changed: order can shift, so rebuild the whole
        // visible model once and refresh the per-row signature/file-availability cache.
        let mut sigs = Vec::with_capacity(ordered.len());
        let mut avail = Vec::with_capacity(ordered.len());
        let items: Vec<DownloadItem> = ordered.iter().map(|task| {
            let available = task.file_path.as_ref().is_some_and(|p| Path::new(p).is_file());
            avail.push(available);
            sigs.push(render_signature(task, state.checked.contains(&task.id), available));
            build_item(task, state.checked.contains(&task.id), available)
        }).collect();
        sync_model(jobs, items);
        state.row_sig = sigs;
        state.row_avail = avail;
        state.order_key = order_key;
    } else {
        // Telemetry-only tick: order and membership are identical, so refresh just the
        // rows whose rendered output moved. The filesystem is not re-polled here.
        for (index, task) in ordered.iter().enumerate() {
            let available = state.row_avail.get(index).copied().unwrap_or(false);
            let checked = state.checked.contains(&task.id);
            let sig = render_signature(task, checked, available);
            if state.row_sig.get(index) != Some(&sig) {
                jobs.set_row_data(index, build_item(task, checked, available));
                state.row_sig[index] = sig;
            }
        }
    }
    let task = snapshot.downloads.iter().find(|task| task.id == state.selected);
    ui.set_has_selection(task.is_some());
    if let Some(task) = task {
        let progress = task.total_bytes.map(|n| fraction(task.downloaded_bytes, n)).unwrap_or(0.);
        ui.set_selected_name(task.filename.clone().into());
        ui.set_selected_host(host(&task.url).into());
        ui.set_selected_status(status_label(&task.status).into());
        ui.set_selected_bytes(format!("{} / {}", human(task.downloaded_bytes), task.total_bytes.map(human).unwrap_or_else(|| "size unknown".into())).into());
        ui.set_selected_speed(format!("{}/s", human(task.speed_bps)).into());
        ui.set_selected_eta(eta(task).into());
        ui.set_selected_connections(format!("{} / {}", task.connections_active, task.connections_limit).into());
        ui.set_selected_queue_name(if task.queue.is_empty() { "Default".into() } else { task.queue.clone().into() });
        ui.set_selected_queue_paused(snapshot.settings.queues.get(&task.queue).map(|queue| queue.paused).unwrap_or(false));
        ui.set_selected_percent(task.total_bytes.map(|_| format!("{:.1}%", progress * 100.)).unwrap_or_else(|| "—".into()).into());
        ui.set_selected_progress(progress);
        ui.set_selected_notice(selected_notice(task).into());
        ui.set_can_pause(running(&task.status));
        ui.set_can_resume(resumable_status(&task.status));
        ui.set_can_open(task.status == "completed");
        let stage = matches!(task.status.as_str(), "assembling" | "merging" | "verifying");
        ui.set_stage_visible(stage);
        ui.set_stage_indeterminate(task.assembly_total_bytes == 0);
        ui.set_stage_label(if task.assembly_total_bytes == 0 { "VERIFYING SAVED DATA" } else { "CHECKING SHA-256" }.into());
        let assembly = fraction(task.assembly_bytes, task.assembly_total_bytes);
        ui.set_stage_progress(assembly);
        ui.set_stage_bytes(if task.assembly_total_bytes == 0 { "Checking saved data".into() }
            else { format!("{:.1}% · {} / {}", assembly * 100., human(task.assembly_bytes), human(task.assembly_total_bytes)).into() });
        ui.set_lane_summary(format!("{} workers · {} receiving", task.lanes.len().min(64), task.connections_active).into());
        ui.set_lane_note(if task.lane_history_estimated {
            "Historical grouping: previous worker identity is estimated. Percent shows each assigned piece.".into()
        } else {
            format!("{} pieces · {} dynamic splits · bytes accumulated per worker; % = current piece", task.part_count, task.dynamic_splits).into()
        });
        ui.set_integrity_label(match task.integrity.as_str() {
            "sha256_verified" => "Expected SHA-256 matched".into(),
            _ if task.status == "completed" && task.sha256.is_some() => "SHA-256 calculated · no independent expected hash supplied".into(),
            _ if task.status == "completed" => "Transfer complete".into(),
            _ if task.expected_sha256.is_some() => "Expected SHA-256 will be verified".into(),
            _ if task.resumable => "Resume protection enabled".into(),
            _ => "Validating transfer".into(),
        });
        ui.set_sha256(task.sha256.clone().unwrap_or_default().into());
        let rows = task.lanes.iter().take(64).map(|lane| {
            let progress = fraction(lane.current_bytes, lane.current_total);
            let status = if lane.active { "Receiving" } else if (lane.current_total > 0 && lane.current_bytes >= lane.current_total)
                || (task.status == "completed" && lane.transferred_bytes > 0) { "Done" }
                else if lane.transferred_bytes > 0 { "Waiting" } else { "Idle" };
            LaneItem {
                label: format!("#{:02}", lane.id).into(), state: status.into(),
                bytes_text: human(lane.transferred_bytes).into(),
                piece_text: format!("{} / {}", human(lane.current_bytes), human(lane.current_total)).into(),
                percent_text: format!("{:.0}%", progress * 100.).into(), progress, active: lane.active,
            }
        }).collect();
        sync_model(lanes, rows);
    } else {
        ui.set_selected_name("Ready for your next download".into());
        ui.set_selected_host("Add a link above to get started.".into());
        ui.set_selected_status("READY".into());
        ui.set_selected_bytes("0 B / —".into()); ui.set_selected_speed("—".into());
        ui.set_selected_eta("Time remaining —".into()); ui.set_selected_percent("0%".into());
        ui.set_selected_progress(0.); ui.set_selected_connections(format!("0 / {}", snapshot.settings.connections).into());
        ui.set_selected_queue_name("".into()); ui.set_selected_queue_paused(false);
        ui.set_can_pause(false); ui.set_can_resume(false); ui.set_can_open(false);
        ui.set_stage_visible(false); ui.set_stage_indeterminate(false);
        ui.set_stage_progress(0.); ui.set_stage_label("".into()); ui.set_stage_bytes("".into());
        ui.set_selected_notice("".into());
        ui.set_lane_summary("Worker activity appears here".into());
        ui.set_lane_note("Each tile is one connection. Workers can receive several pieces.".into());
        ui.set_integrity_label("No file selected".into()); ui.set_sha256("".into());
        sync_model(lanes, vec![]);
    }
}

fn toast(ui: &QdmWindow, state: &RefCell<ViewState>, message: impl Into<SharedString>, error: bool) {
    ui.set_toast(message.into()); ui.set_toast_error(error);
    state.borrow_mut().toast_until = if error { None } else { Some(Instant::now() + Duration::from_secs(7)) };
}
fn send(tx: &Sender<Command>, command: Command, ui: &QdmWindow, state: &RefCell<ViewState>) -> bool {
    if tx.send(command).is_err() { toast(ui, state, "The download engine is unavailable. Please restart QDM.", true); false } else { true }
}
fn valid_url(value: &str) -> Result<String, String> {
    let raw = value.trim();
    let url = url::Url::parse(raw).map_err(|_| "Enter a complete HTTP or HTTPS download URL.")?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() { return Err("Only HTTP and HTTPS download URLs are supported.".into()); }
    Ok(raw.into())
}
fn parse_speed_limit(value: &str) -> Result<u64, String> {
    let limit = value.trim().parse::<u64>().map_err(|_| "Speed limit must be a non-negative whole number.")?;
    if limit > MAX_SPEED_LIMIT_KIB {
        return Err(format!("Speed limit must not exceed {MAX_SPEED_LIMIT_KIB} KiB/s."));
    }
    Ok(limit)
}
fn parse_queue_speed_limits(value: &str) -> Result<BTreeMap<String, u64>, String> {
    let mut limits = BTreeMap::new();
    for (index, line) in value.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let Some((raw_name, raw_limit)) = line.rsplit_once('=') else {
            return Err(format!("Queue limit line {} must use name=KiB/s.", index + 1));
        };
        let name = normalize_queue_name(raw_name)?;
        if name.is_empty() { return Err(format!("Queue limit line {} needs a queue name.", index + 1)); }
        let limit = parse_speed_limit(raw_limit)?;
        if limits.insert(name, limit).is_some() {
            return Err("Queue speed-limit names must be unique after trimming.".into());
        }
        if limits.len() > MAX_QUEUE_COUNT {
            return Err(format!("Configure no more than {MAX_QUEUE_COUNT} queue speed limits."));
        }
    }
    Ok(limits)
}
fn parse_queue_configs(value: &str) -> Result<BTreeMap<String, QueueConfig>, String> {
    let mut queues = BTreeMap::new();
    for (index, line) in value.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() { continue; }
        let mut segments = line.split('|');
        let raw_name = segments.next().unwrap_or("");
        let name = normalize_queue_name(raw_name).map_err(|error| format!("Queue line {}: {error}", index + 1))?;
        if name.is_empty() { return Err(format!("Queue line {} needs a name.", index + 1)); }
        let priority = match segments.next() {
            Some(value) => value.trim().parse::<i32>().map_err(|_| format!("Queue line {} priority must be a whole number.", index + 1))?,
            None => 0,
        };
        let category_dir = segments.next().unwrap_or("").trim().to_owned();
        if queues.insert(name, QueueConfig { priority, category_dir, paused: false }).is_some() {
            return Err(format!("Queue names must be unique after trimming (line {}).", index + 1));
        }
        if queues.len() > MAX_QUEUE_COUNT {
            return Err(format!("Configure no more than {MAX_QUEUE_COUNT} queues."));
        }
    }
    Ok(queues)
}
fn settings_from(ui: &QdmWindow, base: &Settings) -> Result<Settings, String> {
    let mut settings = base.clone();
    settings.connections = ui.get_setting_connections().trim().parse::<u8>().ok().filter(|n| (1..=64).contains(n)).ok_or("Connections must be a whole number from 1 to 64.")?;
    settings.max_concurrent = ui.get_setting_concurrent().trim().parse::<u8>().ok().filter(|n| (1..=8).contains(n)).ok_or("Concurrent downloads must be a whole number from 1 to 8.")?;
    settings.speed_limit_kib = ui.get_setting_speed().trim().parse::<u64>().map_err(|_| "Speed limit must be a non-negative whole number.")?;
    settings.retries = ui.get_setting_retries().trim().parse::<u8>().ok().filter(|n| *n <= 8).ok_or("Retry attempts must be from 0 to 8.")?;
    settings.download_dir = ui.get_setting_directory().trim().into();
    settings.queue_speed_limits = parse_queue_speed_limits(&ui.get_setting_queue_limits().to_string())?;
    let mut parsed_queues = parse_queue_configs(&ui.get_setting_queue_config().to_string())?;
    for (name, config) in parsed_queues.iter_mut() {
        config.paused = base.queues.get(name).map(|existing| existing.paused).unwrap_or(false);
    }
    // Keep a queue paused across a Preferences save even if its line was edited away,
    // so pausing is a deliberate, separate action rather than something text edits erase.
    for (name, existing) in base.queues.iter() {
        if existing.paused && !parsed_queues.contains_key(name) {
            parsed_queues.insert(name.clone(), existing.clone());
        }
    }
    settings.queues = parsed_queues;
    if !Path::new(&settings.download_dir).is_absolute() { return Err("The download folder must be an absolute Windows path.".into()); }
    settings.close_to_tray = ui.get_setting_tray();
    settings.clipboard_suggestions = ui.get_setting_clipboard();
    Ok(settings)
}

fn hwnd(ui: &QdmWindow) -> Option<HWND> {
    let handle = ui.window().window_handle();
    let raw = handle.window_handle().ok()?.as_raw();
    match raw { RawWindowHandle::Win32(h) => Some(h.hwnd.get() as HWND), _ => None }
}
fn show(ui: &QdmWindow) {
    let _ = ui.show();
    if let Some(handle) = hwnd(ui) { unsafe { ShowWindow(handle, SW_RESTORE); SetForegroundWindow(handle); } }
}
fn native_setup(ui: &QdmWindow) -> bool {
    let Some(handle) = hwnd(ui) else { return false; };
    crate::platform::tag_window(handle);
    let enabled: i32 = 1;
    unsafe { DwmSetWindowAttribute(handle, 20, (&enabled as *const i32).cast(), 4); }
    // Slint's preferred portrait size can extend below the taskbar when Windows
    // chooses an offset initial position. Fit once, on the existing UI/DPI
    // thread, so later user moves, resizes and tray restores remain untouched.
    static INITIAL_GEOMETRY: std::sync::Once = std::sync::Once::new();
    INITIAL_GEOMETRY.call_once(|| unsafe {
        let mut bounds: RECT = std::mem::zeroed();
        if GetWindowRect(handle, &mut bounds) == 0 { return; }
        let monitor = MonitorFromWindow(handle, MONITOR_DEFAULTTONEAREST);
        if monitor.is_null() { return; }
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) == 0 { return; }
        let original_width = bounds.right - bounds.left;
        let original_height = bounds.bottom - bounds.top;
        let work_width = info.rcWork.right - info.rcWork.left;
        let work_height = info.rcWork.bottom - info.rcWork.top;
        if original_width <= 0 || original_height <= 0 || work_width <= 0 || work_height <= 0 { return; }
        let width = original_width.min(work_width);
        let height = original_height.min(work_height);
        let left = bounds.left.clamp(info.rcWork.left, info.rcWork.right - width);
        let top = bounds.top.clamp(info.rcWork.top, info.rcWork.bottom - height);
        if (left, top, width, height) != (bounds.left, bounds.top, original_width, original_height) {
            SetWindowPos(handle, std::ptr::null_mut(), left, top, width, height,
                SWP_NOACTIVATE | SWP_NOZORDER | SWP_NOOWNERZORDER);
        }
    });
    true
}
fn copy_text(text: &str) -> Result<(), String> {
    let wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
    unsafe {
        if OpenClipboard(std::ptr::null_mut()) == 0 { return Err("The clipboard is busy. Please try again.".into()); }
        let result = (|| {
            let memory = GlobalAlloc(GMEM_MOVEABLE, wide.len() * 2);
            if memory.is_null() { return Err("Could not allocate clipboard memory.".into()); }
            let pointer = GlobalLock(memory) as *mut u16;
            if pointer.is_null() { GlobalFree(memory); return Err("Could not access clipboard memory.".into()); }
            std::ptr::copy_nonoverlapping(wide.as_ptr(), pointer, wide.len());
            GlobalUnlock(memory);
            EmptyClipboard();
            if SetClipboardData(13, memory).is_null() { GlobalFree(memory); return Err("Could not copy to the clipboard.".into()); }
            Ok(())
        })();
        CloseClipboard(); result
    }
}

// Clipboard text format identifier (CF_UNICODETEXT) for clipboard reads below.
const CF_UNICODETEXT: u32 = 13;
/// Read the current clipboard as Unicode text, if it is available as plain text.
/// Returns None when the clipboard holds no text format or is momentarily locked.
fn clipboard_text() -> Option<String> {
    unsafe {
        if IsClipboardFormatAvailable(CF_UNICODETEXT) == 0 { return None; }
        if OpenClipboard(std::ptr::null_mut()) == 0 { return None; }
        let result = (|| {
            let handle = GetClipboardData(CF_UNICODETEXT);
            if handle.is_null() { return None; }
            let pointer = GlobalLock(handle) as *const u16;
            if pointer.is_null() { return None; }
            let mut length = 0usize;
            while *pointer.add(length) != 0 { length += 1; }
            let text = String::from_utf16_lossy(std::slice::from_raw_parts(pointer, length));
            GlobalUnlock(handle);
            Some(text)
        })();
        CloseClipboard();
        result
    }
}

pub fn run(command_tx: Sender<Command>, event_rx: Receiver<UiEvent>, data: &Path) -> Result<(), String> {
    // The measured portrait UI sustains the display cadence with much smaller
    // private memory in the software renderer. Keep an explicit backend override
    // available for different displays/drivers and diagnostic comparisons.
    if std::env::var_os("SLINT_BACKEND").is_none() {
        slint::BackendSelector::new()
            .backend_name("winit".into())
            .renderer_name("software".into())
            .select()
            .map_err(|error| format!("Could not initialize the QDM renderer: {error}"))?;
    }
    let ui = QdmWindow::new().map_err(|e| e.to_string())?;
    ui.invoke_use_dark_palette();
    let tray = QdmTray::new().map_err(|e| e.to_string())?;
    let preferences = data.join("ui-settings.json");
    let appearance: Appearance = std::fs::read(&preferences).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default();
    ui.set_reduced_motion(appearance.reduced_motion);
    ui.set_version_label(format!("v{}", env!("CARGO_PKG_VERSION")).into());
    let jobs = Rc::new(VecModel::<DownloadItem>::default());
    let lanes = Rc::new(VecModel::<LaneItem>::default());
    ui.set_downloads(ModelRc::from(jobs.clone())); ui.set_lanes(ModelRc::from(lanes.clone()));
    let state = Rc::new(RefCell::new(ViewState {
        snapshot: Snapshot::default(), speed_history: HashMap::new(), last_speed_sample: None, selected: String::new(), checked: HashSet::new(), filter: 0, known_ids: None,
        toast_until: None, preferences, clipboard: ClipboardWatcher::new(), clipboard_seq: 0,
        order_key: 0, row_sig: Vec::new(), row_avail: Vec::new(),
    }));
    let mailbox = Arc::new(Mutex::new(Mailbox::default()));
    let sink = mailbox.clone();
    std::thread::Builder::new().name("qdm-ui-mailbox".into()).spawn(move || {
        while let Ok(event) = event_rx.recv() {
            let mut queue = sink.lock().unwrap_or_else(|e| e.into_inner());
            match event {
                UiEvent::State(snapshot) => queue.latest = Some(snapshot),
                event => { if queue.events.len() == 32 { queue.events.remove(0); } queue.events.push(event); }
            }
        }
        sink.lock().unwrap_or_else(|e| e.into_inner()).disconnected = true;
    }).map_err(|e| e.to_string())?;

    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_add_url(move || if let Some(ui) = weak.upgrade() {
        match valid_url(&ui.get_url()) {
            Ok(url) => { if send(&tx, Command::Add(AddRequest { url, ..Default::default() }), &ui, &state) {
                ui.set_url("".into());
                toast(&ui, &state, "Download added to the queue.", false);
            } }
            Err(error) => toast(&ui, &state, error, true),
        }
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
     ui.on_add_batch(move || if let Some(ui) = weak.upgrade() {
        let urls: Result<Vec<_>, _> = ui.get_batch_urls().lines().filter(|l| !l.trim().is_empty()).map(valid_url).collect();
        match urls {
            Ok(urls) if !urls.is_empty() => {
                let queue = match normalize_queue_name(&ui.get_batch_queue().to_string()) {
                    Ok(queue) => queue,
                    Err(error) => { toast(&ui, &state, error, true); return; }
                };
                let speed_limit_kib = match parse_speed_limit(&ui.get_batch_speed_limit().to_string()) {
                    Ok(limit) => limit,
                    Err(error) => { toast(&ui, &state, error, true); return; }
                };
                let hash = ui.get_expected_hash().trim().to_owned();
                if !hash.is_empty() && (urls.len() != 1 || hash.len() != 64 || !hash.bytes().all(|b| b.is_ascii_hexdigit())) {
                    toast(&ui, &state, "An expected SHA-256 needs exactly one URL and 64 hexadecimal characters.", true); return;
                }
                let count = urls.len();
                for url in urls { if !send(&tx, Command::Add(AddRequest { url, start: ui.get_start_now(), queue: queue.clone(), speed_limit_kib, expected_sha256: if hash.is_empty() { None } else { Some(hash.clone()) }, ..Default::default() }), &ui, &state) { return; } }
                ui.set_advanced_open(false); ui.set_batch_urls("".into()); ui.set_batch_queue("".into()); ui.set_batch_speed_limit("0".into()); ui.set_expected_hash("".into());
                toast(&ui, &state, format!("Added {count} download(s)."), false);
            }
            Ok(_) => toast(&ui, &state, "Paste at least one download URL.", true),
            Err(error) => toast(&ui, &state, error, true),
        }
      }); }
     { let weak = ui.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
       ui.on_select_download(move |id| if let Some(ui) = weak.upgrade() {
           let mut view = state.borrow_mut();
           view.selected = id.to_string();
           if let Some(task) = view.snapshot.downloads.iter().find(|task| task.id == id.as_str()) {
               ui.set_selected_queue(task.queue.clone().into());
               ui.set_selected_download_speed(task.speed_limit_kib.to_string().into());
           }
           render(&ui, &mut view, &jobs, &lanes);
       }); }
    { let weak = ui.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      ui.on_toggle_checked(move |id| if let Some(ui) = weak.upgrade() {
          let id = id.to_string(); let mut state = state.borrow_mut();
          if !state.checked.remove(&id) { state.checked.insert(id); }
          render(&ui, &mut state, &jobs, &lanes);
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      ui.on_clear_checked(move || if let Some(ui) = weak.upgrade() {
          let mut state = state.borrow_mut(); state.checked.clear(); render(&ui, &mut state, &jobs, &lanes);
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      ui.on_select_all_visible(move || if let Some(ui) = weak.upgrade() {
          let mut state = state.borrow_mut();
          let ids: Vec<_> = state.snapshot.downloads.iter().filter(|t| t.status == "completed" && state.filter != 1).map(|t| t.id.clone()).collect();
          state.checked.extend(ids); render(&ui, &mut state, &jobs, &lanes);
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      ui.on_context_download(move |id| if let Some(ui) = weak.upgrade() {
          let id = id.to_string();
          let task = state.borrow().snapshot.downloads.iter().find(|t| t.id == id).cloned();
          let Some(task) = task else { return; };
          let Some(owner) = hwnd(&ui) else { return; };
          unsafe {
              let menu = CreatePopupMenu(); if menu.is_null() { return; }
              let open_enabled = task.status == "completed" && task.file_path.as_ref().is_some_and(|p| Path::new(p).is_file());
              let label = crate::platform::wide("Open file"); AppendMenuW(menu, MF_STRING | if open_enabled { 0 } else { MF_GRAYED }, 1, label.as_ptr());
              let label = crate::platform::wide("Open file location"); AppendMenuW(menu, MF_STRING, 2, label.as_ptr());
              let label = crate::platform::wide("Remove"); AppendMenuW(menu, MF_STRING, 3, label.as_ptr());
              let mut point = windows_sys::Win32::Foundation::POINT { x: 0, y: 0 }; GetCursorPos(&mut point);
              let choice = TrackPopupMenu(menu, TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, 0, owner, std::ptr::null()); DestroyMenu(menu);
              match choice { 1 if open_enabled => { send(&tx, Command::OpenFile(id), &ui, &state); },
                  2 => { send(&tx, Command::RevealFile(id), &ui, &state); },
                  3 => { state.borrow_mut().selected = id.clone(); render(&ui, &mut state.borrow_mut(), &jobs, &lanes); ui.set_remove_target_id(id.into()); ui.set_remove_target_name(task.filename.clone().into()); ui.set_bulk_remove(false); ui.set_remove_open(true); }, _ => {} }
          }
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_open_download(move |id| if let Some(ui) = weak.upgrade() { send(&tx, Command::OpenFile(id.to_string()), &ui, &state); }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_remove_checked(move || if let Some(ui) = weak.upgrade() {
          let ids: Vec<_> = state.borrow().checked.iter().cloned().collect();
          for id in ids {
              let completed = state.borrow().snapshot.downloads.iter().any(|task| task.id == id && task.status == "completed");
              let command = if ui.get_delete_local_files() && completed { Command::RemoveWithFile(id.clone()) } else { Command::Remove(id.clone()) };
              if !send(&tx, command, &ui, &state) { break; } state.borrow_mut().checked.remove(&id);
          }
          ui.set_remove_open(false);
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      ui.on_filter_changed(move |filter| if let Some(ui) = weak.upgrade() {
          state.borrow_mut().filter = filter;
          render(&ui, &mut state.borrow_mut(), &jobs, &lanes);
      }); }
    macro_rules! selected_action {
        ($callback:ident, $variant:ident) => {{
            let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
            ui.$callback(move || if let Some(ui) = weak.upgrade() {
                let id = state.borrow().selected.clone();
                if !id.is_empty() { send(&tx, Command::$variant(id), &ui, &state); }
            });
        }};
    }
    selected_action!(on_pause_selected, Pause);
    selected_action!(on_resume_selected, Resume);
    // Row actions are bound to their own task ID; selection/focus may refer to a different file.
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_resume_download(move |id| if let Some(ui) = weak.upgrade() {
          let id = id.to_string();
          let allowed = state.borrow().snapshot.downloads.iter().any(|task|
              task.id == id && resumable_status(&task.status));
          if allowed { send(&tx, Command::Resume(id), &ui, &state); }
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_pause_download(move |id| if let Some(ui) = weak.upgrade() {
          let id = id.to_string();
          let allowed = state.borrow().snapshot.downloads.iter().any(|task|
              task.id == id && running(&task.status));
          if allowed { send(&tx, Command::Pause(id), &ui, &state); }
      }); }
    selected_action!(on_open_selected, OpenFile);
    selected_action!(on_reveal_selected, RevealFile);
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_remove_selected(move || if let Some(ui) = weak.upgrade() {
          let id = ui.get_remove_target_id().to_string();
          if !id.is_empty() {
              let completed = state.borrow().snapshot.downloads.iter().any(|task| task.id == id && task.status == "completed");
              let command = if ui.get_delete_local_files() && completed { Command::RemoveWithFile(id) } else { Command::Remove(id) };
              if send(&tx, command, &ui, &state) { ui.set_remove_open(false); }
          }
      }); }
     { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
       ui.on_refresh_selected(move || if let Some(ui) = weak.upgrade() {
          match valid_url(&ui.get_replacement_url()) {
              Ok(url) => { let id = state.borrow().selected.clone();
                  if send(&tx, Command::ReplaceUrl { id, url }, &ui, &state) { ui.set_refresh_open(false); ui.set_replacement_url("".into()); }
              }
              Err(error) => toast(&ui, &state, error, true),
          }
       }); }
     { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
       ui.on_update_download_limits(move || if let Some(ui) = weak.upgrade() {
           let id = state.borrow().selected.clone();
           if id.is_empty() { return; }
           let queue = match normalize_queue_name(&ui.get_selected_queue().to_string()) {
               Ok(queue) => queue,
               Err(error) => { toast(&ui, &state, error, true); return; }
           };
           let speed_limit_kib = match parse_speed_limit(&ui.get_selected_download_speed().to_string()) {
               Ok(limit) => limit,
               Err(error) => { toast(&ui, &state, error, true); return; }
           };
           if send(&tx, Command::UpdateDownloadLimits { id, queue, speed_limit_kib }, &ui, &state) {
               toast(&ui, &state, "Transfer limits updated.", false);
           }
       }); }
    macro_rules! queue_pause_action {
        ($callback:ident, $paused:expr, $message:expr) => {{
            let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
            ui.$callback(move || if let Some(ui) = weak.upgrade() {
                let id = state.borrow().selected.clone();
                let queue = state.borrow().snapshot.downloads.iter()
                    .find(|task| task.id == id).map(|task| task.queue.clone());
                match queue {
                    Some(name) if !name.is_empty() => {
                        if send(&tx, Command::SetQueuePaused { name, paused: $paused }, &ui, &state) { toast(&ui, &state, $message, false); }
                    }
                    _ => toast(&ui, &state, "The Default queue cannot be paused individually. Use Pause all instead.", true),
                }
            });
        }};
    }
    queue_pause_action!(on_pause_queue, true, "Queue paused.");
    queue_pause_action!(on_resume_queue, false, "Queue resumed.");
     macro_rules! action {
        ($callback:ident, $variant:ident) => {{
            let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
            ui.$callback(move || if let Some(ui) = weak.upgrade() { send(&tx, Command::$variant, &ui, &state); });
        }};
    }
    action!(on_pause_all, PauseAll); action!(on_resume_all, ResumeAll);
    action!(on_open_folder, OpenDownloadFolder); action!(on_open_integration, ShowIntegration);
    { let weak = ui.as_weak(); let state = state.clone();
      ui.on_open_settings(move || if let Some(ui) = weak.upgrade() {
          let settings = state.borrow().snapshot.settings.clone();
          ui.set_setting_connections(settings.connections.to_string().into());
          ui.set_setting_concurrent(settings.max_concurrent.to_string().into());
           ui.set_setting_speed(settings.speed_limit_kib.to_string().into());
           ui.set_setting_retries(settings.retries.to_string().into());
           ui.set_setting_queue_limits(settings.queue_speed_limits.iter().map(|(name, limit)| format!("{name}={limit}")).collect::<Vec<_>>().join("\n").into());
           ui.set_setting_queue_config(settings.queues.iter().map(|(name, queue)| format!("{}|{}|{}", name, queue.priority, queue.category_dir)).collect::<Vec<_>>().join("\n").into());
           ui.set_setting_directory(settings.download_dir.into());
          ui.set_setting_tray(settings.close_to_tray); ui.set_setting_clipboard(settings.clipboard_suggestions); ui.set_settings_open(true);
      }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_save_settings(move || if let Some(ui) = weak.upgrade() {
          let base = state.borrow().snapshot.settings.clone();
          match settings_from(&ui, &base) {
              Ok(settings) => {
                  let preferences = state.borrow().preferences.clone();
                  let bytes = serde_json::to_vec_pretty(&Appearance { reduced_motion: ui.get_reduced_motion() }).unwrap_or_default();
                  if let Err(e) = std::fs::write(&preferences, bytes) { toast(&ui, &state, format!("Could not save appearance settings: {e}"), true); return; }
                  if send(&tx, Command::UpdateSettings(settings), &ui, &state) { ui.set_settings_open(false); toast(&ui, &state, "Settings saved.", false); }
              }
              Err(error) => toast(&ui, &state, error, true),
          }
      }); }
    { let weak = ui.as_weak(); let state = state.clone();
      ui.on_copy_token(move || if let Some(ui) = weak.upgrade() {
          match copy_text(&ui.get_bridge_token()) { Ok(()) => toast(&ui, &state, "Pairing token copied.", false), Err(e) => toast(&ui, &state, e, true) }
      }); }
    { let weak = ui.as_weak(); let state = state.clone();
      ui.on_open_extension(move || if let Some(ui) = weak.upgrade() {
          if let Err(e) = crate::platform::open(&ui.get_extension_path()) { toast(&ui, &state, e, true); }
      }); }
    { let weak = ui.as_weak(); let state = state.clone();
      ui.on_dismiss_toast(move || if let Some(ui) = weak.upgrade() { ui.set_toast("".into()); state.borrow_mut().toast_until = None; }); }
    { let weak = ui.as_weak(); let state = state.clone(); let tx = command_tx.clone();
      ui.on_clipboard_download(move || if let Some(ui) = weak.upgrade() {
          let url = ui.get_clipboard_url().to_string();
          ui.set_clipboard_visible(false);
          match valid_url(&url) {
              Ok(url) => { if send(&tx, Command::Add(AddRequest { url, ..Default::default() }), &ui, &state) { toast(&ui, &state, "Download added from the clipboard.", false); } }
              Err(error) => toast(&ui, &state, error, true),
          }
      }); }
    { let weak = ui.as_weak(); let state = state.clone();
      ui.on_clipboard_dismiss(move || if let Some(ui) = weak.upgrade() {
          let url = ui.get_clipboard_url().to_string();
          state.borrow_mut().clipboard.dismiss(&url);
          ui.set_clipboard_visible(false);
      }); }
    ui.on_quit(|| { let _ = slint::quit_event_loop(); });
    { let weak = ui.as_weak(); tray.on_restore(move || if let Some(ui) = weak.upgrade() { show(&ui); }); }
    { let tx = command_tx.clone(); tray.on_pause_all(move || { let _ = tx.send(Command::PauseAll); }); }
    { let tx = command_tx.clone(); tray.on_resume_all(move || { let _ = tx.send(Command::ResumeAll); }); }
    tray.on_quit(|| { let _ = slint::quit_event_loop(); });
    { let weak = ui.as_weak(); let state = state.clone();
      ui.window().on_close_requested(move || {
          if state.borrow().snapshot.settings.close_to_tray {
              // Keep the winit window alive and registered for the existing
              // profile-scoped second-instance restore mechanism.
              if let Some(ui) = weak.upgrade() { if let Some(handle) = hwnd(&ui) { unsafe { ShowWindow(handle, SW_HIDE); } } }
              slint::CloseRequestResponse::KeepWindowShown
          } else { let _ = slint::quit_event_loop(); slint::CloseRequestResponse::HideWindow }
      }); }

    let timer = Timer::default();
    { let weak = ui.as_weak(); let tray_weak = tray.as_weak(); let state = state.clone(); let jobs = jobs.clone(); let lanes = lanes.clone();
      let tagged = Cell::new(false);
      timer.start(TimerMode::Repeated, Duration::from_millis(200), move || {
          let Some(ui) = weak.upgrade() else { return; };
          if !tagged.get() { tagged.set(native_setup(&ui)); }
          let (latest, events, disconnected) = {
              let mut queue = mailbox.lock().unwrap_or_else(|e| e.into_inner());
              (queue.latest.take(), std::mem::take(&mut queue.events), queue.disconnected)
          };
           if let Some(snapshot) = latest {
               let mut view = state.borrow_mut();
               let newest_added = newest_unseen_id(view.known_ids.as_ref(), &snapshot.downloads);
               view.known_ids = Some(snapshot.downloads.iter().map(|task| task.id.clone()).collect());
               view.snapshot = snapshot;
               if let Some(ref id) = newest_added {
                   // A new task from QDM, browser capture, or plugin becomes the selected
                   // first row. Switch back to All if a filter would hide the new item.
                   view.selected = id.clone();
                   view.filter = 0;
                   ui.set_filter_index(0);
               }
               render(&ui, &mut view, &jobs, &lanes);
               drop(view);
               if newest_added.is_some() {
                   // ListView preserves scrolling during model diffs; reset only on arrival.
                   ui.invoke_scroll_downloads_to_top();
               }
               if let Some(tray) = tray_weak.upgrade() { tray.set_status(format!("QDM · {} active · {}", ui.get_active_label(), ui.get_aggregate_speed()).into()); }
           }
          for event in events {
              match event {
                  UiEvent::Error(error) => toast(&ui, &state, error, true),
                  UiEvent::Notice(message) => toast(&ui, &state, message, false),
                  UiEvent::Integration { port, token, extension_path } => {
                      ui.set_bridge_port(port.to_string().into()); ui.set_bridge_token(token.into());
                      ui.set_extension_path(extension_path.into()); ui.set_integration_open(true);
                  }
                  UiEvent::State(_) => unreachable!(),
              }
          }
          let expired = state.borrow().toast_until.is_some_and(|deadline| Instant::now() >= deadline);
          if expired { ui.set_toast("".into()); state.borrow_mut().toast_until = None; }
          // Opt-in clipboard link detection. The OS change counter is polled cheaply
          // every tick; clipboard text is only read when it actually changed, and only
          // while the user has the feature enabled.
          let clipboard_enabled = state.borrow().snapshot.settings.clipboard_suggestions;
          if !clipboard_enabled {
              if ui.get_clipboard_visible() { ui.set_clipboard_visible(false); }
          } else {
              let sequence = unsafe { GetClipboardSequenceNumber() } as u64;
              let changed = { state.borrow().clipboard_seq != sequence };
              if changed {
                  let text = clipboard_text();
                  let mut view = state.borrow_mut();
                  view.clipboard_seq = sequence;
                  if let Some(url) = view.clipboard.on_clipboard_change(text.as_deref()) {
                      ui.set_clipboard_name(display_name(&url).into());
                      ui.set_clipboard_url(url.into());
                      ui.set_clipboard_visible(true);
                  }
              }
          }
          if disconnected { let _ = slint::quit_event_loop(); }
      }); }
    tray.show().map_err(|e| e.to_string())?;
    ui.show().map_err(|e| e.to_string())?;
    native_setup(&ui);
    let result = slint::run_event_loop_until_quit().map_err(|e| e.to_string());
    timer.stop();
    drop(tray);
    let _ = ui.hide();
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queue_configs_parse_priority_and_category_and_reject_bad_input() {
        let map = parse_queue_configs("media|5|D:\\media\narchive|-2\n").unwrap();
        assert_eq!(map.get("media").map(|q| (q.priority, q.category_dir.as_str())), Some((5, "D:\\media")));
        assert_eq!(map.get("archive").map(|q| (q.priority, q.category_dir.clone())), Some((-2, String::new())));
        assert!(map.get("media").unwrap().paused == false);
        assert!(parse_queue_configs("   ").unwrap().is_empty());
        assert!(parse_queue_configs("media|notanumber").is_err());
        assert!(parse_queue_configs("bad=name|5").is_err());
        assert!(parse_queue_configs("dup|1\ndup|2").is_err());
    }

    #[test]
    fn queue_speed_limits_parse_names_and_reject_duplicates() {
        let limits = parse_queue_speed_limits(" media =256\narchive=0\n").unwrap();
        assert_eq!(limits.get("media"), Some(&256));
        assert_eq!(limits.get("archive"), Some(&0));
        assert!(parse_queue_speed_limits("media=256\n media =512").is_err());
        assert!(parse_queue_speed_limits("missing separator").is_err());
        assert!(parse_queue_speed_limits("media=18446744073709551615").is_err());
    }

    #[test]
    fn render_signature_tracks_every_visible_input_and_ignores_identity_only_changes() {
        let base = TaskView { id: "a".into(), filename: "file.bin".into(), url: "https://h/x".into(),
            status: "downloading".into(), downloaded_bytes: 100, total_bytes: Some(1000), speed_bps: 10, ..TaskView::default() };
        let s = render_signature(&base, false, false);
        assert_eq!(s, render_signature(&base, false, false), "identical inputs are stable");
        let mut prog = base.clone(); prog.downloaded_bytes = 200;
        assert_ne!(s, render_signature(&prog, false, false), "progress change must invalidate");
        let mut done = base.clone(); done.status = "completed".into();
        assert_ne!(s, render_signature(&done, false, false), "status change must invalidate");
        assert_ne!(s, render_signature(&base, true, false), "checked must invalidate");
        assert_ne!(s, render_signature(&base, false, true), "file availability must invalidate");
    }

    #[test]
    fn build_item_maps_running_completed_and_verifying_states() {
        let running = TaskView { id: "a".into(), filename: "app.zip".into(), url: "https://cdn.host/dir/app.zip".into(),
            status: "downloading".into(), downloaded_bytes: 512, total_bytes: Some(1024), ..TaskView::default() };
        let item = build_item(&running, false, false);
        assert_eq!(item.name, "app.zip");
        assert_eq!(item.status, "DOWNLOADING");
        assert_eq!(item.progress, 0.5);
        assert!(item.active && item.can_pause && !item.can_resume && !item.completed && !item.failed);
        assert!(!item.stage_visible);
        let completed = TaskView { status: "completed".into(), ..running.clone() };
        let done = build_item(&completed, true, true);
        assert_eq!(done.status, "Complete");
        assert!(done.completed && !done.active && done.checked && done.file_available);
        let verifying = TaskView { status: "verifying".into(), ..running.clone() };
        let stage = build_item(&verifying, false, false);
        assert!(stage.stage_visible && stage.stage_indeterminate);
        assert_eq!(stage.stage_text, "VERIFYING");
    }

    #[test]
    fn speed_graph_is_per_download_not_an_aggregate_snapshot() {
        let first = VecDeque::from(vec![0, 5_000, 12_000]);
        let second = VecDeque::from(vec![0, 100_000, 0]);
        let mut history = HashMap::new();
        history.insert("first".to_string(), first);
        history.insert("second".to_string(), second);
        let first_path = rate_sparkline(history.get("first"));
        let second_path = rate_sparkline(history.get("second"));
        assert_ne!(first_path, second_path);
        assert_eq!(rate_sparkline(history.get("missing")), "");
        assert_eq!(rate_sparkline(Some(&VecDeque::new())), "");
    }
    #[test]
    fn paused_and_recoverable_tasks_have_per_file_resume_in_active_view() {
        for status in ["paused", "error", "failed", "scheduled"] {
            assert!(visible_in_filter(status, 1), "{status} hidden from Active");
            assert!(resumable_status(status), "{status} must show Resume");
            assert!(!running(status), "{status} must not show Pause");
        }
        for status in ["queued", "downloading", "preparing", "probing", "assembling", "verifying"] {
            assert!(visible_in_filter(status, 1), "{status} hidden from Active");
            assert!(running(status), "{status} must show Pause");
            assert!(!resumable_status(status), "{status} must not show Resume");
        }
        assert!(!visible_in_filter("completed", 1));
        assert!(visible_in_filter("completed", 2));
        assert!(!visible_in_filter("paused", 2));
    }

    #[test]
    fn incoming_task_focuses_latest_id_even_if_engine_reorders_items() {
        let first = TaskView { id: "a".into(), created_at: 1000, ..TaskView::default() };
        let second = TaskView { id: "b".into(), created_at: 2000, ..TaskView::default() };
        let later = TaskView { id: "c".into(), created_at: 3000, ..TaskView::default() };
        let seen = HashSet::from(["a".to_owned(), "b".to_owned()]);
        assert_eq!(newest_unseen_id(Some(&seen), &[later.clone(), first, second]), Some("c".into()));
        assert_eq!(newest_unseen_id(Some(&seen), &[later]), Some("c".into()));
    }

    #[test]
    fn incoming_focus_preserves_selection_on_first_snapshot_or_progress_only() {
        let old = TaskView { id: "old".into(), created_at: 1000, ..TaskView::default() };
        assert_eq!(newest_unseen_id(None, std::slice::from_ref(&old)), None);
        let seen = HashSet::from(["old".to_owned()]);
        assert_eq!(newest_unseen_id(Some(&seen), std::slice::from_ref(&old)), None);
    }

    #[test]
    fn incoming_focus_chooses_latest_of_batch_and_breaks_timestamp_ties() {
        let seen = HashSet::from(["old".to_owned()]);
        let task = |id: &str, created_at| TaskView {
            id: id.into(), created_at, ..TaskView::default()
        };
        let tasks = vec![task("new1", 9000), task("old", 1000), task("new2", 9000)];
        assert_eq!(newest_unseen_id(Some(&seen), &tasks), Some("new2".into()));
    }

    #[test]
    fn newest_first_uses_created_at_not_engine_snapshot_order() {
        let task = |id: &str, created_at| TaskView { id: id.to_owned(), created_at, ..TaskView::default() };
        let data = vec![task("newest", 3000), task("oldest", 1000), task("middle", 2000)];
        let ids: Vec<&str> = newest_first(&data).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["newest", "middle", "oldest"]);
    }

    #[test]
    fn newest_first_stably_prioritizes_later_inserted_on_exact_timestamp_ties() {
        let first = TaskView { id: "first".into(), created_at: 1000, ..TaskView::default() };
        let second = TaskView { id: "second".into(), created_at: 1000, ..TaskView::default() };
        let data = [first, second];
        let ids: Vec<&str> = newest_first(&data).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["second", "first"]);
    }

    #[test]
    fn completed_legacy_assembly_notice_is_hidden_without_mutating_history() {
        let task = TaskView {
            status: "completed".into(),
            notice: Some("Combining verified partial files into one output...".into()),
            downloaded_bytes: 5_000_000_000,
            part_count: 276,
            ..TaskView::default()
        };
        let before = serde_json::to_value(&task).unwrap();
        assert_eq!(selected_notice(&task), "");
        assert_eq!(serde_json::to_value(&task).unwrap(), before);
    }

    #[test]
    fn assembly_notice_remains_visible_until_completed() {
        for status in ["assembling", "verifying", "paused", "downloading", "error"] {
            let task = TaskView {
                status: status.into(),
                notice: Some("Combining verified partial files into one output...".into()),
                ..TaskView::default()
            };
            assert_eq!(selected_notice(&task), task.notice.as_deref().unwrap(), "{status}");
        }
    }

    #[test]
    fn completed_errors_and_other_notices_remain_visible() {
        let mut task = TaskView { status: "completed".into(), ..TaskView::default() };
        for notice in [
            "Could not save download progress: Access is denied (os error 5)",
            "The server has no strong validator.",
            "Recovered a verified file committed before QDM last stopped.",
            "Combining verified partial files into one output... Warning: check the output.",
        ] {
            task.notice = Some(notice.into());
            assert_eq!(selected_notice(&task), notice);
        }
        task.notice = Some("Combining verified partial files into one output...".into());
        task.error = Some("Could not open the completed file.".into());
        assert_eq!(selected_notice(&task), "Could not open the completed file.");
        task.error = task.notice.clone();
        assert_eq!(selected_notice(&task), task.error.as_deref().unwrap());
    }
}
