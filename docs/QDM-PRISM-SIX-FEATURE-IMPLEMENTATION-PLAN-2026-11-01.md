# QDM — Prism Six-Feature Implementation Plan
Date: 2026-11-01 (Asia/Bangkok)
Owner: QDM native Rust + Slint through QNECTOR
Status: IMPLEMENTATION STARTED — phased, not six features delivered.
Project: C:\Users\QUEEN\Projects\QDM\native

## Goals and constraints
Add all six previously identified Prism advantages without sacrificing the native Rust + Slint UI, 64-worker HTTP range engine, resumable direct writes, SQLite WAL, browser pairing, or file integrity. New code must maintain backwards-compatible deserialization from QDM 0.4.0. Keep all experiments isolated from the production profile. Preserve production EXE and data until independently verified. Note: Prism source comparison report was for QDM 0.3.0; QDM 0.4.0 already includes positioned writes, SQLite WAL, and shared host throttling.

## Phase 0 — safety and baseline (P0)
- Audit active QDM 0.4.0 binary/hash, sqlite, settings, existing tests and installed browser pairing. Back up before replacement.
- Fix and gate database/state update correctness where app restart can rewrite externally inserted history. Do not modify live SQLite while app is running.
- Tests: entire native engine suite, isolated HTTP acceptance, Firefox/Chromium bridge, UI smoke, installed-build SHA.
- Deliverable: dated baseline and test evidence, explicit list of no-go changes.

## Phase 1 — Browser capture parity (P1)
- Build one common normalized capture envelope: URL, suggested filename, Referer, user agent, optional scoped Cookie/Authorization, source origin, interception id, consent metadata.
- Firefox/Zen: preserve native auto-pair and capture acknowledgment, authenticated redirects and temporary URL refresh handling.
- Chromium MV3: support downloads interception/context menus with acknowledgment/fail-open fallback; prevent duplicate browser/QDM downloads.
- Security: never log stored headers or tokens, do not persist session secrets in history, allowlist origin scope, SameSite/cookie permission behavior, no password/DRM bypass.
- Tests: browser restart, Windows restart, authenticated localhost fixture, redirects/401/403, revoked permissions, ChatGPT image/signed URL (only where permitted), no duplicate downloads.

## Phase 2 — 10k-list UI virtualization (P1)
- Replace O(n) full row materialization and global model rewrite on telemetry with a windowed/virtualized Slint model or index range whose visible items update while preserving position and selection.
- Keep per-file sparkline, hot-row updates, stable focus and fast filter changes.
- Tests: 100, 1000 and 10000 jobs at 100/150/200% DPI, 60 Hz interaction, scroll/focus/selection and memory profiles.
- No UI rewrite to React/Tauri.

## Phase 3 — hierarchical speed limits (P1)
- Preserve existing global speed_limit_kib; add optional queue/group and per-download limits with backward-compatible serialized defaults.
- Enforce min-effective budgets using one traffic coordinator (avoid each worker independently consuming an entire job cap).
- Tests: multiple parallel files, changing limits while active, pause/resume, 0=unlimited, no starvation.

## Phase 4 — clipboard detection (P2)
- Add opt-in Windows clipboard watcher (disabled by default), debounced deduplication of well-formed HTTP(S) links only.
- Show unobtrusive suggestion; never auto-start a download without user interaction and never send clipboard content to server.
- Tests: repeated copy, private/sensitive text ignored, background Zen, rapid successive copies, permissions and manual disable.

## Phase 5 — queue, categories and scheduler (P2)
- Reuse existing scheduled_at and max_concurrent primitives: add queue entities, priorities, per-queue speed budget, categories (directory mapping), date/time scheduling, pause/resume queue, and robust clock/reboot behavior.
- SQLite versioned migration/rollback; use incremental upserts with durable changes; never silently lose historic records.
- Tests: order, retry and fairness, abrupt process death, empty queue, schedule across restarts/DST, simultaneous host throttle.

## Phase 6 — cross-platform and protocol/media modules (P3)
- Separate platform-agnostic Rust engine from Win32 integration. Keep Windows Slint installed as the primary target.
- Optional HTTP(S)-only macOS/Linux Slint proof of concept, then independent signed/verified adapters for FTP/FTPS, HLS/DASH, BitTorrent/magnet, Hugging Face model fetch, ZIP inspection, media transformations as individually feature-gated plugins. Do not ship any protocol without threat modeling and end-to-end tests.
- Respect third-party licenses, site credentials and content access permissions. No auto-install of yt-dlp/ffmpeg or extra binaries without review.
- Tests per adapter: real protocol fixture, checksums, interruption/resume, path traversal, resource cap, platform CI.

