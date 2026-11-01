#!/usr/bin/env python3
"""Collect actual licenses from the crates pinned in this QDM Cargo.lock."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import tomllib

parser = argparse.ArgumentParser()
parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1])
parser.add_argument('--cargo-home', type=Path, required=True)
args = parser.parse_args()
root = args.root.resolve()
lock = tomllib.loads((root / 'Cargo.lock').read_text(encoding='utf-8'))
target = root / 'docs' / 'third-party' / 'crates'
target.mkdir(parents=True, exist_ok=True)
registries = list((args.cargo_home / 'registry' / 'src').glob('*'))
entries, missing = [], []
for package in lock.get('package', []):
    if not str(package.get('source', '')).startswith('registry+'):
        continue
    name, version = package['name'], package['version']
    sources = [registry / f'{name}-{version}' for registry in registries]
    source = next((p for p in sources if p.is_dir()), None)
    if source is None:
        missing.append({'name': name, 'version': version, 'reason': 'not downloaded for this target'})
        continue
    manifest = tomllib.loads((source / 'Cargo.toml').read_text(encoding='utf-8'))
    license_expression = manifest.get('package', {}).get('license')
    declared_file = manifest.get('package', {}).get('license-file')
    selected = set()
    for item in source.iterdir():
        prefix = item.name.upper()
        if prefix.startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE', 'COPYRIGHT', 'THIRD-PARTY')):
            if item.is_file():
                selected.add(item)
            elif item.is_dir():
                selected.update(p for p in item.rglob('*') if p.is_file())
    if declared_file:
        explicit = (source / declared_file).resolve()
        if explicit.is_file() and explicit.is_relative_to(source.resolve()):
            selected.add(explicit)
    copied = []
    for item in sorted(selected):
        relative = item.relative_to(source)
        destination = target / f'{name}-{version}' / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(item, destination)
        copied.append({'path': str(destination.relative_to(root)).replace('\\', '/'),
                       'sha256': hashlib.sha256(item.read_bytes()).hexdigest()})
    entries.append({'name': name, 'version': version, 'license': license_expression,
                    'registry_checksum': package.get('checksum'), 'files': copied})
    if not copied:
        missing.append({'name': name, 'version': version, 'reason': 'crate has no discovered license text', 'license': license_expression})
report = {'cargo_lock_sha256': hashlib.sha256((root / 'Cargo.lock').read_bytes()).hexdigest(),
          'crates': entries, 'unavailable_or_missing': missing,
          'scope': 'Pinned registry crates present in the local Cargo cache; target-only crates not downloaded are listed explicitly.'}
(root / 'docs' / 'third-party' / 'dependency-licenses-0.3.0.json').write_text(json.dumps(report, indent=2)+'\n', encoding='utf-8')
print(json.dumps({'crates': len(entries), 'license_files': sum(len(p['files']) for p in entries), 'unavailable_or_missing': missing}))
