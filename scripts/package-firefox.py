"""Package the complete local Firefox/Zen source without changing a browser profile."""
import hashlib
import json
import pathlib
import zipfile

root = pathlib.Path(__file__).resolve().parents[1]
source = root / "extensions" / "firefox"
manifest = json.loads((source / "manifest.json").read_text(encoding="utf-8-sig"))
assert manifest["version"] == "0.3.1"
assert manifest["browser_specific_settings"]["gecko"]["id"] == "qdm@queen.local"
assert set(manifest["permissions"]) == {"downloads", "contextMenus", "storage", "nativeMessaging", "http://127.0.0.1/*"}
release = root / "release"
release.mkdir(exist_ok=True)
output = release / ("QDM-Zen-AutoCapture-" + manifest["version"] + ".xpi")
entries = []
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for file in sorted(source.rglob("*")):
        if not file.is_file():
            continue
        if file.is_symlink():
            raise RuntimeError(f"Unexpected symbolic link: {file}")
        relative = file.relative_to(source).as_posix()
        if any(part.startswith(".") for part in pathlib.PurePosixPath(relative).parts):
            continue
        data = file.read_bytes()
        info = zipfile.ZipInfo(relative, date_time=(1980, 1, 1, 0, 0, 0))
        info.compress_type = zipfile.ZIP_DEFLATED
        archive.writestr(info, data, compresslevel=9)
        entries.append({"name": relative, "bytes": len(data), "sha256": hashlib.sha256(data).hexdigest()})
with zipfile.ZipFile(output) as archive:
    assert archive.testzip() is None
    assert json.loads(archive.read("manifest.json")) == manifest
    for entry in entries:
        assert hashlib.sha256(archive.read(entry["name"])).hexdigest() == entry["sha256"]
record = {
    "status": "passed", "version": manifest["version"], "extensionId": "qdm@queen.local",
    "path": str(output), "bytes": output.stat().st_size,
    "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "files": entries,
    "scope": "Package content and CRC/SHA verification only; installed activation is verified separately.",
}
evidence = root / "docs" / "evidence" / "slint-idm-20261027"
evidence.mkdir(parents=True, exist_ok=True)
(evidence / "extension-package.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
print(json.dumps({key: value for key, value in record.items() if key != "files"}))