## Acceptance / rollout
- Release only after unit + HTTP acceptance + browser bridge + GUI automation + checksum and pause/resume tests pass on identical build artifact.
- Snapshot SQLite and binaries before replacing installed QDM; rollback on failed smoke.
- No edits to actual user downloads or unrequested user files.
- Keep all six as open until independent evidence proves end-to-end UI functionality. Phase 0 and the first safe foundation work begin in this task, not an unattended future commitment.

## Status log
- 2026-11-01: QNECTOR verified project/native sources, installed QDM 0.4.0, SQLite WAL integrity ok, zero download rows at inspection. Existing Chromium and Firefox extension trees identified. Existing scheduled_at support and 10,000 history cap identified. Implementation initiated.

## Initial implementation evidence (2026-11-01)
- Added a forward-compatible `PrismFeatureOptions` type to `src/api.rs`, with opt-in switches for clipboard suggestions, Chromium capture, browser authorization forwarding, default queue, per-file and queue speed limits, virtualization page size and optional protocol identifiers; validation bounds and HTTP(S)-only prototype permit list are included. This is **schema foundation only**, not a wired or user-visible feature. It has not changed persisted `Settings`, `TaskView`, database schema or browser integration.
- `cargo test --locked --bin QDM` using `C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat` succeeded: 32/32 tests passed. Warnings: feature struct and validation presently unused pending integration.
- Installed QDM.exe and production SQLite remain untouched. Six full feature phases are pending; no claim that all six are implemented. No release built or installed from this work.
- Implementation task: `task_426c6f2f-214f-41cd-a87d-b2499f0600c8`.

## Continuation evidence — 2026-11-01, task resumed
- Resumed task via QNECTOR memory.task_resume and verified live project, production QDM process, and SQLite integrity (0 current history records, integrity ok); no production changes.
- Confirmed Chromium MV3 extension is manually paired/context-menu only, and Firefox/Zen has DPAPI native host with auto-capture and fail-open after bridge acknowledgment. Both target authenticated localhost bridge. Session credentials stay transient in Rust task headers and are excluded from SQLite serialization.
- **Implemented and compiled** an incremental transaction-level SQLite WAL writer in `src/engine/store.rs`: unchanged rows are no longer deleted/reinserted; each download/checkpoint is upserted only when changed; vanished downloads and checkpoints are deleted by exact id; settings update only if changed. Existing atomic database transaction and WAL durability are preserved. No live DB migration needed.
- Added isolated regression `incremental_updates_preserve_untouched_rows_and_delete_only_removed_ids` verifying 3 rows, one updated, one removed, an untouched row, unchanged settings, and successful reload; PASS.
- Started phase 3 core with `effective_speed_limit(global, task, queue)` selection helper and test of unlimited/minimum behavior; currently only global legacy limit is wired, so **per-download and per-queue enforcement is not implemented yet**.
- Rust `cargo test --locked --bin QDM` succeeded **34/34** on updated sources, using `C:\BuildTools\VC\Auxiliary\Build\vcvars64.bat`.
- Browser capture parity, clipboard watcher, virtualized UI, queue UI and cross-platform/protocol adapters remain pending and disabled. No new release built or installed; user-facing six-feature completion is NOT claimed. Backup of old store.rs: `native\backups\store-pre-incremental-20261101-220230.rs`.

## 2026-11-01 continuation — Chromium MV3 Phase 1 source implementation
- Resumed memory task, verified project source and production state. Existing Chrome extension was v0.2.0 (manual token and context menu only).
- Implemented Chromium MV3 v0.3.0 **source**: native-message discovery via `com.queen.qdm`, QDM health/pairing retries, opt-in `downloads.onCreated` interception disabled by default, transfer acknowledgment before browser cancellation, Chrome fallback on errors, context-menu/popup compatibility, and explicit options UI toggles. Permissions now request `downloads` and `nativeMessaging`; no broad site or cookie permissions and no forwarding of session credentials.
- Updated Rust QDMHost caller-format logic to recognize Chromium extension ID syntax, in addition to original Firefox `qdm@queen.local`. **Not registered yet in Chrome/Edge Windows NativeMessagingHosts registry**: a per-extension-ID `allowed_origins` registration is required, with a real known extension ID before calling the browser parity feature operational. Do not use wildcards in the installed manifest.
- Verified `node --check` for background/options and valid MV3 manifest; `cargo test --locked --all-targets` passed 34 QDM tests plus other binaries 0/0. Production app, browser extensions, native-host registry, and data unchanged.
- Pending: register host for exact allowed Chrome extension ID on user's machine, install/test extension in isolated real Chromium profile, verify fail-open under QDM offline/errors and no duplicate downloads, rerun Zen tests, validate UI. This phase is source-level partial, NOT production complete.

