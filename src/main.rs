#![cfg_attr(windows, windows_subsystem = "windows")]
mod api;
mod bridge;
mod clipboard;
mod engine;
mod platform;
#[cfg(windows)]
mod ui {
    pub mod slint_ui;
}

use api::{AddRequest, Command, Settings, UiEvent};
use engine::Engine;
use serde_json::{json, Value};
use std::{
    io::{BufRead, Write},
    path::PathBuf,
};

fn main() {
    if let Err(error) = run() {
        eprintln!("QDM: {error}");
        if !std::env::args().any(|a| a == "--cli") {
            platform::error(&error);
        }
        std::process::exit(1);
    }
}
fn argument(args: &[String], key: &str) -> Option<PathBuf> {
    args.windows(2)
        .find(|pair| pair[0] == key)
        .map(|pair| PathBuf::from(&pair[1]))
}
fn run() -> Result<(), String> {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--version") {
        println!("QDM {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let cli = args.iter().any(|a| a == "--cli");
    let data = argument(&args, "--data-dir").unwrap_or_else(platform::data_dir);
    let downloads = argument(&args, "--download-dir").unwrap_or_else(platform::download_dir);
    if !data.is_absolute() || !downloads.is_absolute() {
        return Err("Data and download directories must be absolute paths.".into());
    }
    #[cfg(windows)]
    let _instance = match platform::SingleInstance::acquire(&data, !cli)? {
        Some(v) => v,
        None => return Ok(()),
    };
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .max_blocking_threads(4)
        .thread_name("qdm-worker")
        .enable_all()
        .build()
        .map_err(|e| e.to_string())?;
    let first_run = !data.join("state-v3.sqlite3").exists() && !data.join("state-v2.json").exists();
    let engine = runtime.block_on(Engine::open(data.clone(), downloads))?;
    if first_run {
        runtime.block_on(migrate_settings(&engine, &data))?;
    }
    if cli {
        let server = if args.iter().any(|a| a == "--bridge") {
            Some(runtime.block_on(bridge::start(engine.clone(), &data))?)
        } else {
            None
        };
        return run_cli(runtime, engine, server);
    }
    #[cfg(not(windows))]
    {
        runtime.block_on(engine.shutdown())?;
        return Err(
            "The native desktop interface requires Windows. Use --cli for command-line operation."
                .into(),
        );
    }
    #[cfg(windows)]
    {
        let (commands_tx, commands_rx) = std::sync::mpsc::channel();
        let (events_tx, events_rx) = std::sync::mpsc::channel();
        let bridge = runtime.block_on(bridge::start(engine.clone(), &data));
        let integration = match bridge {
            Ok(v) => Some(v),
            Err(e) => {
                let _ = events_tx.send(UiEvent::Notice(format!(
                    "Browser connection unavailable: {e}"
                )));
                None
            }
        };
        // Subscribe before the baseline snapshot so transitions during startup cannot be lost.
        let mut updates = engine.subscribe();
        let initial_snapshot = engine.snapshot();
        let initial_revision = initial_snapshot.revision;
        let _ = events_tx.send(UiEvent::State(initial_snapshot));
        let initial_warning = engine.warning();
        if let Some(warning) = initial_warning.as_ref() {
            let _ = events_tx.send(UiEvent::Notice(warning.clone()));
        }
        let relay_tx = events_tx.clone();
        let relay_engine = engine.clone();
        runtime.spawn(async move {
            let mut previous_warning = initial_warning;
            let mut last_revision = initial_revision;
            loop {
                match updates.recv().await {
                    Ok(s) => {
                        if s.revision <= last_revision { continue; }
                        last_revision = s.revision;
                        if relay_tx.send(UiEvent::State(s)).is_err() {
                            break;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        let snapshot = relay_engine.snapshot();
                        if snapshot.revision <= last_revision { continue; }
                        last_revision = snapshot.revision;
                        if relay_tx
                            .send(UiEvent::State(snapshot))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
                let warning = relay_engine.warning();
                if warning != previous_warning {
                    if let Some(value) = warning.as_ref() {
                        let _ = relay_tx.send(UiEvent::Notice(value.clone()));
                    }
                    previous_warning = warning;
                }
            }
        });
        let worker = std::thread::spawn(move || {
            while let Ok(command) = commands_rx.recv() {
                if matches!(command, Command::Quit) {
                    break;
                }
                let result = runtime.block_on(dispatch(&engine, command, &integration));
                match result {
                    Ok(Some(event)) => {
                        let _ = events_tx.send(event);
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let _ = events_tx.send(UiEvent::Error(e));
                    }
                }
            }
            if let Some(server) = integration {
                server.close();
            }
            if let Err(e) = runtime.block_on(engine.shutdown()) {
                eprintln!("QDM shutdown: {e}");
            }
        });
        let start_hidden = args.iter().any(|a| a == "--startup-minimized" || a == "--silent");
        let result = ui::slint_ui::run(commands_tx.clone(), events_rx, &data, start_hidden);
        let _ = commands_tx.send(Command::Quit);
        worker
            .join()
            .map_err(|_| "QDM background worker stopped unexpectedly".to_string())?;
        result
    }
}

async fn migrate_settings(engine: &Engine, data: &std::path::Path) -> Result<(), String> {
    let Ok(bytes) = std::fs::read(data.join("downloads.json")) else {
        return Ok(());
    };
    let Ok(legacy) = serde_json::from_slice::<Value>(&bytes) else {
        return Ok(());
    };
    let mut settings = engine.snapshot().settings;
    if let Some(old) = legacy.get("settings") {
        if let Some(folder) = old
            .get("downloadDir")
            .and_then(Value::as_str)
            .filter(|p| std::path::Path::new(p).is_absolute())
        {
            settings.download_dir = folder.into();
        }
        if let Some(v) = old.get("maxConcurrent").and_then(Value::as_u64) {
            settings.max_concurrent = v.clamp(1, 8) as u8;
        }
        if let Some(v) = old.get("speedLimitKBps").and_then(Value::as_u64) {
            settings.speed_limit_kib = v;
        }
        if let Some(v) = old.get("closeToTray").and_then(Value::as_bool) {
            settings.close_to_tray = v;
        }
    }
    // The old file and its partial files are kept intact. New downloads use state-v2.json.
    engine.update_settings(settings).await
}

fn run_cli(
    runtime: tokio::runtime::Runtime,
    engine: Engine,
    server: Option<bridge::Bridge>,
) -> Result<(), String> {
    let stdin = std::io::stdin();
    let mut output = std::io::BufWriter::new(std::io::stdout());
    for line in stdin.lock().lines() {
        let line = line.map_err(|e| e.to_string())?;
        if line.trim().is_empty() {
            continue;
        }
        let parsed = serde_json::from_str::<Value>(&line);
        let id = parsed
            .as_ref()
            .ok()
            .and_then(|v| v.get("id"))
            .cloned()
            .unwrap_or(Value::Null);
        let is_shutdown = parsed
            .as_ref()
            .ok()
            .and_then(|v| v.get("op"))
            .and_then(Value::as_str)
            == Some("shutdown");
        let response = match parsed {
            Ok(request) => match runtime.block_on(cli_request(&engine, request, server.as_ref())) {
                Ok(value) => json!({"id":id,"ok":true,"result":value}),
                Err(error) => json!({"id":id,"ok":false,"error":error}),
            },
            Err(e) => json!({"id":id,"ok":false,"error":e.to_string()}),
        };
        serde_json::to_writer(&mut output, &response).map_err(|e| e.to_string())?;
        output
            .write_all(b"\n")
            .and_then(|_| output.flush())
            .map_err(|e| e.to_string())?;
        if is_shutdown {
            return Ok(());
        }
    }
    runtime.block_on(engine.shutdown())
}
async fn cli_request(
    engine: &Engine,
    r: Value,
    server: Option<&bridge::Bridge>,
) -> Result<Value, String> {
    let task = || {
        r.get("task_id")
            .and_then(Value::as_str)
            .ok_or_else(|| "task_id is required".to_string())
    };
    match r.get("op").and_then(Value::as_str).unwrap_or("") {
        "integration" => {
            let b = server
                .ok_or("Start QDM with --bridge to enable browser integration in CLI mode")?;
            Ok(json!({"port":b.port,"token":b.token}))
        }
        "snapshot" => serde_json::to_value(engine.snapshot()).map_err(|e| e.to_string()),
        "add" => {
            let request: AddRequest =
                serde_json::from_value(r.get("request").cloned().unwrap_or(Value::Null))
                    .map_err(|e| e.to_string())?;
            Ok(json!(engine.add(request).await?))
        }
        "settings" => {
            let mut value =
                serde_json::to_value(engine.snapshot().settings).map_err(|e| e.to_string())?;
            let patch = r
                .get("settings")
                .and_then(Value::as_object)
                .ok_or("settings must be an object")?;
            for (key, v) in patch {
                value[key] = v.clone();
            }
            let settings: Settings = serde_json::from_value(value).map_err(|e| e.to_string())?;
            engine.update_settings(settings).await?;
            Ok(json!(engine.snapshot().settings))
        }
        "queue_pause" => {
            let queue = r.get("queue").and_then(Value::as_str).ok_or("queue is required")?;
            let paused = r.get("paused").and_then(Value::as_bool).unwrap_or(true);
            engine.set_queue_paused(queue, paused).await?;
            Ok(Value::Null)
        }
        "download_limits" => {
            let queue = r.get("queue").and_then(Value::as_str).ok_or("queue is required")?;
            let speed_limit_kib = r.get("speed_limit_kib").and_then(Value::as_u64).ok_or("speed_limit_kib must be a non-negative integer")?;
            engine.update_download_limits(task()?, queue, speed_limit_kib).await?;
            Ok(Value::Null)
        }
        "pause" => {
            engine.pause(task()?).await?;
            Ok(Value::Null)
        }
        "resume" => {
            engine.resume(task()?).await?;
            Ok(Value::Null)
        }
        "pause_all" => {
            engine.pause_all().await?;
            Ok(Value::Null)
        }
        "resume_all" => {
            engine.resume_all().await?;
            Ok(Value::Null)
        }
        "remove" => {
            engine.remove(task()?).await?;
            Ok(Value::Null)
        }
        "replace_url" => {
            engine
                .replace_url(
                    task()?,
                    r.get("url")
                        .and_then(Value::as_str)
                        .ok_or("url is required")?,
                )
                .await?;
            Ok(Value::Null)
        }
        "shutdown" => {
            engine.shutdown().await?;
            Ok(Value::Null)
        }
        _ => Err("Unknown operation".into()),
    }
}

#[cfg(windows)]
async fn dispatch(
    engine: &Engine,
    command: Command,
    integration: &Option<bridge::Bridge>,
) -> Result<Option<UiEvent>, String> {
    match command {
        Command::Add(r) => {
            engine.add(r).await?;
        }
        Command::UpdateDownloadLimits { id, queue, speed_limit_kib } => {
            engine.update_download_limits(&id, &queue, speed_limit_kib).await?;
        }
        Command::Pause(id) => engine.pause(&id).await?,
        Command::Resume(id) => engine.resume(&id).await?,
        Command::Remove(id) => engine.remove(&id).await?,
        Command::RemoveWithFile(id) => {
            if let Some(message) = engine.remove_with_file(&id).await? {
                return Ok(Some(UiEvent::Notice(message.to_owned())));
            }
        },
        Command::ReplaceUrl { id, url } => {
            engine.replace_url(&id, &url).await?;
            engine.resume(&id).await?;
        }
        Command::UpdateSettings(s) => engine.update_settings(s).await?,
        Command::SetQueuePaused { name, paused } => engine.set_queue_paused(&name, paused).await?,
        Command::PauseAll => engine.pause_all().await?,
        Command::ResumeAll => engine.resume_all().await?,
        Command::OpenFile(id) => {
            let t = engine
                .snapshot()
                .downloads
                .into_iter()
                .find(|t| t.id == id)
                .ok_or("Download not found")?;
            if t.status != "completed" {
                return Err("The download has not completed.".into());
            }
            let path = platform::existing_download_path(
                t.file_path.as_deref(), &t.directory, &t.filename,
            ).ok_or("The downloaded file is unavailable")?;
            platform::open(&path.to_string_lossy())?;
        }
        Command::RevealFile(id) => {
            let t = engine
                .snapshot()
                .downloads
                .into_iter()
                .find(|t| t.id == id)
                .ok_or("Download not found")?;
            let output = platform::existing_download_path(
                t.file_path.as_deref(), &t.directory, &t.filename,
            );
            let path = output.as_deref().or_else(|| t.file_path.as_deref().map(std::path::Path::new))
                .unwrap_or_else(|| std::path::Path::new(&t.directory));
            platform::reveal(&path.to_string_lossy())?;
        }
        Command::OpenDownloadFolder => platform::open(&engine.snapshot().settings.download_dir)?,
        Command::ShowIntegration => {
            let b = integration
                .as_ref()
                .ok_or("The browser bridge is unavailable.")?;
            return Ok(Some(UiEvent::Integration {
                port: b.port,
                token: b.token.clone(),
                extension_path: platform::extension_dir().display().to_string(),
            }));
        }
        Command::Quit => {}
    }
    Ok(None)
}

