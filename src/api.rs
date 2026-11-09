use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const MAX_SPEED_LIMIT_KIB: u64 = 1024 * 1024 * 1024;
pub const MAX_QUEUE_COUNT: usize = 256;

pub fn normalize_queue_name(value: &str) -> Result<String, String> {
    let name = value.trim();
    if name.chars().count() > 64 || name.contains('=') || name.chars().any(char::is_control) {
        return Err("Queue names must be 64 characters or fewer and cannot contain control characters or '='.".into());
    }
    Ok(name.to_owned())
}

/// Per-queue scheduling metadata. Priority orders which queues start first when
/// concurrency slots free up (higher wins); category_dir routes a queue's downloads
/// to a dedicated folder; paused stops that queue from launching new transfers and
/// suspends its in-flight ones. Serializes inside `Settings`, so queue entities
/// persist with the existing settings blob and need no separate schema migration.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct QueueConfig {
    pub priority: i32,
    pub category_dir: String,
    pub paused: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub connections: u8,
    pub shed_slow: bool,
    pub max_concurrent: u8,
    pub speed_limit_kib: u64,
    pub retries: u8,
    pub download_dir: String,
    pub close_to_tray: bool,
    /// Opt-in clipboard link detection. Disabled by default so QDM never inspects
    /// the clipboard unless the user turns it on in Preferences.
    pub clipboard_suggestions: bool,
    pub queue_speed_limits: BTreeMap<String, u64>,
    pub queues: BTreeMap<String, QueueConfig>,
}
impl Default for Settings {
    fn default() -> Self { Self { connections: 8, shed_slow: true, max_concurrent: 2, speed_limit_kib: 0, retries: 3, download_dir: String::new(), close_to_tray: true, clipboard_suggestions: false, queue_speed_limits: BTreeMap::new(), queues: BTreeMap::new() } }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AddRequest {
    pub url: String,
    pub filename: Option<String>,
    pub directory: Option<String>,
    pub expected_sha256: Option<String>,
    pub start: bool,
    pub headers: BTreeMap<String, String>,
    pub scheduled_at: Option<u64>,
    pub speed_limit_kib: u64,
    pub queue: String,
}
impl Default for AddRequest {
    fn default() -> Self { Self { url: String::new(), filename: None, directory: None, expected_sha256: None, start: true, headers: BTreeMap::new(), scheduled_at: None, speed_limit_kib: 0, queue: String::new() } }
}
// Optional cross-feature configuration, disabled unless explicitly enabled.
// Keeping this separate from Settings/TaskView preserves QDM 0.4.0 SQLite history compatibility.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
#[serde(default)]
pub struct PrismFeatureOptions {
    pub clipboard_suggestions: bool,
    pub chromium_capture: bool,
    pub browser_auth_forwarding: bool,
    pub default_queue: Option<String>,
    pub per_download_speed_limit_kib: u64,
    pub queue_speed_limit_kib: u64,
    pub virtualization_page_size: usize,
    pub optional_protocol_adapters: Vec<String>,
}

impl PrismFeatureOptions {
    pub fn validate(&mut self) {
        self.per_download_speed_limit_kib = self.per_download_speed_limit_kib.min(1024 * 1024 * 1024);
        self.queue_speed_limit_kib = self.queue_speed_limit_kib.min(1024 * 1024 * 1024);
        self.virtualization_page_size = self.virtualization_page_size.clamp(32, 512);
        self.optional_protocol_adapters.retain(|value| matches!(value.as_str(), "https" | "http"));
    }
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct SegmentView {
    pub id: u64,
    pub start: u64,
    /// Exclusive byte boundary.
    pub end: u64,
    pub downloaded_bytes: u64,
    pub active: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct LaneView {
    /// Stable, one-based connection slot (never a dynamic part/chunk identifier).
    pub id: u8,
    /// Bytes accepted by this worker across its pieces, including its bounded
    /// write buffer. Durable resume checkpoints are tracked separately.
    pub transferred_bytes: u64,
    /// Progress of the most recently assigned piece.
    pub current_bytes: u64,
    pub current_total: u64,
    pub current_part_id: Option<u64>,
    pub active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct TaskView {
    pub id: String,
    pub url: String,
    pub filename: String,
    pub directory: String,
    pub file_path: Option<String>,
    pub status: String,
    pub downloaded_bytes: u64,
    pub total_bytes: Option<u64>,
    pub speed_bps: u64,
    pub connections_active: u8,
    pub connections_limit: u8,
    /// Raw chunks retained for resume and diagnostics; can exceed 64.
    pub segments: Vec<SegmentView>,
    /// Actual connection worker slots, always capped by configured connections (<=64).
    pub lanes: Vec<LaneView>,
    pub part_count: u32,
    pub lane_history_estimated: bool,
    /// Bytes physically written into the assembled output file.
    pub assembly_bytes: u64,
    pub assembly_total_bytes: u64,
    pub sha256: Option<String>,
    pub expected_sha256: Option<String>,
    pub error: Option<String>,
    pub notice: Option<String>,
    pub created_at: u64,
    pub completed_at: Option<u64>,
    pub scheduled_at: Option<u64>,
    /// Optional per-transfer rate limit. Zero means unlimited.
    pub speed_limit_kib: u64,
    /// User-defined queue/group name; empty means Default.
    pub queue: String,
    pub supports_ranges: bool,
    pub resumable: bool,
    pub integrity: String,
    pub dynamic_splits: u64,
    pub peak_connections: u8,
    pub resumed_bytes: u64,
    pub network_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Snapshot {
    pub downloads: Vec<TaskView>,
    pub settings: Settings,
    pub active_count: usize,
    pub total_speed_bps: u64,
    pub revision: u64,
}
#[derive(Debug, Clone)]
pub enum Command {
    Add(AddRequest),
    UpdateDownloadLimits { id: String, queue: String, speed_limit_kib: u64 },
    Pause(String),
    Resume(String),
    Remove(String),
    RemoveWithFile(String),
    ReplaceUrl { id: String, url: String },
    UpdateSettings(Settings),
    SetQueuePaused { name: String, paused: bool },
    PauseAll,
    ResumeAll,
    OpenFile(String),
    RevealFile(String),
    OpenDownloadFolder,
    ShowIntegration,
    Quit,
}
#[derive(Debug, Clone)]
pub enum UiEvent {
    State(Snapshot),
    Error(String),
    Notice(String),
    Integration { port: u16, token: String, extension_path: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn old_settings_default_to_no_queue_limits() {
        let settings: Settings = serde_json::from_str(r#"{"connections":64}"#).unwrap();
        assert_eq!(settings.connections, 64);
        assert!(settings.queue_speed_limits.is_empty());
    }

    #[test]
    fn queue_names_are_trimmed_and_reject_control_characters() {
        assert_eq!(normalize_queue_name("  media  ").unwrap(), "media");
        assert!(normalize_queue_name("bad\nqueue").is_err());
        assert!(normalize_queue_name("name=limit").is_err());
        assert!(normalize_queue_name(&"x".repeat(65)).is_err());
    }
}