## Continuation 2026-11-01 — hierarchical rate budgets (work in progress)
- Added backward-compatible `speed_limit_kib` and `queue` fields to `AddRequest` and `TaskView`; existing persisted jobs default to 0 (unlimited) / empty queue.
- Added global/per-task/per-queue independent reservation times in the throttle state; processing each network chunk now passes its task identity. The user-facing queue limit is currently prototyped using the minimum nonzero per-task limit within the queue, which is **not a finalized, separately configurable queue policy**. Queue UI, commands to modify limits on existing jobs, and runtime rate/throughput acceptance tests are still pending.
- `cargo test --locked --all-targets` passed 34/34 QDM Rust tests, no release installed.
- Outstanding correctness concern: queued transfer retries and pause must not cause spurious shared-rate delays; independently benchmark and test overlapping requests before release.

## Continuation evidence — 2026-11-01 — Phase 3 source completion
- Replaced independent time reservations with shared token buckets for global, per-download and named-queue budgets. Paused waits no longer reset unrelated global or queue budgets.
- Added backward-compatible `Settings.queue_speed_limits`; older 0.4.0 settings deserialize with no queue limits. `Default` limits apply to downloads without an assigned queue. Queue names and count are bounded.
- Added queue and per-download limits to the multi-add form and active task Details sheet. Existing limits update while a transfer runs.
- Updated the isolated Slint UI acceptance harness to read SQLite v3 and exercise the new controls. GUI acceptance passed 14/14; transfer output matched independent SHA-256. Production profile and installed QDM stayed untouched.
- `cargo test --locked --all-targets` passed 38/38 QDM tests. `cargo build --locked --bin QDM` succeeded. Isolated HTTP acceptance passed 6/6 rate-limit cases: global, per-download, named/default queue, active update, pause/resume and peer progress.
- Phase 3 source and isolated UI/HTTP evidence are complete. Release artifact, production installation and max-worker fairness benchmark remain unverified.
- Phase 1 remains source-partial: Chromium host registration and real-profile fail-open, duplicate, restart and Zen checks remain.
- Phase 2 remains open: telemetry still builds and diffs all visible-row models from full snapshots; 10k-job profiling is not complete.
- Phase 4 clipboard watcher remains pending. Phase 5 queue entities, priorities, categories and restart/DST scheduling remain pending. Phase 6 platform split and protocol/media adapters remain pending.
- All-six completion is **NOT** claimed. Keep release gates open until browser, GUI, checksum, pause/resume and platform evidence pass on one verified artifact.

## Continuation evidence — 2026-11-02 — isolated release candidate
- Built an isolated Windows release candidate, without modifying the installed program or production profile. QDM.exe SHA-256: `391ca22fc51fc8c29fbb50d9ea99b1a521ccdff82fbf360f44e15d06c698cd5d`.
- On that candidate: Rust tests 38/38, HTTP engine acceptance 40/40, bridge 18/18, public HTTPS 2/2, Slint UI 14/14, isolated Zen 10/10, staged recovery 3/3, tail scheduling 3/3, and adaptive recovery passed. Reports were checked by all nine current automated release gates.
- Updated release gates for the current 40 engine cases and 14 UI steps; regression tests passed 25/25. Updated packaging and NSIS version handling to use the package version and selected executable. Isolated packaging produced a portable ZIP and an installer, but neither has been published or installed.
- **Not a full six-feature release:** the existing nine gates do not test real Chromium interception, Chromium native-host registration, 10k virtualization, clipboard detection, full queues/categories/scheduling, or optional protocol/platform adapters. Those features remain open. The installer and portable package have not had an independent clean-install smoke test, and the existing installer does not deploy or register QDMHost for browser auto-pairing.
- Release artifacts and evidence are isolated under `C:\Users\QUEEN\AppData\Local\Temp\opencode\qdm-release-20261101-*`; no release artifact is certified for production installation or upload.
