use super::protocol::{
    self, collision_filename, content_length, content_range, disposition_filename, header_string,
    MIN_SEGMENT, OVERLAP,
};
use super::*;
use futures_util::FutureExt;
use reqwest::{
    header::{HeaderMap, ACCEPT_ENCODING, IF_RANGE, RANGE},
    Response,
};
use sha2::{Digest, Sha256};
use std::{future::Future, io::SeekFrom, panic::AssertUnwindSafe};
use tokio::{
    io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt, BufWriter},
    task::JoinSet,
};

const NETWORK_IDLE: Duration = Duration::from_secs(45);
const HEADER_TIMEOUT: Duration = Duration::from_secs(30);
// Coalesce durable checkpoints across fast segments. Completed pieces always
// sync immediately, while long streams revalidate at least every 3 seconds.
const CHECKPOINT_INTERVAL: Duration = Duration::from_secs(3);
const STARTUP_INTERVAL: Duration = Duration::from_millis(100);
const INITIAL_CONNECTIONS: u8 = 2;
// Tokio dispatches ordinary file writes to its blocking pool. Coalesce small
// network frames without buffering a whole part/file: 16 MiB at 64 workers.
const PART_BUFFER_BYTES: usize = 256 * 1024;
type PartWriter = BufWriter<fs::File>;

// Diagnostic records contain local task/part IDs, byte ranges and timings,
// never a URL, request headers, cookies or authentication material. Disabled
// by default; constructing the JSON is also skipped on the production path.
fn trace_transfer(event: &str, fields: impl FnOnce() -> serde_json::Value) {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if !*ENABLED.get_or_init(|| std::env::var("QDM_TRANSFER_TRACE").ok().as_deref() == Some("1")) {
        return;
    }
    let mut value = fields();
    if let Some(record) = value.as_object_mut() {
        record.insert("event".into(), event.into());
        record.insert("atMs".into(), now_ms().into());
        record.insert("processId".into(), std::process::id().into());
    }
    eprintln!("QDM_TRANSFER_TRACE {value}");
}

fn trace_headers(id: &str, part_id: Option<u64>, response: &Response, started: Instant) {
    trace_transfer("headers", || serde_json::json!({
        "task": id, "part": part_id, "status": response.status().as_u16(),
        "setupMs": started.elapsed().as_millis(),
        "remote": response.remote_addr().map(|address| address.to_string()),
        "contentRange": header_string(response.headers(), "content-range"),
        "server": header_string(response.headers(), "server"),
        "cache": header_string(response.headers(), "x-cache"),
        "retryAfter": header_string(response.headers(), "retry-after"),
    }));
}

// Splitting has an up-front request cost. A productive parent should retain a
// short tail that it can finish sooner than another request can help. Revisit
// a deferred split if that parent later stops making progress; no idle poll is
// needed. A response without payload is never recursively subdivided.
fn split_ready_at(part: &Part, now: Instant, setup: Duration) -> Option<Instant> {
    if !part.active || part.response_end == 0 || part.response_payload_bytes == 0
        || part.remaining() < 2 * MIN_SEGMENT {
        return None;
    }
    let started = part.response_started_at?;
    let age = now.saturating_duration_since(started).as_secs_f64();
    // The child's half must offer at least twice the observed setup cost in
    // saved transfer time. The remaining budget includes an in-flight write.
    let useful_age = 4.0 * setup.as_secs_f64()
        * part.response_payload_bytes as f64 / part.remaining() as f64;
    // For a large outstanding download, avoid holding all spare workers for
    // nearly a minute solely because a CDN had an expensive first response.
    // Small tails retain the full setup-cost deferral to avoid slower finishes.
    let cap = if part.remaining() >= 32 * MIN_SEGMENT {
        Duration::from_secs(3)
    } else { NETWORK_IDLE };
    let wait = (useful_age - age).max(0.0).min(cap.as_secs_f64());
    Some(now + Duration::from_secs_f64(wait))
}

// Response coverage must disappear before a worker checkpoints a completed
// response. A speculative child may only return ownership to a live response.
struct ResponseCoverage {
    engine: Engine,
    task_id: String,
    part_id: u64,
}
impl Drop for ResponseCoverage {
    fn drop(&mut self) {
        let mut state = lock(&self.engine.inner.state);
        if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == self.task_id) {
            if let Some(part) = task.parts.iter_mut().find(|part| part.id == self.part_id) {
                part.response_end = 0;
                part.request_started_at = None;
                part.response_started_at = None;
                part.response_payload_bytes = 0;
            }
        }
    }
}

fn restore_untouched_split(parts: &mut Vec<Part>, child_id: u64) -> bool {
    let Some(child_index) = parts.iter().position(|part| part.id == child_id) else { return false; };
    let child = &parts[child_index];
    let Some(parent_id) = child.split_parent else { return false; };
    if !child.active || child.downloaded != 0 || child.durable != 0
        || child.reserved != 0 || child.response_end != 0 { return false; }
    let Some(parent_index) = parts.iter().position(|part| part.id == parent_id) else { return false; };
    let parent = &parts[parent_index];
    // Include the pending write reservation: a network chunk clipped at the
    // old end has already discarded its suffix. Extending ownership during
    // that final write would otherwise skip those bytes in the live response.
    if !parent.active || parent.remaining() == 0
        || parent.end != child.start || parent.response_end < child.end { return false; }
    let restored_end = child.end;
    parts[parent_index].end = restored_end;
    parts.remove(child_index);
    // A not-yet-accepted child can itself have an idle worker split its tail.
    // Keep that descendant's ownership; only update its possible live parent.
    for part in parts.iter_mut() {
        if part.split_parent == Some(child_id) { part.split_parent = Some(parent_id); }
    }
    true
}

#[derive(Debug)]
enum Fault {
    Stopped,
    Retry(String, Duration),
    Transport(String, Duration),
    Restart(String),
    Fatal(String),
}

impl Fault {
    fn message(self) -> String {
        match self {
            Self::Stopped => "Paused".into(),
            Self::Retry(s, _) | Self::Transport(s, _) | Self::Restart(s) | Self::Fatal(s) => s,
        }
    }
}

#[derive(Clone)]
struct Stop {
    task: watch::Receiver<bool>,
    group: watch::Receiver<bool>,
}
impl Stop {
    fn check(&self) -> Result<(), Fault> {
        if *self.task.borrow() || *self.group.borrow() {
            Err(Fault::Stopped)
        } else {
            Ok(())
        }
    }
    async fn wait(&mut self) {
        if self.check().is_err() {
            return;
        }
        tokio::select! { _ = self.task.changed() => {}, _ = self.group.changed() => {} }
    }
}

#[derive(Clone)]
struct Meta {
    total: u64,
    validator: Validator,
    url: String,
    filename: Option<String>,
}
enum StreamOutcome {
    Complete,
    Promote(Meta),
}
enum Probe {
    Range(Meta),
    Full(Response),
}
enum Claim {
    Part(Part),
    Wait,
    WaitUntil(Instant),
    Done,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn disk(error: std::io::Error) -> Fault {
    Fault::Fatal(format!("Could not write or read download data: {error}"))
}

pub(super) async fn check_local_tail(path: &Path, absolute_end: u64, size: u64, expected: Option<&str>) -> bool {
    let Some(expected) = expected else {
        return false;
    };
    async {
        let mut file = fs::File::open(path).await.ok()?;
        let len = size.min(OVERLAP);
        file.seek(SeekFrom::Start(absolute_end - len)).await.ok()?;
        let mut bytes = vec![0; len as usize];
        file.read_exact(&mut bytes).await.ok()?;
        Some(digest(&bytes) == expected)
    }
    .await
    .unwrap_or(false)
}

pub(super) async fn recover_commit(task: &mut Task) {
    let Some(pending) = task.pending_output.clone() else {
        return;
    };
    let path = Path::new(&pending.path);
    if path.parent() != Some(Path::new(&task.view.directory))
        || path.file_name().and_then(|s| s.to_str()) != Some(pending.filename.as_str())
        || pending.sha256.len() != 64
    {
        return;
    }
    let verified = async {
        let mut file = fs::File::open(path).await.ok()?;
        if file.metadata().await.ok()?.len() != pending.size {
            return Some(false);
        }
        let mut hasher = Sha256::new();
        let mut buffer = vec![0; 1024 * 1024];
        loop {
            let count = file.read(&mut buffer).await.ok()?;
            if count == 0 {
                break;
            }
            hasher.update(&buffer[..count]);
        }
        Some(format!("{:x}", hasher.finalize()) == pending.sha256)
    }
    .await
    .unwrap_or(false);
    if verified {
        task.view.filename = pending.filename;
        task.view.file_path = Some(pending.path);
        task.view.sha256 = Some(pending.sha256);
        task.view.total_bytes = Some(pending.size);
        task.view.downloaded_bytes = pending.size;
        task.view.status = "completed".into();
        task.view.completed_at = Some(now_ms());
        task.view.error = None;
        task.view.notice =
            Some("Recovered a verified file committed before QDM last stopped.".into());
        task.pending_output = None;
        let temporary = Path::new(&pending.temporary_path);
        if temporary.parent() == Some(Path::new(&task.view.directory))
            && temporary
                .file_name()
                .and_then(|s| s.to_str())
                .map(|s| {
                    s == format!(".qdm-{}.partial", task.view.id)
                })
                .unwrap_or(false)
        {
            let _ = fs::remove_file(temporary).await;
        }
    }
}

#[cfg(windows)]
async fn move_windows(source: &Path, destination: &Path, flags: u32) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    // Rust canonicalization supplies extended Windows paths, including UNC
    // paths, so publication also works beyond the legacy MAX_PATH boundary.
    let source = fs::canonicalize(source).await?;
    let destination = fs::canonicalize(
        destination
            .parent()
            .ok_or_else(|| std::io::Error::other("Missing destination folder"))?,
    )
    .await?
    .join(
        destination
            .file_name()
            .ok_or_else(|| std::io::Error::other("Missing destination filename"))?,
    );
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    tokio::task::spawn_blocking(move || {
        #[link(name = "kernel32")]
        extern "system" {
            fn MoveFileExW(existing: *const u16, new: *const u16, flags: u32) -> i32;
        }
        if unsafe { MoveFileExW(source.as_ptr(), destination.as_ptr(), flags) } == 0 {
            Err(std::io::Error::last_os_error())
        } else {
            Ok(())
        }
    })
    .await
    .map_err(|error| std::io::Error::other(error.to_string()))?
}

#[cfg(windows)]
// No REPLACE_EXISTING and no COPY_ALLOWED: same-volume, no-overwrite
// publication. The persisted commit journal handles interrupted commits.
async fn promote_exclusive(source: &Path, destination: &Path) -> std::io::Result<()> {
    move_windows(source, destination, 0x8).await
}

