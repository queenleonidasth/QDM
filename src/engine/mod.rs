//! QDM native engine: independent checked byte ownership in a single positioned-write file.
//! Request overlap is checked before appending and again across piece boundaries.
pub(crate) mod protocol;
mod transfer;
mod store;

use crate::api::{normalize_queue_name, AddRequest, LaneView, SegmentView, Settings, Snapshot, TaskView, MAX_QUEUE_COUNT, MAX_SPEED_LIMIT_KIB};
use protocol::{filename_from_url, request_headers, safe_filename, validate_url, Validator};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc, Mutex, MutexGuard,
    },
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};
use tokio::{
    fs,
    sync::{broadcast, watch, Notify},
};
use uuid::Uuid;

const HISTORY_LIMIT: usize = 10_000;
const PART_LIMIT: usize = 4096;

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .min(u64::MAX as u128) as u64
}
fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct Part {
    id: u64,
    start: u64,
    end: u64,
    #[serde(default)]
    owner_lane: Option<u8>,
    /// Bytes accepted by the worker, including its bounded userspace buffer.
    /// Only `durable` bytes may be recovered after an error or process death.
    downloaded: u64,
    #[serde(default)]
    durable: u64,
    #[serde(default)]
    durable_tail_sha256: Option<String>,
    #[serde(skip)]
    reserved: u64,
    #[serde(skip)]
    active: bool,
    // Retry budget survives reassignment in one run, but explicit Resume or a
    // process restart gives the repaired connection a fresh attempt budget.
    #[serde(skip)]
    transport_failures: u8,
    #[serde(skip)]
    last_transport_failure_durable: u64,
    #[serde(skip)]
    split_parent: Option<u64>,
    // Exclusive end of this worker's validated, still-live HTTP response.
    #[serde(skip)]
    response_end: u64,
    #[serde(skip)]
    request_started_at: Option<Instant>,
    #[serde(skip)]
    response_started_at: Option<Instant>,
    #[serde(skip)]
    response_payload_bytes: u64,
    #[serde(default)]
    overlap_start: u64,
    #[serde(default)]
    overlap_len: u64,
    #[serde(default)]
    overlap_sha256: Option<String>,
}

