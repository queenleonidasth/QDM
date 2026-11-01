# Native QDM interface

The Windows interface is implemented in `src/ui/windows.rs` with `windows-sys` 0.61.2 and the operating system's edit, button, list-view, progress, menu, dialog, and tray controls. It has no embedded browser, webview, renderer process, GPU surface, or GUI framework runtime.

## Main screen

Paste an HTTP(S) link, then press **ดาวน์โหลด** or Enter. The table shows the filename, percentage or downloaded bytes, speed, actual/configured connections, and status. The six actions apply to the selected file: pause, resume, open, reveal in folder, replace URL, and remove the record. A compact detail line and progress strip follow the selected file.

The **⋯** menu provides settings, batch URLs and optional expected SHA-256, browser integration, the download folder, pause all, resume all, and exit. Native settings expose 1, 4, 8, 16, 32, or 64 connections (default 32), 1–8 simultaneous files, a global KiB/s limit, the download folder, and close-to-tray behavior. A custom valid connection value already saved by the command-line interface is retained in the selection list.

The connection count is the engine's measured active count alongside its configured limit. The interface does not manufacture 32 or 64 connections when the server or remaining file size supports fewer. Detailed peak connection, dynamic split, reused byte, computed SHA-256, and expected SHA-256 values are available from the selected file's context menu or Enter key.

The browser pairing dialog keeps its token masked, copies it only on an explicit button action, and opens the installed extension folder. It is not a persistent panel in the main window.

## Keyboard and tray

| Input | Action |
|---|---|
| Ctrl+N | Focus and select the URL field |
| Enter in URL field | Add and start the download |
| Space in the file list | Pause or resume the selected eligible task |
| Enter in the file list | Open selected task details |
| Ctrl+R in the file list | Replace the selected URL |
| Delete in the file list | Remove the selected download record and partial working data |
| Escape in a dialog | Close the dialog |
| Ctrl+Enter in a dialog | Apply its main action |
| Tab / Shift+Tab | Use native focus navigation |

Double-click a completed file to open it. Double-click an unfinished file to view its details. Closing the main window hides QDM to the tray when that setting is enabled and the tray icon exists; the engine continues running. The first such close displays one informational notification. The tray menu exposes Show, Pause all, Resume all, and Exit. A second launch is restored to the correct profile by the platform layer's window tag.

## Threading, updates, and ownership

`run(Sender<Command>, Receiver<UiEvent>) -> Result<(), String>` owns the Win32 UI thread. Commands are forwarded to the application worker. A small receiver thread blocks on incoming engine events, coalesces snapshots, and posts one pending `WM_APP + 1` notification. The UI thread blocks in `GetMessageW` when idle. There are no UI polling timers or continuous paint loops.

The file list caches displayed cells and updates only changed values. It rebuilds only when task identities/order change, retaining the selected task. Native list-view double buffering prevents unnecessary whole-window drawing. Error and notice events appear as a compact inline message. Validation dialogs appear only in response to an entered value or explicit action.

Windows can dispatch messages synchronously while controls are being updated or inside native menu, folder-picker, and message-box loops. Window procedures use `RefCell::try_borrow_mut` to avoid aliased mutable Rust state. Pending engine events are drained after the outer callback, preventing a coalesced notification from being lost inside a nested native loop. `IsDialogMessageW` runs without a state borrow, so keyboard button actions can safely re-enter the window procedure.

The main window and auxiliary dialogs scale with display DPI. They use Segoe UI with Windows' Thai glyph fallback. Dialog fonts are owned independently so moving a dialog between monitors does not invalidate the main window's font. The application manifest enables Common Controls v6 and per-monitor DPI awareness. Icon resource 101 is supplied by the build's resources.

## Automation identifiers

Main window class: `QDM.Native.Main.v2`. Title: `QDM — QUEEN Download Manager`.

Auxiliary window class: `QDM.Native.Dialog.v2`.

| Identifier | Control or command |
|---:|---|
| 1001 | URL edit |
| 1002 | Download |
| 1003 | Main menu button |
| 1004 | Native `SysListView32` file list |
| 1005–1010 | Pause, resume, open, folder, refresh URL, remove |
| 1101 | Settings command |
| 1102 | Batch / SHA-256 command |
| 1103 | Browser integration command |
| 1104 / 1105 | Pause all / resume all |
| 1106 / 1107 | Exit / show main window |
| 1108 / 1109 | Details / open download folder |
| 1201 / 1202 | Apply / cancel or close auxiliary dialog |
| 1203 | Browse download folder |
| 1204 / 1205 | Copy pairing token / open extension folder |
| 1302 | Connection selection |
| 1304 / 1306 | Simultaneous file count / KiB/s limit |
| 1309 / 1310 | Download directory / close-to-tray checkbox |
| 1403 | Replacement URL |
| 1502 / 1504 / 1505 | Batch URLs / expected SHA-256 / start checkbox |
| 1604 | Masked pairing token |
| 1701 | Read-only task details |

Send a command with `WM_COMMAND` to the relevant main or auxiliary window. Read or manipulate native controls using their ordinary Win32 or accessibility interfaces. Connection combo indexes are 0–5 for the standard six values, with 64 at index 5. Use isolated application data and download directories for smoke tests.

## Verification boundaries

The implementation passes Rust type checking for `x86_64-pc-windows-msvc` from the local toolchain. That check validates bindings, message structures, and Rust ownership, but does not execute the native controls. Actual Windows smoke, resource measurements, file integrity, pause/resume, tray restore, DPI appearance, and browser integration results are recorded by the project's runtime test/release evidence, not inferred from compilation.