pub(super) async fn replace_history(source: &Path, destination: &Path) -> std::io::Result<()> {
    #[cfg(windows)]
    {
        // Readers, indexers and security scanners can briefly open the old
        // history without FILE_SHARE_DELETE. Keep the last valid file intact
        // and retry the SAME atomic replacement; never delete it first.
        // Persistent ACL/sharing failures still reach the caller unchanged.
        for attempt in 0..=6u32 {
            match move_windows(source, destination, 0x9).await {
                Ok(()) => return Ok(()),
                Err(error) if attempt < 6 && matches!(error.raw_os_error(), Some(5 | 32 | 33 | 1224)) => {
                    tokio::time::sleep(Duration::from_millis(10u64 << attempt)).await;
                }
                Err(error) => return Err(error),
            }
        }
        unreachable!()
    }
    #[cfg(not(windows))]
    {
        fs::rename(source, destination).await?;
        if let Some(parent) = destination.parent() {
            fs::File::open(parent).await?.sync_all().await?;
        }
        Ok(())
    }
}

#[cfg(not(windows))]
async fn promote_exclusive(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::hard_link(source, destination).await
}

fn network_error_message(error: reqwest::Error) -> String {
    use std::error::Error;
    // Reqwest's Display omits the underlying transport/TLS/OS error. Keep that
    // cause visible so a retry can be diagnosed, while omitting signed URLs or
    // credentials from persisted notices and error messages.
    let error = error.without_url();
    let mut message = format!("Network interrupted: {error}");
    let mut source = error.source();
    let mut depth = 0;
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        depth += 1;
        if depth == 8 { break; }
        source = cause.source();
    }
    message
}

async fn network<T, F>(stop: &mut Stop, duration: Duration, future: F) -> Result<T, Fault>
where
    F: Future<Output = Result<T, reqwest::Error>>,
{
    stop.check()?;
    tokio::select! {
        biased;
        _=stop.wait()=>Err(Fault::Stopped),
        result=tokio::time::timeout(duration,future)=>match result {
            Ok(Ok(value))=>Ok(value),
            Ok(Err(error))=>Err(Fault::Transport(network_error_message(error),Duration::ZERO)),
            Err(_)=>Err(Fault::Transport("The server stopped responding".into(),Duration::ZERO)),
        }
    }
}

fn response_status(response: &Response) -> Result<(), Fault> {
    let status = response.status().as_u16();
    match status {
        200..=299=>Ok(()),
        408|425|429|500|502|503|504=>Err(Fault::Retry(format!("The server returned HTTP {status}"),protocol::retry_after(response.headers()))),
        401|403=>Err(Fault::Fatal(format!("HTTP {status}: this link may have expired or require browser credentials. Use Refresh URL with a fresh link."))),
        _=>Err(Fault::Fatal(format!("The server returned HTTP {status}"))),
    }
}

fn encoded(headers: &HeaderMap) -> bool {
    header_string(headers, "content-encoding")
        .map(|s| !s.eq_ignore_ascii_case("identity"))
        .unwrap_or(false)
}

fn validate_range(response: &Response, start: u64, end: u64, meta: &Meta) -> Result<(), Fault> {
    if response.status().as_u16() == 200 {
        let validator = Validator::from_headers(response.headers());
        if validator.changed_from(&meta.validator) || response.url().as_str() != meta.url
            || content_length(response.headers()).map_err(Fault::Restart)?
                .map(|length| length != meta.total).unwrap_or(false) {
            return Err(Fault::Restart("The remote representation changed during a range request.".into()));
        }
        return Err(Fault::Retry("Range request refused with HTTP 200".into(), Duration::ZERO));
    }
    if response.status().as_u16() == 416 {
        return Err(Fault::Restart(
            "The remote file length changed; restarting one complete stream.".into(),
        ));
    }
    response_status(response)?;
    if response.status().as_u16() != 206 || encoded(response.headers()) {
        return Err(Fault::Restart(
            "The server did not return an identity-encoded partial response.".into(),
        ));
    }
    let range = header_string(response.headers(), "content-range")
        .as_deref()
        .and_then(content_range)
        .ok_or_else(|| {
            Fault::Restart("The server sent an invalid Content-Range; restarting safely.".into())
        })?;
    if range.start != start
        || range.end != end
        || range.total != meta.total
        || content_length(response.headers())
            .map_err(Fault::Restart)?
            .map(|n| n != end - start + 1)
            .unwrap_or(false)
    {
        return Err(Fault::Restart(
            "The server returned bytes outside the requested range; restarting safely.".into(),
        ));
    }
    let validator = Validator::from_headers(response.headers());
    if validator.changed_from(&meta.validator) || response.url().as_str() != meta.url {
        return Err(Fault::Restart("The remote representation changed between connections; restarting one complete stream.".into()));
    }
    if let Some(etag) = meta.validator.strong_etag() {
        if validator.strong_etag().is_some() && validator.strong_etag() != Some(etag) {
            return Err(Fault::Restart("The remote ETag changed".into()));
        }
    }
    Ok(())
}

