//! Durable SQLite/WAL repository for download settings, jobs and independent checkpoints.
//! A single transaction owns each complete snapshot; network tasks never block on SQLite I/O.
use super::{Saved, Task, Part};
use rusqlite::{params, Connection, OptionalExtension};
use std::{path::PathBuf, time::Duration};

fn connect(path: &PathBuf) -> Result<Connection, String> {
    let db = Connection::open(path).map_err(|e| format!("Could not open QDM SQLite state: {e}"))?;
    db.busy_timeout(Duration::from_secs(5)).map_err(|e| e.to_string())?;
    db.pragma_update(None, "journal_mode", "WAL").map_err(|e| e.to_string())?;
    db.pragma_update(None, "synchronous", "FULL").map_err(|e| e.to_string())?;
    db.pragma_update(None, "foreign_keys", "ON").map_err(|e| e.to_string())?;
    db.execute_batch(
        "CREATE TABLE IF NOT EXISTS app_settings (
            id INTEGER PRIMARY KEY CHECK(id = 1),
            value_json BLOB NOT NULL
         );
         CREATE TABLE IF NOT EXISTS downloads (
            id TEXT PRIMARY KEY,
            ordinal INTEGER NOT NULL,
            created_at INTEGER NOT NULL,
            status TEXT NOT NULL,
            task_json BLOB NOT NULL
         );
         CREATE INDEX IF NOT EXISTS idx_downloads_order ON downloads(ordinal);
         CREATE INDEX IF NOT EXISTS idx_downloads_status ON downloads(status, created_at);
         CREATE TABLE IF NOT EXISTS checkpoints (
            download_id TEXT NOT NULL REFERENCES downloads(id) ON DELETE CASCADE,
            part_id INTEGER NOT NULL,
            start_byte INTEGER NOT NULL,
            end_byte INTEGER NOT NULL,
            durable_byte INTEGER NOT NULL,
            part_json BLOB NOT NULL,
            PRIMARY KEY(download_id, part_id)
         );
         PRAGMA user_version = 3;"
    ).map_err(|e| format!("Could not initialize QDM SQLite schema: {e}"))?;
    Ok(db)
}

fn load_sync(path: PathBuf) -> Result<Option<Saved>, String> {
    if !path.exists() { return Ok(None); }
    let db = connect(&path)?;
    let settings_json: Option<Vec<u8>> = db.query_row(
        "SELECT value_json FROM app_settings WHERE id = 1", [], |row| row.get(0)
    ).optional().map_err(|e| e.to_string())?;
    let Some(settings_json) = settings_json else {
        let rows: i64 = db.query_row("SELECT COUNT(*) FROM downloads", [], |r| r.get(0))
            .map_err(|e| e.to_string())?;
        if rows != 0 { return Err("SQLite has download jobs but no settings. Refusing data loss.".into()); }
        return Ok(None);
    };
    let settings = serde_json::from_slice(&settings_json).map_err(|e| format!("Invalid SQLite settings: {e}"))?;
    let mut tasks = Vec::new();
    let mut stmt = db.prepare("SELECT id, task_json FROM downloads ORDER BY ordinal")
        .map_err(|e| e.to_string())?;
    let mut rows = stmt.query([]).map_err(|e| e.to_string())?;
    while let Some(row) = rows.next().map_err(|e| e.to_string())? {
        let id: String = row.get(0).map_err(|e| e.to_string())?;
        let blob: Vec<u8> = row.get(1).map_err(|e| e.to_string())?;
        let mut task: Task = serde_json::from_slice(&blob)
            .map_err(|e| format!("Could not decode saved download {id}: {e}"))?;
        if task.view.id != id { return Err(format!("SQLite download identity mismatch: {id}")); }
        let mut ps = db.prepare("SELECT part_json FROM checkpoints WHERE download_id = ?1 ORDER BY start_byte")
            .map_err(|e| e.to_string())?;
        let raw = ps.query_map(params![id], |r| r.get::<_, Vec<u8>>(0))
            .map_err(|e| e.to_string())?;
        for item in raw {
            let blob = item.map_err(|e| e.to_string())?;
            task.parts.push(serde_json::from_slice::<Part>(&blob)
                .map_err(|e| format!("Could not decode download checkpoint: {e}"))?);
        }
        tasks.push(task);
    }
    Ok(Some(Saved { schema_version: 3, settings, tasks }))
}

