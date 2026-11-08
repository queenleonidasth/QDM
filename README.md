# QDM — QUEEN Download Manager

QDM is a native Windows x64 download manager built with Rust and Slint. Its HTTP(S) engine supports parallel range downloads, pause and resume, and integrity checks. The desktop interface uses a dark and gold theme.

## Status

The current source is version 0.4.2. The [six-feature implementation plan](docs/QDM-PRISM-SIX-FEATURE-IMPLEMENTATION-PLAN-2026-11-01.md) remains in progress; it is not a claim that all planned features are complete. A release requires the tests in that plan to pass against the same build artifact.

## Getting started

1. Paste an HTTP or HTTPS link into **Download URL** and select **Download**.
2. Use **All**, **Active**, and **Finished** to filter the download list.
3. Select a download to inspect its progress, speed, and connection activity. Every row, including failed and paused entries, has a checkbox for bulk removal.
4. Use **Pause** and **Resume** for individual downloads, or **Pause all** and **Resume all** for all downloads.
5. Use **Show File** to reveal a completed download in File Explorer. To update an expired link, open **Details**, select **New URL**, and provide a replacement for the same file.
6. **Remove from list only** keeps a completed file. The separate delete option removes the local file when it is safe to do so.

Use **Add multiple downloads** in Preferences to enter one URL per line. You can choose a queue, set a per-download speed limit, and disable immediate start. For a single download, an optional expected SHA-256 verifies the whole file against a trusted independent source. A mismatch prevents publication of the final file.

## Preferences and limits

Preferences include the per-file connection ceiling (1–64, default 32), concurrent download count (1–8), global speed limit, retry attempts, download folder, close-to-tray behavior, and reduced motion.

Speed limits use KiB/s; zero means unlimited. The multi-download form and each download's **Details** sheet also support a per-download limit and queue name. In Preferences, enter queue limits one per line, for example:

```text
Default=512
media=256
```

`Default` applies to downloads without a queue name. The engine enforces the applicable global, queue, and per-download limits together. Queue names are case-sensitive. Queue scheduling, priorities, categories, and queue pause/resume remain planned.

## Connections, resume, and integrity

The connection setting is a ceiling, not a speed guarantee. QDM starts with fewer workers and can add connections as data arrives. It adapts to server restrictions such as HTTP 429 responses, and may split eligible remaining ranges between workers. A worker can receive more than one piece; piece identifiers can therefore exceed 64 without exceeding the worker ceiling.

QDM checks server validators and overlapping data before reusing saved progress. Servers without range support use a single stream, which may restart from the beginning after a pause. Sampled overlaps cannot prove that every unsampled byte is unchanged. QDM calculates a SHA-256 for completed files; only a comparison against a trusted expected hash independently verifies the result.

The Details sheet shows individual worker activity and the calculated SHA-256. Its connection history for older downloads may be estimated.

## Browser integration

The Firefox/Zen extension and native host support automatic pairing on Windows. The host stores the pairing token using Windows DPAPI. Install the extension and native host using the provided scripts, or pair manually through **Browser connection** as a fallback.

Automatic capture waits for QDM to accept the download before cancelling the browser download. If QDM rejects the request or is unavailable, the browser keeps the original download. Cancellation failures may still produce duplicates.

The Chromium extension includes opt-in capture source code, but native-host registration for a specific extension ID and real-browser acceptance testing remain outstanding. Firefox/Zen evidence does not establish Chromium compatibility.

The extensions do not automatically forward website cookies or authorization headers unless **session forwarding** is enabled (default on, toggle in the extension options). When enabled, the extension sends the captured URL's cookies, the referring page and the browser user agent to QDM over the authenticated loopback bridge. QDM applies them only to that download, keeps them in memory, never writes them to download history, and strips cookies and authorization headers on cross-host redirects. Turn the toggle off to keep protected sessions entirely in the browser.

A capture waits briefly for the browser's redirect chain and hands QDM the **final URL**, not the page that started the download. This matters for sites whose visible link redirects to a separate file host. The browser's own copy is cancelled only after QDM reports real download progress; if QDM cannot start (a bot challenge, an expired link, or an origin error), the browser keeps its download instead of losing both copies. QDM does not solve interactive bot challenges (for example Cloudflare's JavaScript check) or bypass authentication or DRM.

## Data and privacy

| Item | Location |
|---|---|
| Installed program | `%LOCALAPPDATA%\Programs\QDM\QDM.exe` |
| Download history and checkpoints | `%APPDATA%\QDM\state-v3.sqlite3` |
| UI appearance settings | `%APPDATA%\QDM\ui-settings.json` |
| Partial download | `.qdm-<task-id>.partial` in the download folder |

Browser session headers forwarded by the extension (cookies, referer, user agent) are kept in memory for the life of the download and are never written to the database.

Keep a backup of the database and program before replacing an installed build. Do not modify a live SQLite database while QDM is running.

## Build and test

Building the Windows UI requires Windows x64, Rust stable with the MSVC toolchain, Microsoft C++ Build Tools, and the Windows SDK. `Cargo.lock` pins dependencies. QDM uses Slint 1.18.1 and Windows native TLS; Node.js is used by acceptance tests, not by the installed application.

```powershell
powershell -NoProfile -ExecutionPolicy Bypass -File scripts\build.ps1
cargo test --locked --all-targets
node tests\acceptance.cjs --binary target\release\QDM.exe
node tests\bridge-native.cjs --binary target\release\QDM.exe
node tests\browser-session.cjs
node tests\browser-capture.cjs
powershell -NoProfile -ExecutionPolicy Bypass -File tests\slint-ui-acceptance.ps1 -Binary target\release\QDM.exe -OutputDirectory "$env:TEMP\qdm-ui-evidence"
```

Run Rust commands in a Visual Studio Developer Command Prompt when the Windows SDK tools are not already on `PATH`. These commands describe testing steps; they do not certify a release. Verify the complete release-gate evidence and executable SHA-256 before distribution. The engine also exposes a JSON `--cli` mode; the desktop UI in this build requires Windows.

FTP, site grabbing, universal video extraction, DRM support, and site-specific account systems are not verified features. See [third-party notices](docs/third-party/README.md) for dependency licenses.