impl Engine {
    async fn run_stream(
        &self,
        id: &str,
        mut initial: Option<Response>,
        stop: &mut Stop,
    ) -> Result<StreamOutcome, Fault> {
        let retries = lock(&self.inner.state).settings.retries;
        for attempt in 0..=retries {
            let response = match initial.take() {
                Some(response) => Ok(response),
                None => self.full_request(id, stop).await,
            };
            let result = match response {
                Ok(response) => self.stream_attempt(id, response, stop).await,
                Err(error) => Err(error),
            };
            match result {
                Err(Fault::Retry(message, delay) | Fault::Transport(message, delay)) if attempt < retries => {
                    self.delay_retry(id, attempt, &message, delay, stop).await?
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn stream_attempt(
        &self,
        id: &str,
        mut response: Response,
        stop: &mut Stop,
    ) -> Result<StreamOutcome, Fault> {
        let total = content_length(response.headers()).map_err(Fault::Fatal)?;
        if total.map(|n| n > i64::MAX as u64).unwrap_or(false) {
            return Err(Fault::Fatal(
                "The remote file is larger than the supported filesystem size".into(),
            ));
        }
        let stream_url = response.url().as_str().to_owned();
        let stream_validator = Validator::from_headers(response.headers());
        let filename = header_string(response.headers(), "content-disposition")
            .as_deref()
            .and_then(disposition_filename);
        {
            let mut state = lock(&self.inner.state);
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
            task.parts = vec![Part {
                id: 1,
                start: 0,
                end: total.unwrap_or(0),
                active: true,
                owner_lane: Some(0),
                ..Part::default()
            }];
            task.lane_activity = vec![LaneRecord::default(); 64];
            task.lane_activity[0] = LaneRecord {
                current_total: total.unwrap_or(0),
                current_part_id: Some(1),
                active: true,
                ..LaneRecord::default()
            };
            task.view.lane_history_estimated = false;
            task.next_part_id = 2;
            task.stream_mode = true;
            task.view.total_bytes = total;
            task.view.supports_ranges = false;
            task.view.resumable = false;
            task.view.resumed_bytes = 0;
            task.view.integrity = "single_stream".into();
            task.resource_url = Some(response.url().as_str().into());
            task.validator = Validator::from_headers(response.headers());
            if !task.custom_filename {
                if let Some(filename) = filename {
                    task.view.filename = filename;
                }
            }
            task.refresh();
        }
        // A fallback stream owns the same single temporary file, starting
        // from byte zero. This never reuses suspect Range fragments.
        let path = self.part_path(id, 1);
        fs::create_dir_all(path.parent().unwrap()).await.map_err(disk)?;
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .await
            .map_err(disk)?;
        let mut file = BufWriter::with_capacity(PART_BUFFER_BYTES, file);
        self.changed();
        self.persist().await.map_err(Fault::Fatal)?;
        let (candidate_tx, mut candidate_rx) = tokio::sync::mpsc::channel(1);
        let probe_task = if !self.task(id).map_err(Fault::Fatal)?.force_single_stream
            && total.is_some() && stream_validator.strong_etag().is_some()
            && lock(&self.inner.state).settings.connections >= 2 {
            let engine = self.clone();
            let probe_id = id.to_owned();
            let mut probe_stop = stop.clone();
            Some(tokio::spawn(async move {
                let initial = if std::env::var("QDM_TEST_MODE").ok().as_deref() == Some("1") {
                    std::env::var("QDM_RANGE_RECOVERY_TEST_MS").ok()
                        .and_then(|s| s.parse::<u64>().ok()).filter(|n| (100..=1000).contains(n))
                        .map(Duration::from_millis).unwrap_or(Duration::from_secs(4))
                } else { Duration::from_secs(4) };
                let mut delay = initial;
                loop {
                    tokio::select! { _ = probe_stop.wait() => break, _ = tokio::time::sleep(delay) => {} }
                    match engine.live_range_probe(&probe_id, &mut probe_stop).await {
                        Ok(Some(meta)) => if candidate_tx.send(meta).await.is_err() { break; },
                        Err(Fault::Stopped) => break,
                        _ => {}
                    }
                    delay = (delay * 2).min(Duration::from_secs(60));
                }
            }))
        } else { None };
        let result = async {
            let mut downloaded = 0u64;
            let mut last_checkpoint = Instant::now();
            let mut probing = probe_task.is_some();
            loop {
                let chunk = tokio::select! {
                    candidate = candidate_rx.recv(), if probing => {
                        if let Some(meta) = candidate {
                            if downloaded > 0 && Some(meta.total) == total && downloaded < meta.total
                                && meta.url == stream_url
                                && meta.validator.strong_etag().is_some()
                                && meta.validator.strong_etag() == stream_validator.strong_etag() {
                                self.checkpoint(id, 1, &mut file).await?;
                                let verified = tokio::time::timeout(Duration::from_secs(10),
                                    self.validate_saved(id, &meta, stop)).await;
                                stop.check()?;
                                if matches!(verified, Ok(Ok(()))) {
                                    return Ok(StreamOutcome::Promote(meta));
                                }
                            }
                        } else { probing = false; }
                        continue;
                    }
                    chunk = network(stop, NETWORK_IDLE, response.chunk()) => chunk?,
                };
                let Some(chunk) = chunk else { break; };
                self.record_network(id, chunk.len() as u64);
                self.throttle(id, chunk.len() as u64, stop).await?;
                if downloaded
                    .checked_add(chunk.len() as u64)
                    .filter(|n| {
                        *n <= i64::MAX as u64 && total.map(|total| *n <= total).unwrap_or(true)
                    })
                    .is_none()
                {
                    return Err(Fault::Fatal(
                        "The complete response exceeded its declared file length".into(),
                    ));
                }
                file.write_all(&chunk).await.map_err(disk)?;
                downloaded += chunk.len() as u64;
                {
                    let mut state = lock(&self.inner.state);
                    if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                        task.parts[0].downloaded = downloaded;
                        if total.is_none() {
                            task.parts[0].end = downloaded;
                        }
                        let lane = &mut task.lane_activity[0];
                        lane.transferred_bytes = downloaded;
                        lane.current_bytes = downloaded;
                        lane.current_total = total.unwrap_or(downloaded);
                    }
                }
                self.changed();
                if last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                    self.checkpoint(id, 1, &mut file).await?;
                    last_checkpoint = Instant::now();
                }
                stop.check()?;
            }
            if total.map(|total| total != downloaded).unwrap_or(false) {
                return Err(Fault::Retry(
                    "The complete response ended before its declared final byte".into(),
                    Duration::ZERO,
                ));
            }
            {
                let mut state = lock(&self.inner.state);
                if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                    task.view.total_bytes = Some(downloaded);
                    task.parts[0].end = downloaded;
                }
            }
            Ok(StreamOutcome::Complete)
        }
        .await;
        if let Some(handle) = probe_task { handle.abort(); let _ = handle.await; }
        self.checkpoint(id, 1, &mut file).await?;
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                task.parts[0].active = false;
                task.lane_activity[0].active = false;
                task.refresh();
            }
        }
        self.changed();
        result
    }

    // Unlike the initial probe, never issue a fallback GET or consume a 200 body.
    async fn live_range_probe(&self, id: &str, stop: &mut Stop) -> Result<Option<Meta>, Fault> {
        let task = self.task(id).map_err(Fault::Fatal)?;
        let headers = request_headers(&task.headers).map_err(Fault::Fatal)?;
        let mut response = network(stop, HEADER_TIMEOUT, self.inner.client.get(&task.view.url)
            .headers(headers).header(RANGE, "bytes=0-0").send()).await?;
        if response.status().as_u16() != 206 || encoded(response.headers()) { return Ok(None); }
        let range = header_string(response.headers(), "content-range").as_deref().and_then(content_range);
        let Some(range) = range.filter(|r| r.start == 0 && r.end == 0 && r.total <= i64::MAX as u64) else { return Ok(None); };
        if content_length(response.headers()).map_err(Fault::Restart)?.map(|n| n != 1).unwrap_or(false) { return Ok(None); }
        let meta = Meta { total: range.total, validator: Validator::from_headers(response.headers()),
            url: response.url().as_str().into(), filename: None };
        let mut received = 0usize;
        while let Some(chunk) = network(stop, NETWORK_IDLE, response.chunk()).await? {
            received += chunk.len();
            if received > 1 { return Ok(None); }
        }
        Ok((received == 1).then_some(meta))
    }

    async fn run_stream_then_maybe_ranges(&self, id: &str, initial: Option<Response>,
        stop: &mut Stop, task_stop: watch::Receiver<bool>) -> Result<(), Fault> {
        let mut initial = initial;
        loop {
            match self.run_stream(id, initial.take(), stop).await? {
                StreamOutcome::Complete => return Ok(()),
                StreamOutcome::Promote(meta) => {
                    {
                        let mut state = lock(&self.inner.state);
                        let configured = state.settings.connections;
                        let task = state.tasks.iter_mut().find(|t| t.view.id == id)
                            .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
                        let part = &mut task.parts[0];
                        part.active = false; part.owner_lane = None; part.reserved = 0;
                        part.response_end = 0; part.request_started_at = None;
                        part.response_started_at = None; part.response_payload_bytes = 0;
                        task.lane_activity = vec![LaneRecord::default(); 64];
                        task.stream_mode = false; task.view.supports_ranges = true;
                        task.view.resumable = true; task.view.total_bytes = Some(meta.total);
                        task.validator = meta.validator.clone(); task.resource_url = Some(meta.url.clone());
                        task.view.integrity = "etag_and_overlap".into();
                        task.view.resumed_bytes = part.downloaded;
                        task.adaptive_limit = configured.min(INITIAL_CONNECTIONS);
                        task.adaptive_starting = configured > task.adaptive_limit;
                        task.adaptive_last_change = Some(Instant::now());
                        task.startup_ready_slots = 0; task.startup_next_growth = None;
                        task.refresh();
                    }
                    self.changed();
                    self.persist().await.map_err(Fault::Fatal)?;
                    match self.run_ranges(id, &meta, task_stop.clone()).await {
                        Ok(()) => return Ok(()),
                        Err(Fault::Restart(reason)) => self.clean_parts(id, Some(&reason)).await?,
                        Err(error) => return Err(error),
                    }
                }
            }
        }
    }

    async fn verify_boundaries(
        &self,
        id: &str,
        parts: &[Part],
        stop: &mut Stop,
    ) -> Result<(), Fault> {
        for part in parts.iter().filter(|part| part.overlap_len > 0) {
            stop.check()?;
            let Some(expected) = &part.overlap_sha256 else {
                return Err(Fault::Restart(
                    "A partial-file boundary has no continuity witness; restarting safely.".into(),
                ));
            };
            let mut position = part.overlap_start;
            let end = position + part.overlap_len;
            let mut hash = Sha256::new();
            for source in parts
                .iter()
                .filter(|p| p.start < end && p.end > part.overlap_start)
            {
                if source.start > position {
                    return Err(Fault::Fatal("Partial-file ownership contains a gap".into()));
                }
                let length = (source.end.min(end) - position) as usize;
                let mut file = fs::File::open(self.part_path(id, source.id))
                    .await
                    .map_err(disk)?;
                file.seek(SeekFrom::Start(position))
                    .await
                    .map_err(disk)?;
                let mut bytes = vec![0; length];
                file.read_exact(&mut bytes).await.map_err(disk)?;
                hash.update(&bytes);
                position += length as u64;
                if position == end {
                    break;
                }
            }
            if position != end || format!("{:x}", hash.finalize()) != *expected {
                return Err(Fault::Restart("Adjacent connections returned different file data. QDM is restarting one complete stream to avoid a mixed file.".into()));
            }
        }
        Ok(())
    }

    async fn finalize(&self, id: &str, stop: &mut Stop) -> Result<(), Fault> {
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

    fn claim(&self, id: &str, slot: u8) -> Result<Claim, Fault> {
        let mut state = lock(&self.inner.state);
        let user_limit = state.settings.connections as usize;
        let origin_budget = origin_worker_budget(&state, id);
        let task = state
            .tasks
            .iter_mut()
            .find(|t| t.view.id == id)
            .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
        if task.parts.iter().all(|p| p.downloaded >= p.len()) {
            return Ok(Claim::Done);
        }
        let now = Instant::now();
        // An origin's Retry-After governs NEW requests. Already accepted
        // responses keep streaming and checkpointing during this interval.
        if let Some(when) = task.request_not_before.filter(|when| *when > now) {
            return Ok(Claim::WaitUntil(when));
        }
        if task.adaptive_starting {
            if task.startup_next_growth.map(|when| when <= now).unwrap_or(false) {
                task.adaptive_limit = task.adaptive_limit.saturating_mul(2)
                    .min(user_limit as u8);
                task.adaptive_last_change = Some(now);
                task.startup_ready_slots = 0;
                task.startup_next_growth = None;
                if task.adaptive_limit as usize >= user_limit {
                    task.adaptive_starting = false;
                }
                self.inner.work_changed.notify_waiters();
            }
        }
        let limit = if task.adaptive_limit == 0 { user_limit } else { user_limit.min(task.adaptive_limit as usize) }
            .min(origin_budget);
        // A worker owns a fixed slot; dynamically split pieces are not new connections.
        if slot as usize >= limit || task.parts.iter().filter(|p| p.active).count() >= limit {
            return Ok(task.startup_next_growth.filter(|when| *when > now)
                .map(Claim::WaitUntil).unwrap_or(Claim::Wait));
        }
        if let Some(index) = task
            .parts
            .iter()
            .position(|p| !p.active && p.downloaded < p.len())
        {
            task.ensure_lane_activity();
            task.parts[index].active = true;
            task.parts[index].owner_lane = Some(slot);
            let part = task.parts[index].clone();
            let lane = &mut task.lane_activity[slot as usize];
            lane.current_part_id = Some(part.id);
            lane.current_bytes = part.downloaded;
            lane.current_total = part.len();
            lane.active = true;
            task.refresh();
            return Ok(Claim::Part(part));
        }
        let mut next_split_check = None;
        if task.parts.len() < PART_LIMIT {
            // Include a currently outstanding request's age: its unfinished
            // setup already demonstrates that another request may be costly.
            let setup = task.parts.iter()
                .filter(|part| part.active && part.response_payload_bytes == 0)
                .filter_map(|part| part.request_started_at)
                .map(|started| now.saturating_duration_since(started))
                .fold(Duration::from_micros(task.request_setup_micros.max(1000)), Duration::max);
            // The reservation is the disk write currently in flight. Splitting may
            // only take bytes after it, so ownership never overlaps a writer.
            let candidate = task
                .parts
                .iter()
                .enumerate()
                .filter(|(_, part)| {
                    let Some(when) = split_ready_at(part, now, setup) else { return false; };
                    if when <= now { return true; }
                    next_split_check = Some(next_split_check.map(|old: Instant| old.min(when)).unwrap_or(when));
                    false
                })
                .max_by_key(|(_, p)| p.remaining())
                .map(|(index, _)| index);
            if let Some(index) = candidate {
                let original = &mut task.parts[index];
                let cursor = original.start + original.downloaded + original.reserved;
                let start = cursor + (original.end - cursor) / 2;
                let end = original.end;
                let parent_id = original.id;
                trace_transfer("split", || serde_json::json!({
                    "task": id, "parent": parent_id, "parentDownloaded": original.downloaded,
                    "parentReserved": original.reserved, "parentResponseEnd": original.response_end,
                    "parentResponsePayload": original.response_payload_bytes,
                    "parentResponseAgeMs": original.response_started_at.map(|at| now.saturating_duration_since(at).as_millis()),
                    "setupCostMs": setup.as_millis(), "childStart": start, "childEnd": end, "slot": slot,
                }));
                original.end = start;
                let id = task.next_part_id;
                task.next_part_id += 1;
                let part = Part {
                    id,
                    start,
                    end,
                    active: true,
                    owner_lane: Some(slot),
                    split_parent: Some(parent_id),
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
                task.refresh();
                self.changed();
                return Ok(Claim::Part(part));
            }
        }
        let next_check = match (task.startup_next_growth.filter(|when| *when > now), next_split_check) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Ok(next_check
            .map(Claim::WaitUntil).unwrap_or(Claim::Wait))
    }

    async fn run_ranges(
        &self,
        id: &str,
        meta: &Meta,
        task_stop: watch::Receiver<bool>,
    ) -> Result<(), Fault> {
        let (group_stop, group) = watch::channel(false);
        let mut workers = JoinSet::new();
        // Idle tasks wait on Notify and hold neither a file nor a socket. This
        // allows increasing the limit live without a polling loop or restart.
        for slot in 0..64u8 {
            let engine = self.clone();
            let id = id.to_string();
            let meta = meta.clone();
            let stop = Stop {
                task: task_stop.clone(),
                group: group.clone(),
            };
            workers.spawn(async move { engine.worker(&id, slot, &meta, stop).await });
        }
        let mut failure = None;
        while let Some(result) = workers.join_next().await {
            let result = match result {
                Ok(result) => result,
                Err(error) => Err(Fault::Fatal(format!("A transfer worker stopped: {error}"))),
            };
            if let Err(error) = result {
                if failure.is_none() {
                    failure = Some(error);
                    let _ = group_stop.send(true);
                    self.inner.work_changed.notify_waiters();
                    self.inner.throttle_changed.notify_waiters();
                }
            }
        }
        if let Some(error) = failure {
            Err(error)
        } else {
            Ok(())
        }
    }

    async fn worker(&self, id: &str, slot: u8, meta: &Meta, mut stop: Stop) -> Result<(), Fault> {
        loop {
            stop.check()?;
            let notified = self.inner.work_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            match self.claim(id, slot)? {
                Claim::Done => return Ok(()),
                Claim::Wait => {
                    tokio::select! {biased;_=stop.wait()=>return Err(Fault::Stopped),_=notified.as_mut()=>{}}
                }
                Claim::WaitUntil(when) => {
                    tokio::select! {biased;
                        _=stop.wait()=>return Err(Fault::Stopped),
                        _=notified.as_mut()=>{},
                        _=tokio::time::sleep_until(tokio::time::Instant::from_std(when))=>{},
                    }
                }
                Claim::Part(part) => {
                    trace_transfer("claim", || serde_json::json!({
                        "task": id, "part": part.id, "slot": slot, "start": part.start,
                        "end": part.end, "downloaded": part.downloaded, "splitParent": part.split_parent,
                    }));
                    let attempt_started = Instant::now();
                    let result = self.download_piece(id, part.id, meta, &mut stop).await;
                    trace_transfer("attempt-finished", || serde_json::json!({
                        "task": id, "part": part.id, "slot": slot,
                        "elapsedMs": attempt_started.elapsed().as_millis(),
                        "result": match &result { Ok(()) => "completed", Err(_) => "interrupted" },
                        "fault": match &result {
                            Err(Fault::Stopped) => Some("stopped"),
                            Err(Fault::Transport(_, _)) => Some("transport"),
                            Err(Fault::Retry(_, _)) => Some("retry"),
                            Err(Fault::Restart(_)) => Some("restart"),
                            Err(Fault::Fatal(_)) => Some("fatal"),
                            Ok(()) => None,
                        },
                    }));
                    // Install the shared request gate before releasing this
                    // piece, so another worker cannot immediately retry it.
                    let result = match result {
                        Err(Fault::Retry(message, delay)) if message.contains("HTTP 429") =>
                            self.backoff_origin(id, part.id, true, &message, delay),
                        Err(Fault::Retry(message, _)) if message == "Range request refused with HTTP 200" =>
                            self.backoff_range_refusal(id, part.id),
                        Err(Fault::Transport(message, delay)) =>
                            self.backoff_origin(id, part.id, false, &message, delay),
                        result => result,
                    };
                    {
                        let mut state = lock(&self.inner.state);
                        if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                            if let Some(piece) = task.parts.iter_mut().find(|p| p.id == part.id) {
                                piece.active = false;
                                piece.reserved = 0;
                                if let Some(lane) = task.lane_activity.get_mut(slot as usize) {
                                    lane.current_bytes = piece.downloaded;
                                    lane.current_total = piece.len();
                                    lane.active = false;
                                }
                            } else if let Some(lane) = task.lane_activity.get_mut(slot as usize) {
                                // An untouched rejected child returned to its
                                // still-live parent; it no longer owns a range.
                                lane.active = false;
                                lane.current_part_id = None;
                                lane.current_bytes = 0;
                                lane.current_total = 0;
                            }
                            task.refresh();
                        }
                    }
                    self.changed();
                    self.inner.work_changed.notify_waiters();
                    result?;
                }
            }
        }
    }

    fn backoff_range_refusal(&self, id: &str, part_id: u64) -> Result<(), Fault> {
        let mut state = lock(&self.inner.state);
        let configured = state.settings.connections.max(1);
        let task = state.tasks.iter_mut().find(|task| task.view.id == id)
            .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
        // HTTP 200 to a validated byte-range request means the requested
        // offsets cannot be trusted. Switch to a fresh complete GET rather
        // than retrying an impossible range for minutes. Do not reuse parts.
        if !task.force_single_stream {
            task.force_single_stream = true;
            return Err(Fault::Restart(
                "The server ignored a byte-range request; restarting safely with one complete connection.".into(),
            ));
        }
        let now = Instant::now();
        let cooling = task.request_not_before.map(|until| until > now).unwrap_or(false);
        if !cooling {
            task.transport_failure_streak = task.transport_failure_streak.saturating_add(1);
        }
        let base = if std::env::var("QDM_TEST_MODE").ok().as_deref() == Some("1") {
            std::env::var("QDM_RANGE_RECOVERY_TEST_MS").ok()
                .and_then(|value| value.parse::<u64>().ok()).filter(|value| *value > 0)
                .map(Duration::from_millis).unwrap_or(Duration::from_secs(5))
        } else { Duration::from_secs(5) };
        let delay = base.saturating_mul(1u32 << task.transport_failure_streak.saturating_sub(1).min(4))
            .min(if base < Duration::from_secs(5) { base.saturating_mul(12) } else { Duration::from_secs(60) });
        let until = now + delay;
        task.request_not_before = Some(task.request_not_before.map(|old| old.max(until)).unwrap_or(until));
        // Retain one speculative slot when the user permits parallelism, so a
        // slow accepted response cannot prevent recovery from being tested.
        task.adaptive_limit = task.adaptive_limit.max(1).min(configured).min(2).min(configured);
        task.adaptive_starting = false;
        task.startup_next_growth = None;
        task.adaptive_last_change = Some(now);
        task.adaptive_healthy_bytes = 0;
        restore_untouched_split(&mut task.parts, part_id);
        task.view.notice = Some(format!("Server temporarily refused a byte range; accepted transfers continue, retrying in {:.1} seconds.", delay.as_secs_f64()));
        drop(state);
        self.changed();
        self.inner.work_changed.notify_waiters();
        Ok(())
    }

    fn note_healthy_body(&self, id: &str, slot: Option<u8>, setup: Duration) {
        let Some(slot) = slot else { return; };
        let mut state = lock(&self.inner.state);
        let configured = state.settings.connections;
        if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == id) {
            let sample = setup.as_micros().min(u64::MAX as u128) as u64;
            task.request_setup_micros = if task.request_setup_micros == 0 { sample.max(1000) }
                else { task.request_setup_micros.saturating_mul(3).saturating_add(sample.max(1000)) / 4 };
            task.startup_ready_slots |= 1u64 << slot;
            if !task.adaptive_starting || task.adaptive_limit >= configured { return; }
            // One productive stream can trigger a controlled probe. Do not
            // wait forever for every slow or idle startup worker to send data.
            if task.startup_next_growth.is_none() {
                let now = Instant::now();
                let elapsed = task.adaptive_last_change
                    .map(|at| now.saturating_duration_since(at))
                    .unwrap_or(STARTUP_INTERVAL);
                let wait = Duration::from_millis(750).saturating_sub(elapsed)
                    .max(STARTUP_INTERVAL);
                task.startup_next_growth = Some(now + wait);
                self.inner.work_changed.notify_waiters();
            }
        }
    }

    fn backoff_origin(&self, id: &str, part_id: u64, rate_limited: bool,
        reason: &str, server_delay: Duration) -> Result<(), Fault> {
        let mut state = lock(&self.inner.state);
        let configured = state.settings.connections;
        let retries = state.settings.retries;
        let task = state.tasks.iter_mut().find(|task| task.view.id == id)
            .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
        let now = Instant::now();
        let cooling = task.request_not_before.map(|until| until > now).unwrap_or(false);
        if !rate_limited {
            let part = task.parts.iter_mut().find(|part| part.id == part_id)
                .ok_or_else(|| Fault::Fatal("A partial file no longer exists".into()))?;
            if !part.record_transport_failure(retries) {
                return Err(Fault::Fatal(format!("{reason}. Retry budget exhausted; saved bytes can be resumed.")));
            }
            task.transport_failure_streak = task.transport_failure_streak.saturating_add(1);
        }
        let reduce = (rate_limited || task.transport_failure_streak >= 2)
            && (!cooling || !task.backoff_reduced);
        if rate_limited && (!cooling || !task.backoff_reduced) {
            task.rate_limit_rounds = task.rate_limit_rounds.saturating_add(1);
            if task.rate_limit_rounds > 6 {
                // Never kill already productive streams just because a CDN
                // refuses extra speculative sockets. Keep backing off instead.
                if task.parts.iter().any(|p| p.active && p.response_payload_bytes > 0) {
                    task.rate_limit_rounds = 6;
                } else {
                    return Err(Fault::Fatal(format!("{reason}. The origin kept rejecting new requests after six backoff rounds; saved bytes can be resumed.")));
                }
            }
        }
        if reduce {
            let previous = if task.adaptive_limit == 0 { configured } else { task.adaptive_limit };
            task.adaptive_limit = (previous / 2).max(1);
            task.adaptive_starting = false;
            task.startup_next_growth = None;
            task.adaptive_last_change = Some(now);
            task.adaptive_healthy_bytes = 0;
        }
        if !cooling { task.backoff_reduced = false; }
        task.backoff_reduced |= reduce;
        let local_delay = if rate_limited {
            Duration::from_millis(500u64 * (1u64 << task.rate_limit_rounds.saturating_sub(1).min(5)))
        } else if reduce { Duration::from_secs(1) } else { Duration::from_millis(300) };
        let delay = server_delay.max(local_delay).min(Duration::from_secs(120));
        let until = now + delay;
        task.request_not_before = Some(task.request_not_before.map(|old| old.max(until)).unwrap_or(until));
        let limit = task.adaptive_limit.max(1);
        trace_transfer("origin-backoff", || serde_json::json!({
            "task": id, "part": part_id, "rateLimited": rate_limited,
            "delayMs": delay.as_millis(), "newLimit": limit,
        }));
        task.view.notice = Some(if rate_limited {
            format!("Server HTTP 429: reducing connections to {limit}; accepted streams continue. New requests wait {:.1} seconds.", delay.as_secs_f64())
        } else {
            format!("{reason}. Keeping accepted streams; new requests retry in {:.1} seconds with up to {limit} connections.", delay.as_secs_f64())
        });
        if rate_limited {
            // No new HTTP request is needed for these bytes: the original
            // parent's already validated response still covers them. Any data
            // accepted by the child, or a closed/finished parent, forbids this.
            restore_untouched_split(&mut task.parts, part_id);
            // Learn the origin-wide successful concurrency only after a real
            // 429. Expire the cap automatically so CDN behavior can recover.
            let origin = state.tasks.iter().find(|t| t.view.id == id)
                .and_then(|t| origin_key(t.resource_url.as_deref().unwrap_or(&t.view.url)));
            if let Some(origin) = origin {
                let receiving = state.tasks.iter().filter(|t| {
                    state.active.contains_key(&t.view.id)
                        && origin_key(t.resource_url.as_deref().unwrap_or(&t.view.url)).as_deref() == Some(origin.as_str())
                }).flat_map(|t| t.parts.iter())
                    .filter(|p| p.active && p.response_payload_bytes > 0).count().max(1);
                let cap = state.host_caps.entry(origin).or_insert(HostCap {
                    connections: receiving, learned_at: now,
                });
                cap.connections = cap.connections.min(receiving);
                cap.learned_at = now;
            }
        }
        drop(state);
        self.changed();
        self.inner.work_changed.notify_waiters();
        Ok(())
    }

    async fn wait_for_origin(&self, id: &str, stop: &mut Stop) -> Result<(), Fault> {
        loop {
            stop.check()?;
            let notified = self.inner.work_changed.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            let when = {
                let state = lock(&self.inner.state);
                state.tasks.iter().find(|task| task.view.id == id)
                    .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?
                    .request_not_before.filter(|when| *when > Instant::now())
            };
            let Some(when) = when else { return Ok(()); };
            tokio::select! {biased;
                _=stop.wait()=>return Err(Fault::Stopped),
                _=notified.as_mut()=>{},
                _=tokio::time::sleep_until(tokio::time::Instant::from_std(when))=>{},
            }
        }
    }

    fn current_part(&self, id: &str, part_id: u64) -> Result<Part, Fault> {
        let state = lock(&self.inner.state);
        state
            .tasks
            .iter()
            .find(|t| t.view.id == id)
            .and_then(|t| t.parts.iter().find(|p| p.id == part_id))
            .cloned()
            .ok_or_else(|| Fault::Fatal("A partial file no longer exists".into()))
    }

    async fn checkpoint(&self, id: &str, part_id: u64, file: &mut PartWriter) -> Result<(), Fault> {
        let part = self.current_part(id, part_id)?;
        let started = Instant::now();
        trace_transfer("checkpoint-start", || serde_json::json!({"task": id, "part": part_id, "bytes": part.downloaded}));
        // Flush the userspace buffer AND Tokio's pending OS write before a
        // checkpoint can advertise these bytes as durable. Never truncate the
        // inner file while the buffer still contains accepted data.
        file.flush().await.map_err(disk)?;
        let file = file.get_mut();
        // Never truncate the shared file: other workers own distant offsets.
        // OS synchronization precedes the persisted SQLite checkpoint.
        file.sync_data().await.map_err(disk)?;
        let length = part.downloaded.min(OVERLAP);
        let mut tail = vec![0; length as usize];
        file.seek(SeekFrom::Start(part.start + part.downloaded - length))
            .await
            .map_err(disk)?;
        if length > 0 {
            file.read_exact(&mut tail).await.map_err(disk)?;
        }
        file.seek(SeekFrom::Start(part.start + part.downloaded))
            .await
            .map_err(disk)?;
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                if let Some(current) = task.parts.iter_mut().find(|p| p.id == part_id) {
                    current.durable = part.downloaded;
                    current.durable_tail_sha256 = Some(digest(&tail));
                }
            }
        }
        self.changed();
        trace_transfer("checkpoint-end", || serde_json::json!({"task": id, "part": part_id, "bytes": part.downloaded, "elapsedMs": started.elapsed().as_millis()}));
        Ok(())
    }

    async fn download_piece(
        &self,
        id: &str,
        part_id: u64,
        meta: &Meta,
        stop: &mut Stop,
    ) -> Result<(), Fault> {
        let part = self.current_part(id, part_id)?;
        let mut file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(self.part_path(id, part_id))
            .await
            .map_err(disk)?;
        file.seek(SeekFrom::Start(part.start + part.downloaded))
            .await
            .map_err(disk)?;
        let mut file = BufWriter::with_capacity(PART_BUFFER_BYTES, file);
        let retries = lock(&self.inner.state).settings.retries;
        let mut result = Ok(());
        for attempt in 0..=retries {
            result = self.piece_attempt(id, part_id, meta, &mut file, stop).await;
            let checkpoint = self.checkpoint(id, part_id, &mut file).await;
            if let Err(error) = checkpoint {
                return Err(error);
            }
            match result {
                Err(Fault::Retry(ref message, _)) if message.contains("HTTP 429") || message == "Range request refused with HTTP 200" => break,
                Err(Fault::Transport(_, _)) => break,
                Err(Fault::Retry(ref message, delay)) if attempt < retries => {
                    self.delay_retry(id, attempt, message, delay, stop).await?
                }
                _ => break,
            }
        }
        result
    }

    async fn piece_attempt(
        &self,
        id: &str,
        part_id: u64,
        meta: &Meta,
        file: &mut PartWriter,
        stop: &mut Stop,
    ) -> Result<(), Fault> {
        stop.check()?;
        self.wait_for_origin(id, stop).await?;
        let part = self.current_part(id, part_id)?;
        if part.downloaded >= part.len() {
            return Ok(());
        }
        let cursor = part.start + part.downloaded;
        let wire_start = if part.downloaded > 0 {
            cursor.saturating_sub(OVERLAP).max(part.start)
        } else {
            part.start.saturating_sub(OVERLAP)
        };
        let wire_end = part.end - 1;
        let prefix_len = (cursor - wire_start) as usize;
        let mut expected = vec![0; prefix_len];
        if part.downloaded > 0 && prefix_len > 0 {
            file.seek(SeekFrom::Start(wire_start))
                .await
                .map_err(disk)?;
            file.read_exact(&mut expected).await.map_err(disk)?;
            file.seek(SeekFrom::Start(part.start + part.downloaded))
                .await
                .map_err(disk)?;
        }
        let task = self.task(id).map_err(Fault::Fatal)?;
        let mut request = self
            .inner
            .client
            .get(&task.view.url)
            .headers(request_headers(&task.headers).map_err(Fault::Fatal)?)
            .header(RANGE, format!("bytes={wire_start}-{wire_end}"));
        if let Some(validator) = meta.validator.if_range() {
            request = request.header(IF_RANGE, validator);
        }
        // A different worker may have received429 while this worker was
        // preparing its overlap bytes. Recheck immediately before dispatch.
        self.wait_for_origin(id, stop).await?;
        let request_started = Instant::now();
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == id) {
                if let Some(current) = task.parts.iter_mut().find(|part| part.id == part_id) {
                    current.request_started_at = Some(request_started);
                    current.response_started_at = None;
                    current.response_payload_bytes = 0;
                }
            }
        }
        // Cover header failures as well as successful bodies so no stale
        // request age can remain after a retry, cancellation or panic.
        let _coverage = ResponseCoverage { engine: self.clone(), task_id: id.to_string(), part_id };
        trace_transfer("request-send", || serde_json::json!({
            "task": id, "part": part_id, "start": wire_start, "end": wire_end,
            "savedBytes": part.downloaded, "slot": part.owner_lane,
        }));
        let mut response = network(stop, HEADER_TIMEOUT, request.send()).await?;
        trace_headers(id, Some(part_id), &response, request_started);
        validate_range(&response, wire_start, wire_end, meta)?;
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == id) {
                if let Some(current) = task.parts.iter_mut().find(|part| part.id == part_id) {
                    current.response_end = wire_end + 1;
                    current.response_started_at = Some(Instant::now());
                }
            }
        }
        let mut prefix = Vec::with_capacity(prefix_len);
        let mut last_checkpoint = Instant::now();
        let mut first_body = true;
        while let Some(chunk) = network(stop, NETWORK_IDLE, response.chunk()).await? {
            if first_body && !chunk.is_empty() {
                first_body = false;
                self.note_healthy_body(id, part.owner_lane, request_started.elapsed());
                trace_transfer("first-body", || serde_json::json!({
                    "task": id, "part": part_id, "elapsedMs": request_started.elapsed().as_millis(), "bytes": chunk.len(),
                }));
            }
            self.record_network(id, chunk.len() as u64);
            self.throttle(id, chunk.len() as u64, stop).await?;
            let mut offset = 0;
            if prefix.len() < prefix_len {
                let count = (prefix_len - prefix.len()).min(chunk.len());
                if part.downloaded > 0
                    && chunk[..count] != expected[prefix.len()..prefix.len() + count]
                {
                    return Err(Fault::Restart("A resume overlap did not match saved bytes; restarting one complete stream.".into()));
                }
                prefix.extend_from_slice(&chunk[..count]);
                offset = count;
                if prefix.len() == prefix_len && part.downloaded == 0 && prefix_len > 0 {
                    let mut state = lock(&self.inner.state);
                    if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                        if let Some(part) = task.parts.iter_mut().find(|p| p.id == part_id) {
                            part.overlap_start = wire_start;
                            part.overlap_len = prefix_len as u64;
                            part.overlap_sha256 = Some(digest(&prefix));
                        }
                    }
                }
            }
            if offset < chunk.len() {
                let count = {
                    let mut state = lock(&self.inner.state);
                    let task = state
                        .tasks
                        .iter_mut()
                        .find(|t| t.view.id == id)
                        .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
                    let part = task
                        .parts
                        .iter_mut()
                        .find(|p| p.id == part_id)
                        .ok_or_else(|| Fault::Fatal("A partial file no longer exists".into()))?;
                    let count = (chunk.len() - offset)
                        .min(part.len().saturating_sub(part.downloaded) as usize);
                    part.reserved = count as u64;
                    count
                };
                if count > 0 {
                    // Accepted bytes include this worker's bounded write buffer.
                    // This write must finish before reacting to pause. Ownership
                    // is committed atomically; checkpoints flush and sync it.
                    let written = file.write_all(&chunk[offset..offset + count]).await;
                    let mut first_payload = false;
                    {
                        let mut state = lock(&self.inner.state);
                        if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                            if let Some(part) = task.parts.iter_mut().find(|p| p.id == part_id) {
                                if written.is_ok() {
                                    first_payload = part.response_payload_bytes == 0;
                                    part.response_payload_bytes += count as u64;
                                    part.downloaded += count as u64;
                                    if let Some(slot) = part.owner_lane {
                                        if let Some(lane) = task.lane_activity.get_mut(slot as usize) {
                                            lane.transferred_bytes = lane.transferred_bytes.saturating_add(count as u64);
                                            lane.current_bytes = part.downloaded;
                                            lane.current_total = part.len();
                                        }
                                    }
                                }
                                part.reserved = 0;
                            }
                        }
                    }
                    written.map_err(disk)?;
                    self.changed();
                    if first_payload {
                        self.inner.work_changed.notify_waiters();
                        trace_transfer("first-payload", || serde_json::json!({
                            "task": id, "part": part_id, "elapsedMs": request_started.elapsed().as_millis(), "bytes": count,
                        }));
                    }
                }
            }
            let current = self.current_part(id, part_id)?;
            if current.downloaded >= current.len() {
                trace_transfer("last-owned-payload", || serde_json::json!({
                    "task": id, "part": part_id, "downloaded": current.downloaded,
                    "elapsedMs": request_started.elapsed().as_millis(),
                }));
                return Ok(());
            }
            // Establish early durable recovery witnesses on the initial two
            // range workers without syncing all speculative workers every second.
            let early_durable = part_id <= 2 && current.durable == 0
                && last_checkpoint.elapsed() >= Duration::from_millis(750);
            if early_durable || last_checkpoint.elapsed() >= CHECKPOINT_INTERVAL {
                self.checkpoint(id, part_id, file).await?;
                last_checkpoint = Instant::now();
            }
            stop.check()?;
        }
        let current = self.current_part(id, part_id)?;
        if current.downloaded == current.len() {
            Ok(())
        } else {
            Err(Fault::Retry(
                "The server closed a partial response before its final byte".into(),
                Duration::ZERO,
            ))
        }
    }

    pub(super) async fn run_owned(
        &self,
        id: String,
        task_stop: watch::Receiver<bool>,
        done: watch::Sender<bool>,
    ) {
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == id) {
                for part in &mut task.parts {
                    part.transport_failures = 0;
                    part.last_transport_failure_durable = 0;
                }
                task.rate_limit_rounds = 0;
                task.transport_failure_streak = 0;
                task.startup_ready_slots = 0;
                task.startup_next_growth = None;
                task.request_setup_micros = 0;
            }
        }
        let result = AssertUnwindSafe(self.run_download(&id, task_stop))
            .catch_unwind()
            .await;
        let result = match result {
            Ok(result) => result,
            Err(_) => Err(Fault::Fatal(
                "The download worker stopped unexpectedly; saved bytes can be resumed.".into(),
            )),
        };
        // An I/O error or panic may have dropped a buffer before checkpointing.
        // Resume in this process must be as conservative as process recovery:
        // only durable bytes may survive a failed transfer attempt.
        let failed = result.is_err() && !matches!(&result, Err(Fault::Stopped));
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                for part in &mut task.parts {
                    part.active = false;
                    part.reserved = 0;
                    if failed && task.view.status != "completed" {
                        part.downloaded = part.downloaded.min(part.durable);
                    }
                }
                for lane in &mut task.lane_activity {
                    lane.active = false;
                    if let Some(part_id) = lane.current_part_id {
                        if let Some(part) = task.parts.iter().find(|p| p.id == part_id) {
                            lane.current_bytes = part.downloaded;
                            lane.current_total = part.len();
                        }
                    }
                }
                task.view.speed_bps = 0;
                if task.view.status != "completed" {
                    match result {
                        Ok(()) => {}
                        Err(Fault::Stopped) => {
                            task.view.status = "paused".into();
                            task.view.error = None;
                        }
                        Err(error) => {
                            task.view.status = "error".into();
                            task.view.error = Some(error.message());
                        }
                    }
                }
                task.refresh();
            }
            state.active.remove(&id);
        }
        self.inner.work_changed.notify_waiters();
        self.changed();
        if let Err(error) = self.persist().await {
            lock(&self.inner.state).warning = Some(format!("Could not save progress: {error}"));
        }
        self.emit();
        let _ = done.send(true);
        self.kick();
    }

    async fn delay_retry(
        &self,
        id: &str,
        attempt: u8,
        reason: &str,
        server_delay: Duration,
        stop: &mut Stop,
    ) -> Result<(), Fault> {
        let delay = server_delay
            .max(Duration::from_millis(
                300u64.saturating_mul(1u64 << attempt.min(6)),
            ))
            .min(Duration::from_secs(120));
        {
            let mut state = lock(&self.inner.state);
            if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                task.view.notice = Some(format!(
                    "{reason}. Retrying in {:.1} seconds…",
                    delay.as_secs_f64()
                ));
            }
        }
        self.changed();
        stop.check()?;
        tokio::select! {biased;_=stop.wait()=>Err(Fault::Stopped),_=tokio::time::sleep(delay)=>Ok(())}
    }

    async fn probe(&self, id: &str, stop: &mut Stop) -> Result<Probe, Fault> {
        let retries = lock(&self.inner.state).settings.retries;
        for attempt in 0..=retries {
            let result = self.probe_once(id, stop).await;
            match result {
                Err(Fault::Retry(message, delay) | Fault::Transport(message, delay)) if attempt < retries => {
                    self.delay_retry(id, attempt, &message, delay, stop).await?
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn probe_once(&self, id: &str, stop: &mut Stop) -> Result<Probe, Fault> {
        self.wait_for_origin(id, stop).await?;
        let task = self.task(id).map_err(Fault::Fatal)?;
        let headers = request_headers(&task.headers).map_err(Fault::Fatal)?;
        let started = Instant::now();
        trace_transfer("probe-send", || serde_json::json!({"task": id}));
        let mut response = network(
            stop,
            HEADER_TIMEOUT,
            self.inner
                .client
                .get(&task.view.url)
                .headers(headers)
                .header(RANGE, "bytes=0-0")
                .send(),
        )
        .await?;
        trace_headers(id, None, &response, started);
        if response.status().as_u16() == 416
            && header_string(response.headers(), "content-range").as_deref() == Some("bytes */0")
        {
            drop(response);
            return self.full_request(id, stop).await.map(Probe::Full);
        }
        response_status(&response)?;
        if response.status().as_u16() == 200 {
            if encoded(response.headers()) {
                drop(response);
                return self.full_request(id, stop).await.map(Probe::Full);
            }
            return Ok(Probe::Full(response));
        }
        if response.status().as_u16() != 206 || encoded(response.headers()) {
            drop(response);
            return self.full_request(id, stop).await.map(Probe::Full);
        }
        let range = header_string(response.headers(), "content-range")
            .as_deref()
            .and_then(content_range);
        let Some(range) =
            range.filter(|r| r.start == 0 && r.end == 0 && r.total <= i64::MAX as u64)
        else {
            drop(response);
            return self.full_request(id, stop).await.map(Probe::Full);
        };
        if content_length(response.headers())
            .map_err(Fault::Restart)?
            .map(|len| len != 1)
            .unwrap_or(false)
        {
            drop(response);
            return self.full_request(id, stop).await.map(Probe::Full);
        }
        let meta = Meta {
            total: range.total,
            validator: Validator::from_headers(response.headers()),
            url: response.url().as_str().into(),
            filename: header_string(response.headers(), "content-disposition")
                .as_deref()
                .and_then(disposition_filename),
        };
        let mut received = 0;
        while let Some(chunk) = network(stop, NETWORK_IDLE, response.chunk()).await? {
            self.record_network(id, chunk.len() as u64);
            received += chunk.len();
            if received > 1 {
                return Err(Fault::Restart(
                    "The server sent an invalid range probe".into(),
                ));
            }
        }
        if received != 1 {
            return Err(Fault::Retry(
                "The range probe was truncated".into(),
                Duration::ZERO,
            ));
        }
        trace_transfer("probe-complete", || serde_json::json!({"task": id, "elapsedMs": started.elapsed().as_millis()}));
        Ok(Probe::Range(meta))
    }

    async fn full_request(&self, id: &str, stop: &mut Stop) -> Result<Response, Fault> {
        self.wait_for_origin(id, stop).await?;
        let task = self.task(id).map_err(Fault::Fatal)?;
        let mut headers = request_headers(&task.headers).map_err(Fault::Fatal)?;
        headers.remove(ACCEPT_ENCODING);
        let response = network(
            stop,
            HEADER_TIMEOUT,
            self.inner
                .stream_client
                .get(&task.view.url)
                .headers(headers)
                .send(),
        )
        .await?;
        response_status(&response)?;
        if response.status().as_u16() != 200 {
            return Err(Fault::Fatal(
                "The server did not return a complete file for an ordinary GET".into(),
            ));
        }
        if encoded(response.headers()) {
            return Err(Fault::Fatal(
                "The server used an unsupported Content-Encoding".into(),
            ));
        }
        Ok(response)
    }

    fn record_network(&self, id: &str, bytes: u64) {
        let mut state = lock(&self.inner.state);
        let configured = state.settings.connections;
        if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
            task.view.network_bytes = task.view.network_bytes.saturating_add(bytes);
            // Continued healthy traffic is evidence even when a speculative
            // startup worker has not delivered its first byte. Periodically
            // retry unused slots without requiring all initial slots to report.
            if bytes > 0 && task.view.status == "downloading"
                && task.adaptive_starting && task.adaptive_limit < configured
                && task.startup_next_growth.is_none()
                && task.adaptive_last_change
                    .map(|at| at.elapsed() >= Duration::from_millis(750))
                    .unwrap_or(true) {
                task.startup_next_growth = Some(Instant::now() + STARTUP_INTERVAL);
                self.inner.work_changed.notify_waiters();
            }
            if task.view.status == "downloading" && !task.adaptive_starting && task.adaptive_limit > 0
                && task.adaptive_limit < configured {
                task.adaptive_healthy_bytes = task.adaptive_healthy_bytes.saturating_add(bytes);
                let elapsed = task.adaptive_last_change
                    .get_or_insert_with(Instant::now).elapsed();
                // Additive increase after sustained good traffic. A renewed
                // HTTP 429 halves the limit immediately (multiplicative decrease).
                if elapsed >= Duration::from_secs(6)
                    && task.adaptive_healthy_bytes >= 1024 * 1024 {
                    let step = (task.adaptive_limit / 4).max(1);
                    task.adaptive_limit = task.adaptive_limit.saturating_add(step).min(configured);
                    task.adaptive_last_change = Some(Instant::now());
                    task.adaptive_healthy_bytes = 0;
                    task.transport_failure_streak = 0;
                    task.rate_limit_rounds = 0;
                    task.view.notice = Some(format!(
                        "Stable transfer: testing {} connections (up to {})",
                        task.adaptive_limit, configured));
                    self.inner.work_changed.notify_waiters();
                }
            }
        }
    }

    // Choose the strictest nonzero budget; all applicable buckets are enforced below.
    fn effective_speed_limit(global_kib: u64, task_kib: u64, queue_kib: u64) -> u64 {
        [global_kib, task_kib, queue_kib].into_iter().filter(|limit| *limit > 0).min().unwrap_or(0)
    }
    async fn throttle(&self, id: &str, bytes: u64, stop: &mut Stop) -> Result<(), Fault> {
        loop {
            stop.check()?;
            let changed = self.inner.throttle_changed.notified();
            tokio::pin!(changed);
            changed.as_mut().enable();
            let wait = {
                let state = lock(&self.inner.state);
                let task = state.tasks.iter().find(|t| t.view.id == id);
                let queue = task.map(|t| t.view.queue.clone()).unwrap_or_default();
                let queue_key = if queue.is_empty() { "Default" } else { &queue };
                let global_kib = state.settings.speed_limit_kib;
                let task_kib = task.map_or(0, |t| t.view.speed_limit_kib);
                let queue_kib = state.settings.queue_speed_limits.get(queue_key).copied().unwrap_or(0);
                if Self::effective_speed_limit(global_kib, task_kib, queue_kib) == 0 {
                    return stop.check();
                }
                let mut throttle = lock(&self.inner.throttle);
                let now = Instant::now();
                let global_wait = if global_kib > 0 {
                    throttle.global.wait_duration(now, global_kib, bytes)
                } else { Duration::ZERO };
                let task_wait = if task_kib > 0 {
                    throttle.per_task.entry(id.to_owned()).or_default().wait_duration(now, task_kib, bytes)
                } else { Duration::ZERO };
                let queue_wait = if queue_kib > 0 {
                    throttle.per_queue.entry(queue_key.to_owned()).or_default().wait_duration(now, queue_kib, bytes)
                } else { Duration::ZERO };
                let wait = global_wait.max(task_wait).max(queue_wait);
                if wait.is_zero() {
                    if global_kib > 0 { throttle.global.consume(bytes); }
                    if task_kib > 0 { throttle.per_task.get_mut(id).unwrap().consume(bytes); }
                    if queue_kib > 0 { throttle.per_queue.get_mut(queue_key).unwrap().consume(bytes); }
                }
                wait
            };
            if wait.is_zero() { return stop.check(); }
            tokio::select! {biased;
                _ = stop.wait() => return Err(Fault::Stopped),
                _ = changed.as_mut() => {},
                _ = tokio::time::sleep(wait) => {},
            }
        }
    }
    async fn run_download(&self, id: &str, task_stop: watch::Receiver<bool>) -> Result<(), Fault> {
        let (_group_hold, group) = watch::channel(false);
        let mut stop = Stop {
            task: task_stop.clone(),
            group,
        };
        let probe = match self.probe(id, &mut stop).await {
            Err(Fault::Restart(reason)) => {
                self.clean_parts(id, Some(&reason)).await?;
                self.run_stream_then_maybe_ranges(id, None, &mut stop, task_stop.clone()).await?;
                return self.finalize(id, &mut stop).await;
            }
            result => result?,
        };
        match probe {
            Probe::Full(response) => {
                self.clean_parts(id,Some("This server uses one complete stream. Pause is available; resuming starts the file again when byte ranges are unavailable.")).await?;
                self.run_stream_then_maybe_ranges(id, Some(response), &mut stop, task_stop.clone()).await?;
            }
            Probe::Range(meta) => {
                let old = self.task(id).map_err(Fault::Fatal)?;
                let incompatible = old.stream_mode
                    || old
                        .view
                        .total_bytes
                        .map(|n| n != meta.total)
                        .unwrap_or(false)
                    || meta.validator.changed_from(&old.validator);
                if incompatible {
                    self.clean_parts(
                        id,
                        Some("The remote file changed; old partial bytes were safely discarded."),
                    )
                    .await?;
                }
                {
                    let mut state = lock(&self.inner.state);
                    let task = state
                        .tasks
                        .iter_mut()
                        .find(|t| t.view.id == id)
                        .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
                    task.view.total_bytes = Some(meta.total);
                    task.view.supports_ranges = true;
                    task.view.resumable = true;
                    task.stream_mode = false;
                    task.validator = meta.validator.clone();
                    task.resource_url = Some(meta.url.clone());
                    if !task.custom_filename {
                        if let Some(filename) = &meta.filename {
                            task.view.filename = filename.clone();
                        }
                    }
                    task.view.integrity = if meta.validator.strong_etag().is_some() {
                        "etag_and_overlap"
                    } else if meta.validator.if_range().is_some() {
                        "last_modified_and_overlap"
                    } else {
                        "overlap_checked"
                    }
                    .into();
                    if meta.validator.if_range().is_none() {
                        task.view.notice=Some("The server has no strong validator. QDM checks overlapping bytes; an expected SHA-256 adds whole-file verification.".into());
                    }
                }
                let saved = self
                    .task(id)
                    .map_err(Fault::Fatal)?
                    .parts
                    .iter()
                    .map(|p| p.downloaded)
                    .sum::<u64>();
                if saved > 0 {
                    // Resume validates the saved head/tail/boundary witnesses
                    // before starting writers. Expose that real phase so a
                    // large set of saved pieces does not look like a stall.
                    {
                        let mut state = lock(&self.inner.state);
                        if let Some(task) = state.tasks.iter_mut().find(|task| task.view.id == id) {
                            task.view.status = "verifying".into();
                            task.view.speed_bps = 0;
                            task.view.assembly_bytes = 0;
                            task.view.assembly_total_bytes = meta.total;
                        }
                    }
                    self.changed();
                    self.emit();
                    match self.validate_saved(id, &meta, &mut stop).await {
                        Ok(()) => {
                            let mut state = lock(&self.inner.state);
                            if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                                task.view.resumed_bytes = saved;
                                task.view.status = "downloading".into();
                                task.sampled_network = task.view.network_bytes;
                                task.sampled_at = Some(Instant::now());
                            }
                        }
                        Err(Fault::Restart(reason)) => {
                            self.clean_parts(id, Some(&reason)).await?;
                            self.run_stream_then_maybe_ranges(id, None, &mut stop, task_stop.clone()).await?;
                            return self.finalize(id, &mut stop).await;
                        }
                        Err(error) => return Err(error),
                    }
                }
                {
                    let mut state = lock(&self.inner.state);
                    let configured = state.settings.connections;
                    let task = state.tasks.iter_mut().find(|task| task.view.id == id)
                        .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
                    if task.adaptive_limit == 0 {
                        task.adaptive_limit = configured.min(INITIAL_CONNECTIONS);
                        task.adaptive_starting = configured > task.adaptive_limit;
                        task.adaptive_last_change = Some(Instant::now());
                        task.startup_ready_slots = 0;
                        task.startup_next_growth = None;
                    }
                }
                self.ensure_parts(id, meta.total).await?;
                // Workers handle origin pressure locally, after checkpointing
                // only their rejected piece. Healthy responses stay alive.
                match self.run_ranges(id, &meta, task_stop.clone()).await {
                    Ok(()) => {},
                    Err(Fault::Restart(reason)) => {
                        self.clean_parts(id, Some(&reason)).await?;
                        self.run_stream_then_maybe_ranges(id, None, &mut stop, task_stop.clone()).await?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
        match self.finalize(id, &mut stop).await {
            Err(Fault::Restart(reason)) => {
                self.clean_parts(id, Some(&reason)).await?;
                self.run_stream_then_maybe_ranges(id, None, &mut stop, task_stop.clone()).await?;
                self.finalize(id, &mut stop).await
            }
            result => result,
        }
    }

    async fn clean_parts(&self, id: &str, notice: Option<&str>) -> Result<(), Fault> {
        let partial = self.part_path(id, 0);
        match fs::remove_file(&partial).await {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(disk(error)),
        }
        {
            let mut state = lock(&self.inner.state);
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
            task.parts.clear();
            task.lane_activity.clear();
            task.view.lane_history_estimated = false;
            task.next_part_id = 1;
            task.view.downloaded_bytes = 0;
            task.view.resumed_bytes = 0;
            task.view.assembly_bytes = 0;
            task.view.assembly_total_bytes = 0;
            task.view.sha256 = None;
            task.view.file_path = None;
            task.view.status = "downloading".into();
            if let Some(notice) = notice {
                task.view.notice = Some(notice.into());
            }
            task.refresh();
        }
        self.changed();
        self.persist().await.map_err(Fault::Fatal)
    }

    async fn ensure_parts(&self, id: &str, total: u64) -> Result<(), Fault> {
        // Prepare one sparse, absolutely addressed temporary output on the
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
            // Large initial pieces let accepted streams continue productively
            // when an origin rejects an attempted concurrency increase.
            let count = state.settings.connections.min(INITIAL_CONNECTIONS) as u64;
            let count = count.min((total / MIN_SEGMENT).max(1));
            let task = state
                .tasks
                .iter_mut()
                .find(|t| t.view.id == id)
                .ok_or_else(|| Fault::Fatal("Download no longer exists".into()))?;
            if task.parts.is_empty() {
                for index in 0..count {
                    let start = total * index / count;
                    let end = total * (index + 1) / count;
                    let part_id = task.next_part_id;
                    task.next_part_id += 1;
                    task.parts.push(Part {
                        id: part_id,
                        start,
                        end,
                        ..Part::default()
                    });
                }
                task.refresh();
            }
        }
        self.changed();
        self.persist().await.map_err(Fault::Fatal)
    }

    async fn sample_range(
        &self,
        id: &str,
        start: u64,
        end: u64,
        meta: &Meta,
        stop: &mut Stop,
    ) -> Result<Vec<u8>, Fault> {
        let retries = lock(&self.inner.state).settings.retries;
        for attempt in 0..=retries {
            let result = async {
                self.wait_for_origin(id, stop).await?;
                let task = self.task(id).map_err(Fault::Fatal)?;
                let mut request = self
                    .inner
                    .client
                    .get(&task.view.url)
                    .headers(request_headers(&task.headers).map_err(Fault::Fatal)?)
                    .header(RANGE, format!("bytes={start}-{end}"));
                if let Some(validator) = meta.validator.if_range() {
                    request = request.header(IF_RANGE, validator);
                }
                let mut response = network(stop, HEADER_TIMEOUT, request.send()).await?;
                validate_range(&response, start, end, meta)?;
                let expected = (end - start + 1) as usize;
                let mut bytes = Vec::with_capacity(expected);
                while let Some(chunk) = network(stop, NETWORK_IDLE, response.chunk()).await? {
                    self.record_network(id, chunk.len() as u64);
                    self.throttle(id, chunk.len() as u64, stop).await?;
                    if bytes.len() + chunk.len() > expected {
                        return Err(Fault::Restart(
                            "A continuity check returned an invalid byte count".into(),
                        ));
                    }
                    bytes.extend_from_slice(&chunk);
                }
                if bytes.len() != expected {
                    return Err(Fault::Retry(
                        "A continuity check was truncated".into(),
                        Duration::ZERO,
                    ));
                }
                Ok(bytes)
            }
            .await;
            match result {
                Err(Fault::Retry(message, delay) | Fault::Transport(message, delay)) if attempt < retries => {
                    self.delay_retry(id, attempt, &message, delay, stop).await?
                }
                result => return result,
            }
        }
        unreachable!()
    }

    async fn validate_saved(&self, id: &str, meta: &Meta, stop: &mut Stop) -> Result<(), Fault> {
        let parts = self.task(id).map_err(Fault::Fatal)?.parts;
        for part in parts.iter().filter(|part| part.downloaded > 0) {
            stop.check()?;
            let mut file = fs::File::open(self.part_path(id, part.id))
                .await
                .map_err(disk)?;
            let length = part.downloaded.min(OVERLAP);
            let mut offsets = vec![0];
            if part.downloaded > length {
                offsets.push(part.downloaded - length);
            }
            for offset in offsets {
                let mut expected = vec![0; length as usize];
                file.seek(SeekFrom::Start(part.start + offset)).await.map_err(disk)?;
                file.read_exact(&mut expected).await.map_err(disk)?;
                let actual = self
                    .sample_range(
                        id,
                        part.start + offset,
                        part.start + offset + length - 1,
                        meta,
                        stop,
                    )
                    .await?;
                if actual != expected {
                    return Err(Fault::Restart("Saved bytes no longer match the remote file. Restarting one complete stream to avoid mixing versions.".into()));
                }
            }
            // Recover a boundary witness even if a crash happened before its metadata checkpoint.
            if part.start > 0 {
                let start = part.start.saturating_sub(OVERLAP);
                let bytes = self
                    .sample_range(id, start, part.start - 1, meta, stop)
                    .await?;
                let mut state = lock(&self.inner.state);
                if let Some(task) = state.tasks.iter_mut().find(|t| t.view.id == id) {
                    if let Some(current) = task.parts.iter_mut().find(|p| p.id == part.id) {
                        current.overlap_start = start;
                        current.overlap_len = part.start - start;
                        current.overlap_sha256 = Some(digest(&bytes));
                    }
                }
            }
        }
        self.changed();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn durable_progress_renews_transport_retry_budget() {
        let mut part = super::Part::default();
        for durable in [10, 10, 20, 20, 30, 30] {
            part.durable = durable;
            assert!(part.record_transport_failure(2));
        }
        assert_eq!(part.transport_failures, 2);
    }

    #[test]
    fn transport_failures_without_durable_progress_exhaust_budget() {
        let mut part = super::Part { durable: 10, ..Default::default() };
        assert!(part.record_transport_failure(2));
        part.downloaded = 20; // Uncheckpointed bytes do not renew the budget.
        assert!(part.record_transport_failure(2));
        assert!(!part.record_transport_failure(2));
    }
    use super::*;

    #[test]
    fn split_requires_current_response_payload_and_enough_remaining_bytes() {
        let now = Instant::now();
        let part = Part { id: 1, end: 4 * 1024 * 1024, downloaded: 64 * 1024,
            active: true, response_end: 4 * 1024 * 1024,
            response_started_at: Some(now - Duration::from_millis(200)),
            response_payload_bytes: 64 * 1024, ..Part::default() };
        assert_eq!(split_ready_at(&part, now, Duration::from_millis(200)), Some(now));
        for invalid in 0..5 {
            let mut candidate = part.clone();
            match invalid {
                0 => candidate.response_end = 0,
                1 => candidate.response_payload_bytes = 0, // Saved bytes are not current response progress.
                2 => candidate.response_started_at = None,
                3 => candidate.active = false,
                _ => candidate.end = candidate.downloaded + 2 * MIN_SEGMENT - 1,
            }
            assert_eq!(split_ready_at(&candidate, now, Duration::from_millis(200)), None,
                "unproductive or unsafe candidate {invalid}");
        }
    }

    #[test]
    fn split_cost_defers_fast_tails_but_reconsiders_stalled_progress() {
        let now = Instant::now();
        let part = Part { id: 1, end: 10 * 1024 * 1024, downloaded: 9 * 1024 * 1024,
            active: true, response_end: 10 * 1024 * 1024,
            response_started_at: Some(now - Duration::from_secs(1)),
            response_payload_bytes: 9 * 1024 * 1024, ..Part::default() };
        let when = split_ready_at(&part, now, Duration::from_millis(250)).unwrap();
        assert!(when >= now + Duration::from_secs(7));
        assert!(when <= now + Duration::from_secs(9));
        // If no further data arrives, a later wake can help without waiting
        // for the global45-second network timeout or periodically polling.
        assert_eq!(split_ready_at(&part, when, Duration::from_millis(250)), Some(when));
        // The same fast stream on a low-latency origin may benefit from help.
        assert_eq!(split_ready_at(&part, now, Duration::from_millis(1)), Some(now));
    }

    #[test]
    fn large_healthy_range_rechecks_spare_worker_within_three_seconds() {
        let now = Instant::now();
        let part = Part { end: 2 * 1024 * 1024 * 1024,
            downloaded: 500 * 1024 * 1024, active: true,
            response_end: 2 * 1024 * 1024 * 1024,
            response_started_at: Some(now - Duration::from_secs(1)),
            response_payload_bytes: 500 * 1024 * 1024, ..Part::default() };
        let ready = split_ready_at(&part, now, Duration::from_secs(12)).unwrap();
        assert!(ready >= now);
        assert!(ready <= now + Duration::from_secs(3));
    }
    fn rollback_parts() -> Vec<Part> {
        vec![
            Part { id: 1, start: 0, end: 100, downloaded: 80, durable: 80,
                active: true, owner_lane: Some(0), response_end: 200, ..Part::default() },
            Part { id: 2, start: 100, end: 200, active: true, owner_lane: Some(1),
                split_parent: Some(1), ..Part::default() },
        ]
    }

    #[test]
    fn rollback_requires_unclipped_live_parent_and_untouched_child() {
        let original = rollback_parts();
        // In particular, a reserved write reaching the OLD boundary must not
        // be extended: its received chunk may have had an unreserved suffix.
        for invalid in 0..12 {
            let mut parts = original.clone();
            match invalid {
                0 => parts[0].reserved = 20,
                1 => parts[0].downloaded = 100,
                2 => parts[0].response_end = 199,
                3 => parts[0].active = false,
                4 => parts[1].downloaded = 1,
                5 => parts[1].durable = 1,
                6 => parts[1].reserved = 1,
                7 => parts[1].response_end = 200,
                8 => parts[1].start = 101,
                9 => parts[1].split_parent = None,
                10 => parts[0].response_end = 0,
                _ => parts[1].active = false,
            }
            let before: Vec<_> = parts.iter().map(|p| (p.id, p.start, p.end, p.downloaded, p.durable, p.reserved)).collect();
            assert!(!restore_untouched_split(&mut parts, 2), "invalid ownership case {invalid}");
            assert_eq!(parts.iter().map(|p| (p.id, p.start, p.end, p.downloaded, p.durable, p.reserved)).collect::<Vec<_>>(), before);
        }
        let mut parts = original;
        parts[0].reserved = 10;
        assert!(restore_untouched_split(&mut parts, 2));
        assert_eq!(parts.len(), 1);
        assert_eq!((parts[0].start, parts[0].end, parts[0].downloaded, parts[0].durable, parts[0].reserved), (0, 200, 80, 80, 10));
    }

    #[test]
    fn rollback_preserves_received_descendant_ownership() {
        let mut parts = rollback_parts();
        parts[1].end = 150;
        parts.push(Part { id: 3, start: 150, end: 200, downloaded: 10, durable: 10,
            active: true, owner_lane: Some(2), response_end: 200, split_parent: Some(2), ..Part::default() });
        assert!(restore_untouched_split(&mut parts, 2));
        assert_eq!(parts.len(), 2);
        assert_eq!((parts[0].id, parts[0].end), (1, 150));
        assert_eq!((parts[1].id, parts[1].start, parts[1].end, parts[1].downloaded, parts[1].durable), (3, 150, 200, 10, 10));
        assert_eq!(parts[1].split_parent, Some(1));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn history_replacement_retries_transient_sharing_and_reports_persistent_failure() {
        use std::os::windows::fs::OpenOptionsExt;
        let root = std::env::temp_dir().join(format!("qdm-history-sharing-{}", Uuid::new_v4()));
        fs::create_dir_all(&root).await.unwrap();
        let source = root.join("state-v2.tmp");
        let destination = root.join("state-v2.json");
        fs::write(&source, b"new valid state").await.unwrap();
        fs::write(&destination, b"old valid state").await.unwrap();
        let reader = std::fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2)
            .open(&destination).unwrap();
        let release = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(200)).await;
            drop(reader);
        });
        let started = Instant::now();
        replace_history(&source, &destination).await.unwrap();
        assert!(started.elapsed() >= Duration::from_millis(150));
        release.await.unwrap();
        assert_eq!(fs::read(&destination).await.unwrap(), b"new valid state");
        assert!(!source.exists());

        fs::write(&source, b"next valid state").await.unwrap();
        let reader = std::fs::OpenOptions::new().read(true).share_mode(0x1 | 0x2)
            .open(&destination).unwrap();
        let error = replace_history(&source, &destination).await.unwrap_err();
        assert!(matches!(error.raw_os_error(), Some(5 | 32 | 33 | 1224)), "{error:?}");
        // Failure neither reports success nor removes either recoverable file.
        assert_eq!(fs::read(&destination).await.unwrap(), b"new valid state");
        assert_eq!(fs::read(&source).await.unwrap(), b"next valid state");
        drop(reader);
        replace_history(&source, &destination).await.unwrap();
        assert_eq!(fs::read(&destination).await.unwrap(), b"next valid state");
        fs::remove_dir_all(root).await.unwrap();
    }

    #[test]
    fn hierarchical_bandwidth_limit_chooses_lowest_nonzero_budget() {
        assert_eq!(Engine::effective_speed_limit(0, 0, 0), 0);
        assert_eq!(Engine::effective_speed_limit(1024, 0, 0), 1024);
        assert_eq!(Engine::effective_speed_limit(1024, 512, 0), 512);
        assert_eq!(Engine::effective_speed_limit(1024, 0, 256), 256);
        assert_eq!(Engine::effective_speed_limit(100, 500, 700), 100);
    }

    #[test]
    fn rate_bucket_waits_for_bytes_then_charges_them_once() {
        let now = Instant::now();
        let mut bucket = RateBucket::default();
        assert_eq!(bucket.wait_duration(now, 2, 2048), Duration::from_secs(1));
        assert_eq!(bucket.wait_duration(now + Duration::from_secs(1), 2, 2048), Duration::ZERO);
        bucket.consume(2048);
        assert_eq!(bucket.wait_duration(now + Duration::from_secs(1), 2, 2048), Duration::from_secs(1));
    }

    // Phase 3 fairness benchmark: a global or per-task budget is a single shared
    // token bucket. Even with the maximum worker fan-out drawing on it concurrently,
    // their combined throughput is capped at one rate (workers must never each get an
    // entire job cap) yet no worker is starved far below that ceiling. Time is stepped
    // synthetically so the result is deterministic without real sleeps.
    #[test]
    fn max_workers_share_one_bucket_without_overshoot_or_starvation() {
        let start = Instant::now();
        let rate_kib: u64 = 100;
        let chunk: u64 = 1024;
        let workers: u32 = 64;
        let seconds: u64 = 10;
        let steps: u64 = seconds * 1000;
        let expected = rate_kib * 1024 * seconds; // one-rate ceiling for the whole run
        let mut bucket = RateBucket::default();
        let mut consumed: u64 = 0;
        for step in 0..steps {
            let now = start + Duration::from_millis(step + 1);
            for _ in 0..workers {
                if bucket.wait_duration(now, rate_kib, chunk).is_zero() {
                    bucket.consume(chunk);
                    consumed += chunk;
                }
            }
        }
        assert!(consumed <= expected, "shared budget overshot its one-rate ceiling: {consumed} > {expected}");
        assert!(consumed < expected * 2, "fan-out fairness broken: workers behaved like {}x independent budgets", consumed / expected);
        // Allow up to ~1% float/clamp drift; the run must still saturate the shared budget.
        assert!(consumed >= expected - chunk * 8, "workers were starved: {consumed} far below {expected}");
    }

    #[tokio::test]
    async fn pause_does_not_consume_shared_bandwidth_tokens() {
        let root = std::env::temp_dir().join(format!("qdm-throttle-{}", Uuid::new_v4()));
        let engine = Engine::open(root.join("state"), root.join("downloads"))
            .await
            .unwrap();
        let mut settings = engine.snapshot().settings;
        settings.speed_limit_kib = 1;
        engine.update_settings(settings).await.unwrap();
        let (pause, task) = watch::channel(false);
        let (_group, group) = watch::channel(false);
        let mut stop = Stop { task, group };
        let worker_engine = engine.clone();
        let worker =
            tokio::spawn(async move { worker_engine.throttle("", 1024 * 1024, &mut stop).await });
        tokio::time::timeout(Duration::from_secs(1), async {
            while lock(&engine.inner.throttle).global.updated_at.is_none() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        pause.send(true).unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), worker)
                .await
                .unwrap()
                .unwrap(),
            Err(Fault::Stopped)
        ));
        let (_pause, task) = watch::channel(false);
        let (_group, group) = watch::channel(false);
        let mut stop = Stop { task, group };
        assert!(
            tokio::time::timeout(Duration::from_millis(250), engine.throttle("", 1, &mut stop))
                .await
                .unwrap()
                .is_ok()
        );
        engine.shutdown().await.unwrap();
        fs::remove_dir_all(root).await.unwrap();
    }
}

