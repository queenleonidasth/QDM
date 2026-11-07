"""Package the Chromium/Edge extension folder for loading unpacked or as a ZIP."""
import hashlib
import json
import pathlib
import zipfile

root = pathlib.Path(__file__).resolve().parents[1]
source = root / "extensions" / "chromium"
manifest = json.loads((source / "manifest.json").read_text(encoding="utf-8-sig"))
assert manifest["version"] == "0.4.1"
assert manifest["manifest_version"] == 3
assert "cookies" in manifest["permissions"]
assert "<all_urls>" in manifest["host_permissions"]
assert manifest["background"]["service_worker"] == "background.js"
assert (source / "session.js").is_file()
release = root / "release"
release.mkdir(exist_ok=True)
output = release / ("QDM-Chromium-Extension-" + manifest["version"] + ".zip")
entries = []
with zipfile.ZipFile(output, "w", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as archive:
    for file in sorted(source.rglob("*")):
        if not file.is_file() or file.is_symlink():
            continue
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
    assert archive.read("session.js") == (source / "session.js").read_bytes()
    for entry in entries:
        assert hashlib.sha256(archive.read(entry["name"])).hexdigest() == entry["sha256"]
record = {
    "status": "passed", "version": manifest["version"], "browser": "chromium",
    "path": str(output), "bytes": output.stat().st_size,
    "sha256": hashlib.sha256(output.read_bytes()).hexdigest(), "files": entries,
    "scope": "Package content and CRC/SHA verification only; installed activation is verified separately.",
}
evidence = root / "docs" / "evidence" / "slint-idm-20261027"
evidence.mkdir(parents=True, exist_ok=True)
(evidence / "extension-package-chromium.json").write_text(json.dumps(record, indent=2) + "\n", encoding="utf-8")
print(json.dumps({key: value for key, value in record.items() if key != "files"}))