impl Part {
    fn record_transport_failure(&mut self, retries: u8) -> bool {
        if self.durable > self.last_transport_failure_durable {
            self.transport_failures = 0;
        }
        self.last_transport_failure_durable = self.durable;
        self.transport_failures = self.transport_failures.saturating_add(1);
        self.transport_failures <= retries
    }
    fn len(&self) -> u64 {
        self.end.saturating_sub(self.start)
    }
    fn remaining(&self) -> u64 {
        self.len().saturating_sub(self.downloaded + self.reserved)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
struct LaneRecord {
    /// Cumulative bytes accepted by this worker slot, including its write buffer.
    transferred_bytes: u64,
    current_bytes: u64,
    current_total: u64,
    current_part_id: Option<u64>,
    active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Task {
    view: TaskView,
    #[serde(default)]
    parts: Vec<Part>,
    #[serde(default)]
    lane_activity: Vec<LaneRecord>,
    #[serde(default)]
    validator: Validator,
    #[serde(default)]
    resource_url: Option<String>,
    #[serde(default)]
    custom_filename: bool,
    #[serde(default)]
    stream_mode: bool,
    // Browser credentials are session-scoped, never written to plaintext history.
    #[serde(skip)]
    headers: BTreeMap<String, String>,
    #[serde(default)]
    next_part_id: u64,
    #[serde(default)]
    pending_output: Option<PendingOutput>,
    #[serde(skip)]
    sampled_network: u64,
    #[serde(skip)]
    sampled_at: Option<Instant>,
    #[serde(skip)]
    adaptive_limit: u8,
    #[serde(skip)]
    adaptive_last_change: Option<Instant>,
    #[serde(skip)]
    adaptive_healthy_bytes: u64,
    #[serde(skip)]
    adaptive_starting: bool,
    #[serde(skip)]
    startup_ready_slots: u64,
    #[serde(skip)]
    startup_next_growth: Option<Instant>,
    #[serde(skip)]
    request_not_before: Option<Instant>,
    #[serde(skip)]
    backoff_reduced: bool,
    #[serde(skip)]
    rate_limit_rounds: u8,
    #[serde(skip)]
    transport_failure_streak: u8,
    // Measured request-to-first-body cost for this run, not a saved setting.
    #[serde(skip)]
    request_setup_micros: u64,
    // When a server advertises ranges but ignores bounded requests, keep
    // the fallback GET in single-stream mode for the remainder of this run.
    #[serde(skip)]
    force_single_stream: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PendingOutput {
    path: String,
    temporary_path: String,
    filename: String,
    size: u64,
    sha256: String,
}

impl Task {
    fn ensure_lane_activity(&mut self) {
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
        if self.view.status != "completed" {
            self.view.downloaded_bytes = self.parts.iter().map(|p| p.downloaded).sum();
        }
        self.view.part_count = self.parts.len() as u32;
        self.view.connections_active = self.parts.iter().filter(|p| p.active).count().min(64) as u8;
        self.view.peak_connections = self.view.peak_connections.max(self.view.connections_active);
        self.view.segments = self
            .parts
            .iter()
            .map(|p| SegmentView {
                id: p.id,
                start: p.start,
                end: p.end,
                downloaded_bytes: p.downloaded,
                active: p.active,
            })
            .collect();
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

#[derive(Serialize, Deserialize)]
struct Saved {
    schema_version: u32,
    settings: Settings,
    tasks: Vec<Task>,
}

// A short-lived cap learned from actual HTTP 429 responses. Normal hosts
// keep their per-file budget; never halve parallelism without evidence.
struct HostCap { connections: usize, learned_at: Instant }

struct State {
    settings: Settings,
    tasks: Vec<Task>,
    active: HashMap<String, RunHandle>,
    host_caps: HashMap<String, HostCap>,
    revision: u64,
    closing: bool,
    warning: Option<String>,
}

// Only coordinate same-origin concurrency after actual rate-limit evidence.
// Unrestricted hosts retain the configured per-file parallelism.
fn origin_key(raw: &str) -> Option<String> {
    let url = url::Url::parse(raw).ok()?;
    Some(format!("{}://{}:{}", url.scheme(), url.host_str()?, url.port_or_known_default()?))
}
fn origin_worker_budget(state: &State, id: &str) -> usize {
    let configured = state.settings.connections.max(1) as usize;
    let Some(task) = state.tasks.iter().find(|t| t.view.id == id) else { return configured };
    let Some(origin) = origin_key(task.resource_url.as_deref().unwrap_or(&task.view.url)) else { return configured };
    let Some(cap) = state.host_caps.get(&origin) else { return configured };
    let age = cap.learned_at.elapsed();
    if age >= Duration::from_secs(60) { return configured; }
    // Probe cautiously after an observed 429. Without periodic upward probes,
    // a CDN that raises its limit strands a resumed transfer at the old cap.
    let probe_slots = if age >= Duration::from_secs(8) {
        1 + (age.as_secs() - 8) as usize / 8
    } else { 0 };
    let budget = cap.connections.saturating_add(probe_slots).min(configured);
    let active: Vec<_> = state.tasks.iter().filter(|other| {
        state.active.contains_key(&other.view.id)
            && other.view.status == "downloading"
            && origin_key(other.resource_url.as_deref().unwrap_or(&other.view.url)).as_deref() == Some(origin.as_str())
    }).map(|t| t.view.id.as_str()).collect();
    let peers = active.len().max(1);
    let rank = active.iter().position(|candidate| *candidate == id).unwrap_or(0);
    (budget / peers + usize::from(rank < budget % peers)).max(1).min(configured)
}

#[derive(Clone)]
struct RunHandle {
    stop: watch::Sender<bool>,
    done: watch::Receiver<bool>,
}

#[derive(Default)]
struct RateBucket {
    tokens: f64,
    updated_at: Option<Instant>,
}

impl RateBucket {
    fn wait_duration(&mut self, now: Instant, limit_kib: u64, bytes: u64) -> Duration {
        if limit_kib == 0 || bytes == 0 { return Duration::ZERO; }
        let elapsed = self.updated_at.map(|at| now.saturating_duration_since(at).as_secs_f64()).unwrap_or(0.0);
        let target = bytes as f64;
        let rate = limit_kib as f64 * 1024.0;
        self.tokens = (self.tokens + elapsed * rate).min(target);
        self.updated_at = Some(now);
        if self.tokens >= target { Duration::ZERO }
        else { Duration::from_secs_f64((target - self.tokens) / rate) }
    }

    fn consume(&mut self, bytes: u64) {
        self.tokens = (self.tokens - bytes as f64).max(0.0);
    }
}

#[derive(Default)]
struct Throttle {
    global: RateBucket,
    per_task: HashMap<String, RateBucket>,
    per_queue: HashMap<String, RateBucket>,
}

struct Inner {
    data_dir: PathBuf,
    state: Mutex<State>,
    persist_lock: tokio::sync::Mutex<()>,
    events: broadcast::Sender<Snapshot>,
    client: Client,
    stream_client: Client,
    work_changed: Notify,
    monitor_changed: Arc<Notify>,
    throttle_changed: Notify,
    throttle: Mutex<Throttle>,
    dirty: AtomicBool,
    change_counter: AtomicU64,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

fn client(raw: bool) -> Result<Client, String> {
    let builder = Client::builder()
        .http1_only()
        .connect_timeout(Duration::from_secs(20))
        .pool_idle_timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(64)
        .tcp_keepalive(Duration::from_secs(30))
        .user_agent(concat!("QDM/", env!("CARGO_PKG_VERSION")))
        .referer(false)
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.previous().len() >= 10 {
                return attempt.error("Too many redirects");
            }
            if !matches!(attempt.url().scheme(), "http" | "https")
                || !attempt.url().username().is_empty()
                || attempt.url().password().is_some()
            {
                return attempt.error("Unsafe redirect URL");
            }
            if attempt
                .previous()
                .last()
                .map(|u| u.scheme() == "https")
                .unwrap_or(false)
                && attempt.url().scheme() == "http"
            {
                return attempt.error("HTTPS redirect attempted an unencrypted connection");
            }
            attempt.follow()
        }));
    let builder = if raw {
        builder.no_gzip().no_brotli().no_deflate().no_zstd()
    } else {
        builder
    };
    builder.build().map_err(|e| e.to_string())
}

fn validated_settings(mut settings: Settings, fallback: &Path) -> Result<Settings, String> {
    settings.connections = settings.connections.clamp(1, 64);
    settings.max_concurrent = settings.max_concurrent.clamp(1, 8);
    settings.retries = settings.retries.min(8);
    settings.speed_limit_kib = settings.speed_limit_kib.min(MAX_SPEED_LIMIT_KIB);
    if settings.queue_speed_limits.len() > MAX_QUEUE_COUNT {
        return Err(format!("Configure no more than {MAX_QUEUE_COUNT} queue speed limits"));
    }
    let mut queue_speed_limits = BTreeMap::new();
    for (raw_name, limit) in std::mem::take(&mut settings.queue_speed_limits) {
        let name = normalize_queue_name(&raw_name)?;
        if name.is_empty() { return Err("Queue speed limits need a queue name".into()); }
        if queue_speed_limits.insert(name, limit.min(MAX_SPEED_LIMIT_KIB)).is_some() {
            return Err("Queue speed-limit names must be unique after trimming".into());
        }
    }
    settings.queue_speed_limits = queue_speed_limits;
    if settings.download_dir.is_empty() {
        settings.download_dir = fallback.to_string_lossy().into_owned();
    }
    if !Path::new(&settings.download_dir).is_absolute() {
        return Err("Choose an absolute download folder".into());
    }
    Ok(settings)
}

impl Engine {
    pub async fn open(data_dir: PathBuf, download_dir: PathBuf) -> Result<Self, String> {
        if !data_dir.is_absolute() || !download_dir.is_absolute() {
            return Err("QDM requires absolute data and download folders".into());
        }

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
        let (events, _) = broadcast::channel(16);
        let engine = Self {
            inner: Arc::new(Inner {
                data_dir,
                state: Mutex::new(State {
                    settings,
                    tasks,
                    active: HashMap::new(),
                    host_caps: HashMap::new(),
                    revision: 0,
                    closing: false,
                    warning,
                }),
                persist_lock: tokio::sync::Mutex::new(()),
                events,
                client: client(true)?,
                stream_client: client(false)?,
                work_changed: Notify::new(),
                monitor_changed: Arc::new(Notify::new()),
                throttle_changed: Notify::new(),
                throttle: Mutex::new(Throttle::default()),
                dirty: AtomicBool::new(false),
                change_counter: AtomicU64::new(0),
            }),
        };
        engine.persist().await?; // Initialize SQLite before scheduling any jobs.
        engine.spawn_monitor();
        engine.kick();
        Ok(engine)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Snapshot> {
        self.inner.events.subscribe()
    }
    pub fn warning(&self) -> Option<String> {
        lock(&self.inner.state).warning.clone()
    }
    pub fn snapshot(&self) -> Snapshot {
        Self::snapshot_locked(&lock(&self.inner.state))
    }
    fn snapshot_locked(state: &State) -> Snapshot {
        Snapshot {
            downloads: state.tasks.iter().map(|t| t.view.clone()).collect(),
            settings: state.settings.clone(),
            active_count: state.active.len(),
            total_speed_bps: state.tasks.iter().map(|t| t.view.speed_bps).sum(),
            revision: state.revision,
        }
    }
    fn changed(&self) {
        self.inner.dirty.store(true, Ordering::Release);
        self.inner.change_counter.fetch_add(1, Ordering::Relaxed);
        self.inner.monitor_changed.notify_one();
    }
    fn emit(&self) {
        let snapshot = {
            let mut state = lock(&self.inner.state);
            state.revision += 1;
            Self::snapshot_locked(&state)
        };
        let _ = self.inner.events.send(snapshot);
    }
    fn task(&self, id: &str) -> Result<Task, String> {
        lock(&self.inner.state)
            .tasks
            .iter()
            .find(|t| t.view.id == id)
            .cloned()
            .ok_or_else(|| "Download no longer exists".into())
    }
    // A transfer has ONE temporary file on the same volume as the final output.
    // Independent workers use separate file handles and disjoint absolute offsets.
    fn part_path(&self, id: &str, _part: u64) -> PathBuf {
        let state = lock(&self.inner.state);
        let directory = state.tasks.iter().find(|t| t.view.id == id)
            .map(|t| PathBuf::from(&t.view.directory))
            .unwrap_or_else(|| self.inner.data_dir.clone());
        directory.join(format!(".qdm-{id}.partial"))
    }

    pub async fn add(&self, request: AddRequest) -> Result<String, String> {
        let url = validate_url(&request.url)?;
        request_headers(&request.headers)?;
        let queue = normalize_queue_name(&request.queue)?;
        let expected = request
            .expected_sha256
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.trim().to_lowercase());
        if expected
            .as_deref()
            .map(|s| s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()))
            .unwrap_or(false)
        {
            return Err("Expected SHA-256 must contain 64 hexadecimal characters".into());
        }
        let settings = lock(&self.inner.state).settings.clone();
        let directory = request
            .directory
            .filter(|s| !s.trim().is_empty())
            .unwrap_or(settings.download_dir.clone());
        if !Path::new(&directory).is_absolute() {
            return Err("Choose an absolute download folder".into());
        }
        fs::create_dir_all(&directory)
            .await
            .map_err(|e| e.to_string())?;
        let id = Uuid::new_v4().to_string();
        let scheduled = request.scheduled_at.filter(|time| *time > now_ms());
        let filename = request
            .filename
            .as_deref()
            .map(safe_filename)
            .unwrap_or_else(|| filename_from_url(&url));
        let status = if scheduled.is_some() {
            "scheduled"
        } else if request.start {
            "queued"
        } else {
            "paused"
        };
        let view = TaskView {
            id: id.clone(),
            url,
            filename,
            directory,
            status: status.into(),
            connections_limit: settings.connections,
            expected_sha256: expected,
            created_at: now_ms(),
            scheduled_at: scheduled,
            speed_limit_kib: request.speed_limit_kib.min(MAX_SPEED_LIMIT_KIB),
            queue,
            integrity: "pending".into(),
            ..TaskView::default()
        };
        {
            let mut state = lock(&self.inner.state);
            if state.closing {
                return Err("QDM is closing".into());
            }
            if state.tasks.len() >= HISTORY_LIMIT {
                return Err("Remove old history before adding more downloads".into());
            }
            state.tasks.insert(
                0,
                Task {
                    view,
                    parts: Vec::new(),
                    lane_activity: Vec::new(),
                    validator: Validator::default(),
                    resource_url: None,
                    custom_filename: request.filename.is_some(),
                    stream_mode: false,
                    headers: request.headers,
                    next_part_id: 1,
                    pending_output: None,
                    sampled_network: 0,
                    sampled_at: None,
                    adaptive_limit: 0,
                    adaptive_last_change: None,
                    adaptive_healthy_bytes: 0,
                    adaptive_starting: false,
                    startup_ready_slots: 0,
                    startup_next_growth: None,
                    request_not_before: None,
                    backoff_reduced: false,
                    rate_limit_rounds: 0,
                    transport_failure_streak: 0,
                    request_setup_micros: 0,
                    force_single_stream: false,
                },
            );
        }
        self.changed();
        self.persist().await?;
        self.emit();
        self.kick();
        Ok(id)
    }

    pub async fn update_download_limits(&self, id: &str, queue: &str, speed_limit_kib: u64) -> Result<(), String> {
        let queue = normalize_queue_name(queue)?;
        let speed_limit_kib = speed_limit_kib.min(MAX_SPEED_LIMIT_KIB);
        {
            let mut state = lock(&self.inner.state);
            let task = state.tasks.iter_mut().find(|t| t.view.id == id).ok_or("Download no longer exists")?;
            let speed_changed = task.view.speed_limit_kib != speed_limit_kib;
            task.view.queue = queue;
            task.view.speed_limit_kib = speed_limit_kib;
            if speed_changed { lock(&self.inner.throttle).per_task.remove(id); }
        }
        self.inner.throttle_changed.notify_waiters();
        self.changed();
        self.persist().await?;
        self.emit();
        Ok(())
    }

    pub async fn pause(&self, id: &str) -> Result<(), String> {
        let handle = {
            let mut state = lock(&self.inner.state);
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or("Download no longer exists")?;
            if task.view.status == "completed" {
                return Ok(());
            }
            let active = state.active.get(id).cloned();
            if active.is_none() {
                let task = state.tasks.iter_mut().find(|t| t.view.id == id).unwrap();
                task.view.status = "paused".into();
                task.view.scheduled_at = None;
                task.view.speed_bps = 0;
            }
            active
        };
        if let Some(mut handle) = handle {
            let _ = handle.stop.send(true);
            self.inner.work_changed.notify_waiters();
            while !*handle.done.borrow() {
                if handle.done.changed().await.is_err() {
                    break;
                }
            }
        }
        self.changed();
        self.persist().await?;
        self.emit();
        Ok(())
    }

    pub async fn pause_all(&self) -> Result<(), String> {
        let ids = {
            let mut state = lock(&self.inner.state);
            for t in &mut state.tasks {
                if matches!(t.view.status.as_str(), "queued" | "scheduled") {
                    t.view.status = "paused".into();
                    t.view.scheduled_at = None;
                }
            }
            // Ownership covers all transfer phases, including assembly and a
            // pending commit. Do not infer liveness from a subset of labels.
            state.active.keys().cloned().collect::<Vec<_>>()
        };
        for id in ids {
            self.pause(&id).await?;
        }
        self.changed();
        self.persist().await?;
        self.emit();
        Ok(())
    }

    pub async fn resume(&self, id: &str) -> Result<(), String> {
        {
            let mut state = lock(&self.inner.state);
            if state.closing {
                return Err("QDM is closing".into());
            }
            if state.active.contains_key(id) {
                return Ok(());
            }
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or("Download no longer exists")?;
            if matches!(
                task.view.status.as_str(),
                "completed" | "downloading" | "verifying" | "assembling"
            ) {
                return Ok(());
            }
            task.view.status = "queued".into();
            task.view.error = None;
            task.view.scheduled_at = None;
        }
        self.changed();
        self.persist().await?;
        self.emit();
        self.kick();
        Ok(())
    }

    pub async fn resume_all(&self) -> Result<(), String> {
        {
            let mut state = lock(&self.inner.state);
            for task in &mut state.tasks {
                if matches!(task.view.status.as_str(), "paused" | "error") {
                    task.view.status = "queued".into();
                    task.view.error = None;
                    task.view.scheduled_at = None;
                }
            }
        }
        self.changed();
        self.persist().await?;
        self.emit();
        self.kick();
        Ok(())
    }

    pub async fn remove_with_file(&self, id: &str) -> Result<Option<&'static str>, String> {
        let task = self.task(id)?;
        if task.view.status != "completed" { return Err("Only completed downloads can have their output deleted".into()); }
        let raw = task.view.file_path.as_deref().filter(|p| !p.trim().is_empty()).ok_or("Completed download has no output file")?;
        let path = Path::new(raw);
        match fs::symlink_metadata(path).await {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => { self.remove(id).await?; return Ok(None); },
            Err(e) => return Err(format!("Cannot inspect output file: {e}")),
            Ok(_) => {}
        }
        if !path.is_absolute() || path.file_name().is_none() { return Err("Output file path must be an absolute file path".into()); }
        let metadata = fs::symlink_metadata(path).await.map_err(|e| format!("Cannot inspect output file: {e}"))?;
        if !metadata.file_type().is_file() { return Err("Output is not a regular file or is a link".into()); }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt;
            if metadata.file_attributes() & 0x400 != 0 { return Err("Output is a reparse point".into()); }
        }
        let target = fs::canonicalize(path).await.map_err(|e| format!("Cannot resolve output file: {e}"))?;
        let directory = Path::new(&task.view.directory);
        if !directory.is_absolute() { return Err("Download directory must be absolute".into()); }
        let directory = fs::canonicalize(directory).await.map_err(|e| format!("Cannot resolve download directory: {e}"))?;
        if !target.parent().is_some_and(|parent| parent.starts_with(&directory)) { return Err("Output file is outside its download directory".into()); }
        let other_paths: Vec<_> = {
            let state = lock(&self.inner.state);
            state.tasks.iter().filter(|other| other.view.id != id).filter_map(|other| other.view.file_path.clone()).collect()
        };
        for other in other_paths {
            if let Ok(other) = fs::canonicalize(other).await {
                if other == target {
                    // Remove only this history entry. Never delete a file still referenced by another task.
                    self.remove(id).await?;
                    return Ok(Some("History entry removed. Shared output file kept until its final reference is removed."));
                }
            }
        }
        fs::remove_file(path).await.map_err(|e| format!("Cannot delete output file: {e}"))?;
        self.remove(id).await?;
        Ok(None)
    }

    pub async fn remove(&self, id: &str) -> Result<(), String> {
        self.pause(id).await?;
        let temporary = self.part_path(id, 0);
        {
            let mut state = lock(&self.inner.state);
            state.tasks.retain(|t| t.view.id != id);
            lock(&self.inner.throttle).per_task.remove(id);
        }
        if Uuid::parse_str(id).is_ok() {
            let _ = fs::remove_file(temporary).await;
        }
        self.changed();
        self.persist().await?;
        self.emit();
        self.kick();
        Ok(())
    }

    pub async fn replace_url(&self, id: &str, url: &str) -> Result<(), String> {
        let normalized = validate_url(url)?;
        let task = self.task(id)?;
        if task.view.status == "completed" {
            return Err("Completed downloads do not need a replacement URL".into());
        }
        let mut headers = task.headers;
        if url::Url::parse(&task.view.url).ok().map(|u| u.origin())
            != url::Url::parse(&normalized).ok().map(|u| u.origin())
        {
            headers.clear();
        }
        self.replace_context(id, &normalized, headers).await
    }

    pub async fn replace_context(
        &self,
        id: &str,
        url: &str,
        headers: BTreeMap<String, String>,
    ) -> Result<(), String> {
        let normalized = validate_url(url)?;
        request_headers(&headers)?;
        if self.task(id)?.view.status == "completed" {
            return Err("Completed downloads do not need a replacement URL".into());
        }
        self.pause(id).await?;
        {
            let mut state = lock(&self.inner.state);
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or("Download no longer exists")?;
            task.view.url = normalized;
            task.headers = headers;
            task.view.error = None;
            task.view.notice = Some(
                "The replacement link will be checked against saved file data before resuming."
                    .into(),
            );
        }
        self.changed();
        self.persist().await?;
        self.emit();
        Ok(())
    }

    pub async fn update_settings(&self, settings: Settings) -> Result<(), String> {
        let fallback = PathBuf::from(lock(&self.inner.state).settings.download_dir.clone());
        let settings = validated_settings(settings, &fallback)?;
        fs::create_dir_all(&settings.download_dir)
            .await
            .map_err(|e| e.to_string())?;
        {
            let mut state = lock(&self.inner.state);
            let global_limit_changed = state.settings.speed_limit_kib != settings.speed_limit_kib;
            let changed_queues: Vec<_> = state.settings.queue_speed_limits.keys()
                .chain(settings.queue_speed_limits.keys())
                .filter(|name| state.settings.queue_speed_limits.get(*name) != settings.queue_speed_limits.get(*name))
                .cloned().collect();
            state.settings = settings.clone();
            for task in &mut state.tasks {
                task.view.connections_limit = settings.connections;
            }
            let mut throttle = lock(&self.inner.throttle);
            if global_limit_changed { throttle.global = RateBucket::default(); }
            for queue in changed_queues { throttle.per_queue.remove(&queue); }
        }
        self.inner.throttle_changed.notify_waiters();
        self.inner.work_changed.notify_waiters();
        self.changed();
        self.persist().await?;
        self.emit();
        self.kick();
        Ok(())
    }

    pub async fn shutdown(&self) -> Result<(), String> {
        let handles = {
            let mut state = lock(&self.inner.state);
            state.closing = true;
            state.active.values().cloned().collect::<Vec<_>>()
        };
        self.inner.monitor_changed.notify_one();
        for handle in &handles {
            let _ = handle.stop.send(true);
        }
        self.inner.work_changed.notify_waiters();
        self.inner.throttle_changed.notify_waiters();
        for mut handle in handles {
            while !*handle.done.borrow() {
                if handle.done.changed().await.is_err() {
                    break;
                }
            }
        }
        self.persist().await
    }

    async fn persist(&self) -> Result<(), String> {
        let _serial = self.inner.persist_lock.lock().await;
        let saved = {
            let state = lock(&self.inner.state);
            Saved {
                schema_version: 3,
                settings: state.settings.clone(),
                tasks: state.tasks.clone(),
            }
        };
        store::save(self.inner.data_dir.join("state-v3.sqlite3"), saved).await

    }

    fn spawn_monitor(&self) {
        let weak = Arc::downgrade(&self.inner);
        tokio::spawn(async move {
            let mut last_save = Instant::now();
            let mut last_change = 0;
            loop {
                let Some(inner) = weak.upgrade() else { break };
                let engine = Engine { inner };
                let notified = engine.inner.monitor_changed.clone().notified_owned();
                tokio::pin!(notified);
                notified.as_mut().enable();
                let closing = {
                    let mut state = lock(&engine.inner.state);
                    let now = Instant::now();
                    for task in &mut state.tasks {
                        if task.view.status == "downloading" {
                            if let Some(last) = task.sampled_at {
                                let seconds = now.duration_since(last).as_secs_f64();
                                if seconds > 0.0 {
                                    task.view.speed_bps = ((task
                                        .view
                                        .network_bytes
                                        .saturating_sub(task.sampled_network))
                                        as f64
                                        / seconds)
                                        as u64;
                                }
                            }
                            task.sampled_network = task.view.network_bytes;
                            task.sampled_at = Some(now);
                            task.refresh();
                        }
                    }
                    state.closing
                };
                if closing {
                    break;
                }
                engine.kick();
                let (active, scheduled) = {
                    let state = lock(&engine.inner.state);
                    (
                        !state.active.is_empty(),
                        state
                            .tasks
                            .iter()
                            .filter(|t| t.view.status == "scheduled")
                            .filter_map(|t| t.view.scheduled_at)
                            .min(),
                    )
                };
                let change = engine.inner.change_counter.load(Ordering::Relaxed);
                if active || change != last_change {
                    engine.emit();
                    last_change = change;
                }
                if last_save.elapsed() >= Duration::from_millis(1500)
                    && engine.inner.dirty.swap(false, Ordering::AcqRel)
                {
                    if let Err(error) = engine.persist().await {
                        lock(&engine.inner.state).warning =
                            Some(format!("Could not save download progress: {error}"));
                        engine.inner.dirty.store(true, Ordering::Release);
                    }
                    last_save = Instant::now();
                }
                let checkpoint = engine
                    .inner
                    .dirty
                    .load(Ordering::Acquire)
                    .then(|| Duration::from_millis(1500).saturating_sub(last_save.elapsed()));
                let scheduled =
                    scheduled.map(|due| Duration::from_millis(due.saturating_sub(now_ms())));
                let wait = match (checkpoint, scheduled) {
                    (Some(a), Some(b)) => Some(a.min(b)),
                    (a, b) => a.or(b),
                };
                drop(engine);
                if active {
                    tokio::time::sleep(Duration::from_millis(200)).await;
                } else if let Some(wait) = wait {
                    tokio::select! {_=notified.as_mut()=>{},_=tokio::time::sleep(wait)=>{}}
                } else {
                    notified.as_mut().await;
                }
            }
        });
    }

    fn kick(&self) {
        let launches = {
            let mut state = lock(&self.inner.state);
            if state.closing {
                return;
            }
            let now = now_ms();
            for task in &mut state.tasks {
                if task.view.status == "scheduled"
                    && task
                        .view
                        .scheduled_at
                        .map(|time| time <= now)
                        .unwrap_or(false)
                {
                    task.view.status = "queued".into();
                    task.view.scheduled_at = None;
                }
            }
            let count = (state.settings.max_concurrent as usize).saturating_sub(state.active.len());
            let ids = state
                .tasks
                .iter()
                .rev()
                .filter(|t| t.view.status == "queued" && !state.active.contains_key(&t.view.id))
                .take(count)
                .map(|t| t.view.id.clone())
                .collect::<Vec<_>>();
            let mut launches = Vec::new();
            for id in ids {
                let (stop, stop_rx) = watch::channel(false);
                let (done, done_rx) = watch::channel(false);
                state.active.insert(
                    id.clone(),
                    RunHandle {
                        stop,
                        done: done_rx,
                    },
                );
                let task = state.tasks.iter_mut().find(|t| t.view.id == id).unwrap();
                task.view.status = "downloading".into();
                task.view.error = None;
                task.sampled_at = Some(Instant::now());
                task.sampled_network = task.view.network_bytes;
                launches.push((id, stop_rx, done));
            }
            launches
        };
        if !launches.is_empty() {
            self.changed();
            self.emit();
        }
        for (id, stop, done) in launches {
            let engine = self.clone();
            tokio::spawn(Box::pin(async move {
                engine.run_owned(id, stop, done).await;
            }));
        }
    }
}


#[cfg(test)]
mod removal_tests {
    use super::*;