fn save_sync(path: PathBuf, saved: Saved) -> Result<(), String> {
    let mut db = connect(&path)?;
    let tx = db.transaction().map_err(|e| e.to_string())?;
    let mut existing = std::collections::HashSet::<String>::new();
    {
        let mut statement = tx.prepare("SELECT id FROM downloads").map_err(|e| e.to_string())?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0)).map_err(|e| e.to_string())?;
        for row in rows { existing.insert(row.map_err(|e| e.to_string())?); }
    }
    let settings_blob = serde_json::to_vec(&saved.settings).map_err(|e| e.to_string())?;
    tx.execute(
        "INSERT INTO app_settings(id, value_json) VALUES(1, ?1)
         ON CONFLICT(id) DO UPDATE SET value_json=excluded.value_json
         WHERE app_settings.value_json != excluded.value_json",
        params![settings_blob]
    ).map_err(|e| e.to_string())?;
    for (index, task) in saved.tasks.iter().enumerate() {
        existing.remove(&task.view.id);
        let mut summary = task.clone();
        summary.parts.clear();
        let json = serde_json::to_vec(&summary).map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO downloads(id, ordinal, created_at, status, task_json)
             VALUES(?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(id) DO UPDATE SET
                 ordinal=excluded.ordinal,
                 created_at=excluded.created_at,
                 status=excluded.status,
                 task_json=excluded.task_json
             WHERE downloads.ordinal != excluded.ordinal
                OR downloads.created_at != excluded.created_at
                OR downloads.status != excluded.status
                OR downloads.task_json != excluded.task_json",
            params![task.view.id, index as i64, task.view.created_at as i64, task.view.status, json]
        ).map_err(|e| e.to_string())?;
        let mut stale_parts = std::collections::HashSet::<i64>::new();
        {
            let mut statement = tx.prepare("SELECT part_id FROM checkpoints WHERE download_id=?1")
                .map_err(|e| e.to_string())?;
            let rows = statement.query_map(params![task.view.id], |row| row.get::<_, i64>(0))
                .map_err(|e| e.to_string())?;
            for row in rows { stale_parts.insert(row.map_err(|e| e.to_string())?); }
        }
        for part in &task.parts {
            stale_parts.remove(&(part.id as i64));
            tx.execute(
                "INSERT INTO checkpoints(download_id, part_id, start_byte, end_byte, durable_byte, part_json)
                 VALUES(?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(download_id, part_id) DO UPDATE SET
                     start_byte=excluded.start_byte,
                     end_byte=excluded.end_byte,
                     durable_byte=excluded.durable_byte,
                     part_json=excluded.part_json
                 WHERE checkpoints.start_byte != excluded.start_byte
                    OR checkpoints.end_byte != excluded.end_byte
                    OR checkpoints.durable_byte != excluded.durable_byte
                    OR checkpoints.part_json != excluded.part_json",
                params![task.view.id, part.id as i64, part.start as i64,
                    part.end as i64, part.durable as i64,
                    serde_json::to_vec(part).map_err(|e| e.to_string())?]
            ).map_err(|e| e.to_string())?;
        }
        for part_id in stale_parts {
            tx.execute("DELETE FROM checkpoints WHERE download_id=?1 AND part_id=?2",
                params![task.view.id, part_id]).map_err(|e| e.to_string())?;
        }
    }
    for id in existing {
        tx.execute("DELETE FROM downloads WHERE id=?1", params![id]).map_err(|e| e.to_string())?;
    }
    tx.commit().map_err(|e| format!("SQLite WAL transaction failed: {e}"))
}
pub(super) async fn load(path: PathBuf) -> Result<Option<Saved>, String> {
    tokio::task::spawn_blocking(move || load_sync(path)).await.map_err(|e| e.to_string())?
}
pub(super) async fn save(path: PathBuf, saved: Saved) -> Result<(), String> {
    tokio::task::spawn_blocking(move || save_sync(path, saved)).await.map_err(|e| e.to_string())?
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{Settings, TaskView};
    #[tokio::test]
    async fn incremental_updates_preserve_untouched_rows_and_delete_only_removed_ids() {
        let dir = std::env::temp_dir().join(format!("qdm-incremental-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state-v3.sqlite3");
        let task = |id: String, name: &str| -> Task {
            let view = TaskView { id, url: "https://example.com/test.bin".into(), filename: name.into(),
                status: "completed".into(), created_at: 123, ..TaskView::default() };
            serde_json::from_value(serde_json::json!({"view":view})).unwrap()
        };
        let first = task(uuid::Uuid::new_v4().to_string(), "first.bin");
        let second = task(uuid::Uuid::new_v4().to_string(), "second.bin");
        let third = task(uuid::Uuid::new_v4().to_string(), "third.bin");
        let mut saved = Saved { schema_version: 3,
            settings: Settings { connections: 64, ..Settings::default() },
            tasks: vec![first.clone(), second.clone(), third.clone()] };
        save(path.clone(), Saved { schema_version: saved.schema_version, settings: saved.settings.clone(), tasks: saved.tasks.clone() }).await.unwrap();
        let (initial_row, initial_settings) = {
            let db = connect(&path).unwrap();
            let original: Vec<u8> = db.query_row("SELECT task_json FROM downloads WHERE id=?1", params![first.view.id], |r| r.get(0)).unwrap();
            let settings: Vec<u8> = db.query_row("SELECT value_json FROM app_settings WHERE id=1", [], |r| r.get(0)).unwrap();
            (original, settings)
        };
        saved.tasks[1].view.filename = "second-edited.bin".into();
        saved.tasks.remove(2);
        save(path.clone(), Saved { schema_version: saved.schema_version, settings: saved.settings.clone(), tasks: saved.tasks.clone() }).await.unwrap();
        let db = connect(&path).unwrap();
        let unchanged: Vec<u8> = db.query_row("SELECT task_json FROM downloads WHERE id=?1", params![first.view.id], |r| r.get(0)).unwrap();
        let settings: Vec<u8> = db.query_row("SELECT value_json FROM app_settings WHERE id=1", [], |r| r.get(0)).unwrap();
        let count: i64 = db.query_row("SELECT COUNT(*) FROM downloads", [], |r| r.get(0)).unwrap();
        let orphan: i64 = db.query_row("SELECT COUNT(*) FROM downloads WHERE id=?1", params![third.view.id], |r| r.get(0)).unwrap();
        assert_eq!(initial_row, unchanged);
        assert_eq!(initial_settings, settings);
        assert_eq!(count, 2);
        assert_eq!(orphan, 0);
        drop(db);
        let after = load(path.clone()).await.unwrap().unwrap();
        assert_eq!(after.tasks.len(), 2);
        assert_eq!(after.tasks[1].view.filename, "second-edited.bin");
        std::fs::remove_dir_all(dir).unwrap();
    }
    #[tokio::test]
    async fn sqlite_round_trip_restores_segments_and_settings() {
        let dir = std::env::temp_dir().join(format!("qdm-sqlite-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state-v3.sqlite3");
        let view = TaskView { id: uuid::Uuid::new_v4().to_string(),
            url: "https://example.com/file".into(), status: "paused".into(),
            created_at: 1234, ..TaskView::default() };
        let task: Task = serde_json::from_value(serde_json::json!({"view":view})).unwrap();
        let mut task = task;
        task.parts = vec![Part { id: 1, start: 0, end: 4096, downloaded: 3072,
            durable: 3072, durable_tail_sha256: Some("abcd".into()), ..Part::default() }];
        let saved = Saved { schema_version: 3,
            settings: Settings { connections: 64,
                queue_speed_limits: std::collections::BTreeMap::from([("media".into(), 256)]),
                ..Settings::default() },
            tasks: vec![task] };
        save(path.clone(), saved).await.unwrap();
        let saved = load(path.clone()).await.unwrap().unwrap();
        assert_eq!(saved.settings.connections, 64);
        assert_eq!(saved.settings.queue_speed_limits.get("media"), Some(&256));
        assert_eq!(saved.tasks.len(), 1);
        assert_eq!(saved.tasks[0].parts[0].durable, 3072);
        assert_eq!(saved.tasks[0].parts[0].durable_tail_sha256.as_deref(), Some("abcd"));
        let db = connect(&path).unwrap();
        let journal: String = db.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(journal, "wal");
        drop(db);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
