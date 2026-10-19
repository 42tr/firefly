#!/usr/bin/env python3
"""Collect shipped license/notice texts for locked Rust and npm dependencies.

Run after npm ci / cargo fetch. The release workflow regenerates the inventory
before packaging (the output file is gitignored, never committed); --check
compares a fresh render against the local output file. Includes development and
platform-specific dependencies for a conservative, platform-independent
inventory. No dependency source code is copied.
"""
import argparse
import hashlib
import json
from pathlib import Path
import re
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / 'THIRD_PARTY_LICENSES.md'
LICENSE_NAME = re.compile(r'^(licen[cs]e|copying|notice|copyright)(?:[._-].*)?$', re.I)


def texts(directory, declared=None):
    candidates = {p for p in directory.rglob('*') if p.is_file() and LICENSE_NAME.match(p.name)
                  and 'node_modules' not in p.relative_to(directory).parts}
    if declared:
        candidates.add(directory / declared)
    result = []
    for p in sorted(candidates):
        if p.suffix.lower() in {'.json', '.rs', '.c', '.h', '.go', '.py'}:
            continue
        raw = p.read_bytes()
        if b'\0' in raw or not raw.strip():
            continue
        value = raw.decode('utf-8', errors='replace').replace('\r\n', '\n').strip()
        result.append((p.relative_to(directory).as_posix(), value))
    return result


def render():
    packages = []
    metadata = json.loads(subprocess.check_output(
        ['cargo', 'metadata', '--locked', '--format-version', '1'], cwd=ROOT))
    for p in metadata['packages']:
        if p['source'] is None:
            continue  # Workspace code is covered by the project license.
        packages.append(('Rust', p['name'], p['version'], p['license'] or 'See bundled texts',
                         f"https://crates.io/crates/{p['name']}/{p['version']}",
                         texts(Path(p['manifest_path']).parent, p['license_file'])))
        if p['name'] == 'libsqlite3-sys':
            header = (Path(p['manifest_path']).parent / 'sqlite3/sqlite3.h').read_text()
            version = re.search(r'#define SQLITE_VERSION\s+"([^"]+)"', header).group(1)
            notice = next(body for body in re.findall(r'/\*.*?\*/', header, re.S)
                          if 'The author disclaims copyright' in body)
            packages.append(('Asset', 'SQLite', version, 'Public Domain',
                             'https://www.sqlite.org/copyright.html', [('SQLite source notice', notice)]))
    lock = json.loads((ROOT / 'package-lock.json').read_text())
    for location, p in lock['packages'].items():
        if not location:
            continue
        directory = ROOT / location
        info = (json.loads((directory / 'package.json').read_text()) if directory.is_dir()
                else {'name': location.rsplit('node_modules/', 1)[1], 'version': p['version']})
        name, version = info['name'], info['version']
        license_id = info.get('license', p.get('license', 'See bundled texts'))
        if not isinstance(license_id, str):
            license_id = json.dumps(license_id, sort_keys=True)
        packages.append(('npm', name, version, license_id,
                         f'https://www.npmjs.com/package/{name}/v/{version}', texts(directory)))
    for name, license_id, filename in [('Feather icons', 'MIT', 'LICENSE'), ('Lucide icons', 'ISC AND MIT', 'LICENSE-lucide')]:
        body = (ROOT / 'frontend/src/assets/icons' / filename).read_text().strip()
        packages.append(('Asset', name, '', license_id,
                         'https://github.com/' + ('feathericons/feather' if filename == 'LICENSE' else 'lucide-icons/lucide'),
                         [(filename, body)]))
    packages.sort(key=lambda p: (p[0], p[1], p[2]))
    bodies, lines, missing = {}, [], []
    for ecosystem, name, version, license_id, url, files in packages:
        links = []
        for filename, body in files:
            digest = hashlib.sha256(body.encode()).hexdigest()[:16]
            bodies[digest] = body
            links.append(f'[{filename}](#license-{digest})')
        if not links:
            # Some registry packages declare SPDX terms but omit license files.
            # Checked-in canonical terms keep regeneration offline and stable.
            ids = re.findall(r'[A-Za-z0-9][A-Za-z0-9.+-]*', license_id)
            for identifier in ids:
                canonical = ROOT / 'scripts' / 'license-texts' / (identifier + '.txt')
                if not canonical.is_file():
                    continue
                body = canonical.read_text().strip()
                digest = hashlib.sha256(body.encode()).hexdigest()[:16]
                bodies[digest] = body
                links.append(f'[{identifier} terms](#license-{digest})')
        if not links:
            missing.append(f'{ecosystem} {name} {version}: {license_id}')
        lines.append(f'| {ecosystem} | [{name} {version}]({url}) | {license_id.replace("|", "/")} | {", ".join(links)} |')
    if missing:
        raise RuntimeError('Missing license texts (add upstream terms to scripts/license-texts):\n' + '\n'.join(missing))
    output = ['# Third-party dependency licenses', '',
              'Generated by `python3 scripts/generate_third_party_licenses.py` from the locked dependency set.',
              'Includes build, test and platform-specific dependencies. License and notice files are preserved verbatim;',
              'identical texts are shared between inventory entries. For packages without shipped texts, canonical',
              'SPDX terms are included. Source links identify the upstream package and version.', '',
              '| Ecosystem | Package | Declared license | License / notice texts |',
              '| --- | --- | --- | --- |', *lines, '']
    for digest, body in sorted(bodies.items()):
        output.extend([f'<a id="license-{digest}"></a>', '', f'## License {digest}', '',
                       '<pre>', body.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;'), '</pre>', ''])
    return '\n'.join(output)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args()
    result = render()
    if args.check:
        if not OUTPUT.exists() or OUTPUT.read_text() != result:
            sys.exit('Third-party licenses are stale; run scripts/generate_third_party_licenses.py')
        print('Third-party licenses match the locked dependencies')
    else:
        OUTPUT.write_text(result)
        print(f'Updated {OUTPUT.name}')


if __name__ == '__main__':
    main()