    async fn fixture() -> (PathBuf, Engine, PathBuf) {
        let root = std::env::temp_dir().join(format!("qdm-removal-{}", Uuid::new_v4()));
        let downloads = root.join("downloads");
        fs::create_dir_all(&downloads).await.unwrap();
        let engine = Engine::open(root.join("state"), downloads.clone()).await.unwrap();
        (root, engine, downloads)
    }
    fn add_task(engine: &Engine, directory: &Path, path: &Path, status: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let view = TaskView { id: id.clone(), url: "https://example.com/test.bin".into(), filename: "test.bin".into(), status: status.into(), directory: directory.to_string_lossy().into_owned(), file_path: Some(path.to_string_lossy().into_owned()), ..Default::default() };
        let task: Task = serde_json::from_value(serde_json::json!({"view": view})).unwrap();
        lock(&engine.inner.state).tasks.push(task);
        id
    }
    #[tokio::test]
    async fn list_only_preserves_file_and_delete_removes_it() {
        let (root, engine, dir) = fixture().await;
        let file = dir.join("output.bin"); fs::write(&file, b"fixture").await.unwrap();
        let id = add_task(&engine, &dir, &file, "completed");
        engine.remove(&id).await.unwrap(); assert!(file.is_file());
        let id = add_task(&engine, &dir, &file, "completed");
        engine.remove_with_file(&id).await.unwrap(); assert!(!file.exists()); assert!(engine.task(&id).is_err());
        engine.shutdown().await.unwrap(); fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn shared_output_is_kept_until_the_final_history_reference_is_removed() {
        let (root, engine, dir) = fixture().await;
        let file = dir.join("shared-output.bin");
        fs::write(&file, b"downloaded-data").await.unwrap();
        let ids: Vec<_> = (0..4).map(|_| add_task(&engine, &dir, &file, "completed")).collect();
        for id in &ids[..3] {
            let notice = engine.remove_with_file(id).await.unwrap();
            assert!(notice.is_some(), "shared file must explain why it was preserved");
            assert!(engine.task(id).is_err());
            assert!(file.is_file(), "file must stay while another task references it");
        }
        let final_notice = engine.remove_with_file(&ids[3]).await.unwrap();
        assert!(final_notice.is_none(), "final reference must delete the real file");
        assert!(!file.exists());
        assert!(engine.task(&ids[3]).is_err());
        engine.shutdown().await.unwrap();
        fs::remove_dir_all(root).await.unwrap();
    }

    #[tokio::test]
    async fn missing_outputs_remove_history_including_bulk() {
        let (root, engine, dir) = fixture().await;
        let existing = dir.join("keep.bin"); fs::write(&existing, b"keep").await.unwrap();
        let ids: Vec<_> = (0..3).map(|n| add_task(&engine, &dir, &dir.join(format!("missing-{n}.bin")), "completed")).collect();
        for id in &ids { assert!(engine.remove_with_file(id).await.unwrap().is_none(), "missing file is normal, not a warning"); assert!(engine.task(id).is_err()); }
        assert_eq!(fs::read(&existing).await.unwrap(), b"keep");
        engine.shutdown().await.unwrap(); fs::remove_dir_all(root).await.unwrap();
    }
    #[tokio::test]
    async fn persisted_remove_choices_survive_restart() {
        let (root, engine, dir) = fixture().await;
        let keep = dir.join("keep.bin");
        let remove_file = dir.join("remove.bin");
        fs::write(&keep, b"keep").await.unwrap();
        fs::write(&remove_file, b"delete").await.unwrap();
        let a = add_task(&engine, &dir, &keep, "completed");
        let b = add_task(&engine, &dir, &remove_file, "completed");
        let c = add_task(&engine, &dir, &dir.join("missing.bin"), "completed");
        engine.persist().await.unwrap(); engine.shutdown().await.unwrap(); drop(engine);
        let engine = Engine::open(root.join("state"), dir.clone()).await.unwrap();
        assert!(engine.task(&a).is_ok() && engine.task(&b).is_ok() && engine.task(&c).is_ok());
        engine.remove(&a).await.unwrap();
        assert!(engine.remove_with_file(&b).await.unwrap().is_none());
        assert!(engine.remove_with_file(&c).await.unwrap().is_none());
        engine.shutdown().await.unwrap(); drop(engine);
        let engine = Engine::open(root.join("state"), dir.clone()).await.unwrap();
        assert!(engine.task(&a).is_err() && engine.task(&b).is_err() && engine.task(&c).is_err());
        assert_eq!(fs::read(&keep).await.unwrap(), b"keep");
        assert!(!remove_file.exists());
        engine.shutdown().await.unwrap(); fs::remove_dir_all(root).await.unwrap();
    }
    #[test]
    fn simultaneous_files_share_only_their_own_origin_budget() {
        let mut state = State {
            settings: Settings { connections: 64, ..Settings::default() },
            tasks: Vec::new(), active: HashMap::new(),
            host_caps: HashMap::new(), revision: 0,
            closing: false, warning: None,
        };
        for (id, url) in [("one", "https://cdn.example/a"),
            ("two", "https://cdn.example/b"), ("other", "https://other.example/c")] {
            let view = TaskView { id: id.into(), url: url.into(),
                status: "downloading".into(), ..TaskView::default() };
            let task: Task = serde_json::from_value(serde_json::json!({ "view": view })).unwrap();
            state.tasks.push(task);
            let (stop, _) = watch::channel(false);
            let (_, done) = watch::channel(true);
            state.active.insert(id.into(), RunHandle { stop, done });
        }
        // Do not cap ordinary multi-file hosts that never returned 429.
        assert_eq!(origin_worker_budget(&state, "one"), 64);
        assert_eq!(origin_worker_budget(&state, "two"), 64);
        state.host_caps.insert("https://cdn.example:443".into(),
            HostCap { connections: 8, learned_at: Instant::now() });
        assert_eq!(origin_worker_budget(&state, "one"), 4);
        assert_eq!(origin_worker_budget(&state, "two"), 4);
        assert_eq!(origin_worker_budget(&state, "other"), 64);
        // A learned low ceiling must recover without waiting the full TTL.
        state.host_caps.get_mut("https://cdn.example:443").unwrap().learned_at =
            Instant::now() - Duration::from_secs(16);
        assert_eq!(origin_worker_budget(&state, "one"), 5);
        assert_eq!(origin_worker_budget(&state, "two"), 5);
        assert_eq!(origin_key("https://cdn.example/b?token=hidden"),
            Some("https://cdn.example:443".into()));
    }
    #[tokio::test]
    async fn rejects_unsafe_files_and_retains_history() {
        let (root, engine, dir) = fixture().await;
        let outside = root.join("outside.bin"); fs::write(&outside, b"fixture").await.unwrap();
        for path in [&outside, &dir] {
            let id = add_task(&engine, &dir, path, "completed");
            assert!(engine.remove_with_file(&id).await.is_err()); assert!(engine.task(&id).is_ok());
        }
        let file = dir.join("shared.bin"); fs::write(&file, b"fixture").await.unwrap();
        let id = add_task(&engine, &dir, &file, "completed");
        let other = add_task(&engine, &dir, &file, "completed");
        let note = engine.remove_with_file(&id).await.unwrap();
        assert!(note.is_some()); assert!(engine.task(&id).is_err()); assert!(engine.task(&other).is_ok()); assert!(file.exists());
        let id = add_task(&engine, &dir, &file, "paused");
        assert!(engine.remove_with_file(&id).await.is_err()); assert!(engine.task(&id).is_ok());
        let link = dir.join("link.bin");
        #[cfg(windows)] let linked = std::os::windows::fs::symlink_file(&outside, &link).is_ok();
        #[cfg(unix)] let linked = std::os::unix::fs::symlink(&outside, &link).is_ok();
        if linked { let id = add_task(&engine, &dir, &link, "completed"); assert!(engine.remove_with_file(&id).await.is_err()); assert!(engine.task(&id).is_ok()); assert!(outside.exists()); }
        engine.shutdown().await.unwrap(); fs::remove_dir_all(root).await.unwrap();
    }
}
