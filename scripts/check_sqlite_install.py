import json
import sqlite3
import sys
from pathlib import Path

if len(sys.argv) != 2:
    raise SystemExit("Usage: check_sqlite_install.py <state-v3.sqlite3>")

# Read only, never repair or mutate the user's production database.
with sqlite3.connect(Path(sys.argv[1]).resolve().as_uri() + "?mode=ro", uri=True, timeout=5) as db:
    connection_limit = db.execute("SELECT value_json FROM app_settings WHERE id=1").fetchone()
    if connection_limit is None:
        raise SystemExit("QDM SQLite app_settings row missing")
    settings = json.loads(connection_limit[0])
    rows = db.execute("SELECT COUNT(*) FROM downloads").fetchone()[0]
    checkpoints = db.execute("SELECT COUNT(*) FROM checkpoints").fetchone()[0]
    journal = db.execute("PRAGMA journal_mode").fetchone()[0]
    integrity = db.execute("PRAGMA integrity_check").fetchone()[0]
    if integrity != "ok":
        raise SystemExit(f"SQLite integrity_check failed: {integrity}")
    print(json.dumps({
        "downloads": rows,
        "checkpoints": checkpoints,
        "settings": settings,
        "journal": journal,
        "integrity": integrity
    }, ensure_ascii=False))
