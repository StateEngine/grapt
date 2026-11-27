#!/usr/bin/env python3
"""Check dependency licenses and generate the complete release license register."""
import argparse
import hashlib
import html
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import tomllib

ROOT = Path(__file__).resolve().parents[2]
NOTICE = Path('licenses/dependencies/NOTICE.html')
MAX_NOTICE = 1024 * 1024
ADVISORY_FETCH_ATTEMPTS = 3
ADVISORY_FETCH_BACKOFF = (3, 9)
ADVISORY_CACHE_MAX_AGE = 24 * 60 * 60
ADVISORY_FETCH_STAMP = 'secode-advisory-fetch.stamp'
ADVISORY_REFETCH_FAILURE = 'failed to fetch advisory database'


def read_toml(path):
    return tomllib.loads(path.read_text(encoding='utf-8'))


def shipped(manifest, workspace, directory=None, root=None):
    """A member's shipped dependency edges and normalized external declarations.

    Normal and build dependencies count, for any target. `Cargo.lock` records
    every optional edge, so local path targets are resolved to their exact
    `(name, version)` to distinguish normal vs dev-only versions of the same
    vendored crate.
    """
    tables = {'dependencies': manifest.get('dependencies', {}),
              'build-dependencies': manifest.get('build-dependencies', {})}
    for target, table in manifest.get('target', {}).items():
        for kind in ('dependencies', 'build-dependencies'):
            tables[f'{target}.{kind}'] = table.get(kind, {})
    edges, declared = [], {}
    for kind, table in tables.items():
        for key, raw_spec in table.items():
            if isinstance(raw_spec, dict) and raw_spec.get('workspace') is True:
                inherited = workspace.get('dependencies', {}).get(key, {})
                if not isinstance(inherited, dict):
                    inherited = {'version': inherited}
                effective_name = inherited.get('package', key)
                raw_path = inherited.get('path')
                base_dir = root
                spec = {'member': raw_spec, 'workspace': inherited}
            else:
                effective_name = raw_spec.get('package', key) if isinstance(raw_spec, dict) else key
                raw_path = raw_spec.get('path') if isinstance(raw_spec, dict) else None
                base_dir = directory
                spec = raw_spec
            local_version = None
            rel_path = None
            if raw_path and base_dir is not None and root is not None:
                target_dir = (base_dir / raw_path).resolve()
                target_manifest = target_dir / 'Cargo.toml'
                if target_manifest.is_file():
                    local_version = read_toml(target_manifest)['package']['version']
                    try:
                        rel_path = target_dir.relative_to(root.resolve()).as_posix()
                    except ValueError:
                        rel_path = target_dir.as_posix()
            edges.append((effective_name, local_version, rel_path, spec))
            declared.setdefault(kind, {})[key] = spec
    return edges, {'dependencies': declared, 'features': manifest.get('features', {})}


def notice_graph(root):
    """Locked packages the register covers: reachable from a member without dev edges.

    `about.toml` ignores dev-dependencies, and `Cargo.lock` holds both kinds, so
    a member's own manifest decides which of its locked edges count. Returns the
    reached packages and the deduplicated external/vendored non-dev specs
    (never internal workspace crates or internal edges).
    """
    locked = read_toml(root / 'Cargo.lock')['package']
    if len(locked) > 16384:
        raise ValueError('Dependency license graph exceeds 16384 packages')
    named = {}
    for package in locked:
        named.setdefault(package['name'], []).append(package)

    def resolve(reference):
        name, _, rest = reference.partition(' ')
        version, _, source = rest.partition(' ')
        found = [package for package in named.get(name, [])
                 if (not version or package['version'] == version)
                 and (not source or package.get('source') == source.strip('()'))]
        if len(found) != 1:
            raise ValueError(f'Cargo.lock does not resolve dependency {reference!r}')
        return found[0]

    workspace = read_toml(root / 'Cargo.toml')['workspace']
    excluded = {(root / path).resolve() for path in workspace.get('exclude', [])}
    members = {}
    for pattern in workspace.get('members', []):
        for directory in sorted(root.glob(pattern), key=lambda p: p.as_posix()):
            if directory.resolve() in excluded or not (directory / 'Cargo.toml').is_file():
                continue
            manifest = read_toml(directory / 'Cargo.toml')
            package = resolve(manifest['package']['name'])
            if package.get('source'):
                raise ValueError(f'Workspace member {package["name"]} is locked with a source')
            members[package['name']], _ = shipped(manifest, workspace, directory, root)
    if not members:
        raise ValueError('The workspace declares no members')

    member_names = set(members)
    member_local_edges = {}
    member_nonlocal_names = {}
    external_specs = []
    for name, edges in members.items():
        local_set = set()
        nonlocal_set = set()
        for dep_name, local_version, rel_path, spec in edges:
            if dep_name in member_names:
                nonlocal_set.add(dep_name)
                continue
            if local_version is not None:
                local_set.add((dep_name, local_version))
                external_specs.append({
                    'name': dep_name,
                    'version': local_version,
                    'path': rel_path,
                    'spec': spec,
                })
            else:
                nonlocal_set.add(dep_name)
                external_specs.append({'name': dep_name, 'spec': spec})
        member_local_edges[name] = local_set
        member_nonlocal_names[name] = nonlocal_set

    reached, pending, seen = [], [resolve(name) for name in sorted(members)], set()
    while pending:
        package = pending.pop()
        key = (package['name'], package['version'], package.get('source'))
        if key in seen:
            continue
        seen.add(key)
        member = not package.get('source') and package['name'] in member_names
        if not member:
            reached.append(package)
        for reference in package.get('dependencies', []):
            dependency = resolve(reference)
            # Non-member lock entries never record dev edges.
            if not member:
                pending.append(dependency)
            elif not dependency.get('source') and dependency['name'] not in member_names:
                # Vendored local dependency: match exact (name, version) from normal/build path.
                if (dependency['name'], dependency['version']) in member_local_edges[package['name']]:
                    pending.append(dependency)
            elif dependency['name'] in member_nonlocal_names[package['name']]:
                pending.append(dependency)
    dedup_specs = sorted({json.dumps(item, sort_keys=True) for item in external_specs})
    return reached, dedup_specs


def dependency_digest(root):
    # Registry checksums and git revisions pin the upstream license files too.
    # Hash only external Cargo.lock dependencies, vendored third-party crates,
    # non-dev external dependency specs, and license configuration — never
    # internal workspace crates or internal workspace edges.
    reached, external_specs = notice_graph(root)
    external = [{key: package[key] for key in ('name', 'version', 'source', 'checksum')
                 if key in package} for package in reached if package.get('source')]
    inputs = {Path(p) for p in ['about.toml',
                               'scripts/release/license-overrides.toml']}
    local = {(package['name'], package['version']) for package in reached
             if not package.get('source')}
    for manifest in sorted(root.glob('third-party/*/Cargo.toml'), key=lambda p: p.as_posix()):
        vendored = read_toml(manifest)
        package = vendored['package']
        if (package['name'], package['version']) not in local:
            continue
        local.discard((package['name'], package['version']))
        entry = {key: package[key] for key in ('name', 'version', 'license', 'license-file')
                 if key in package}
        entry['path'] = manifest.parent.relative_to(root).as_posix()
        # Its own declarations and features select among its locked optional
        # edges, as a member's do; no registry checksum pins them.
        entry['declarations'] = shipped(vendored, {}, manifest.parent, root)[1]
        external.append(entry)
        if package.get('license-file'):
            inputs.add((manifest.parent / package['license-file']).relative_to(root))
        for path in manifest.parent.rglob('*'):
            if path.is_file() and any(part.upper().startswith(('LICENSE', 'LICENCE', 'COPYING', 'NOTICE'))
                                      for part in path.relative_to(manifest.parent).parts):
                inputs.add(path.relative_to(root))
    if local:
        raise ValueError(f'Shipped local packages outside the workspace and third-party/: {sorted(local)}')
    overrides = tomllib.loads((root / 'scripts/release/license-overrides.toml').read_text(encoding='utf-8'))
    inputs.update(Path(p['text']) for p in overrides['package'])
    if len(inputs) > 256:
        raise ValueError('Dependency license inputs exceed 256 files')
    digest = hashlib.sha256()
    packages = sorted(external, key=lambda package: json.dumps(package, sort_keys=True))
    digest.update(b'packages\0' + json.dumps(packages, sort_keys=True).encode() + b'\0')
    digest.update(b'external-specs\0' + json.dumps(external_specs, sort_keys=True).encode() + b'\0')
    for path in sorted(inputs, key=lambda path: path.as_posix()):
        # Git may check out these text inputs with CRLF on Windows.
        contents = (root / path).read_bytes().replace(b'\r\n', b'\n')
        digest.update(path.as_posix().encode() + b'\0' + contents + b'\0')
    return digest.hexdigest()


def third_party(crate, root):
    return bool(crate['source']) or not Path(crate['manifest_path']).is_relative_to(root / 'crates')


def render(document, root):
    overrides = tomllib.loads((root / 'scripts/release/license-overrides.toml').read_text(encoding='utf-8'))
    overrides = {(p['name'], p['version']): p for p in overrides['package']}
    groups = {}
    covered = set()
    for license in document['licenses']:
        for entry in license['used_by']:
            crate = entry['crate']
            if not third_party(crate, root):  # Grapt's own licenses ship separately.
                continue
            name = (crate['name'], crate['version'])
            text, source = license['text'], ''
            if license['id'] == 'MIT' and '<year>' in text:
                replacement = overrides.get(name)
                if replacement is None or replacement['license'] != license['id']:
                    raise ValueError(f'Missing published copyright notice for {name}')
                text = (root / replacement['text']).read_text(encoding='utf-8')
                source = replacement['source']
                if '<year>' in text or 'Permission is hereby granted' not in text:
                    raise ValueError(f'Incomplete MIT license for {name}')
            if not text.strip():
                raise ValueError(f'Empty license text for {name}')
            groups.setdefault((license['id'], text), set()).add((*name, source))
            covered.add(crate['id'])
    expected = {c['package']['id'] for c in document['crates'] if third_party(c['package'], root)}
    if not expected or expected != covered:
        raise ValueError('Dependency license register does not cover the resolved graph')
    escape = html.escape
    sections = [
        '<!doctype html><html lang="en"><meta charset="utf-8">',
        '<title>Grapt dependency licenses</title><h1>Dependency licenses</h1>',
        f'<!-- Dependency inputs SHA-256: {dependency_digest(root)} -->',
        '<p>All platforms and features; includes build dependencies. The separately licensed',
        'StateEngine component is excluded; its license and notices remain under stateengine/. Source adaptations',
        'and bundled runtime licenses are recorded separately in THIRD_PARTY_NOTICES.md.</p>',
    ]
    for (identifier, text), crates in sorted(groups.items()):
        sections.append(f'<section><h2>{escape(identifier)}</h2><ul>')
        for name, version, source in sorted(crates):
            link = f' <a href="{escape(source, quote=True)}">published license</a>' if source else ''
            sections.append(f'<li>{escape(name)} {escape(version)}{link}</li>')
        sections.append(f'</ul><pre>{escape(text)}</pre></section>')
    result = '\n'.join(sections) + '\n'
    if len(result.encode()) > MAX_NOTICE:
        raise ValueError('Dependency license register exceeds the package bound')
    return result


def verify(root):
    path = root / NOTICE
    if path.stat().st_size > MAX_NOTICE:
        raise ValueError('Dependency license register exceeds the package bound')
    if f'<!-- Dependency inputs SHA-256: {dependency_digest(root)} -->' not in path.read_text(encoding='utf-8'):
        raise ValueError('Dependency licenses are stale; run scripts/release/licenses.py generate')


def advisory_databases(cargo_home=None):
    return Path(cargo_home or os.environ.get('CARGO_HOME') or Path.home() / '.cargo') / 'advisory-dbs'


def advisory_cache_age(cargo_home=None, now=None):
    """Seconds since this gate last fetched the advisory databases, or None.

    Only the stamp written after a successful fetch counts. Git's FETCH_HEAD
    cannot: a failed fetch truncates it and moves its mtime to the failure.
    """
    databases = advisory_databases(cargo_home)
    stamp = databases / ADVISORY_FETCH_STAMP
    if not stamp.is_file() or not any(databases.glob('*/.git')):
        return None
    return max(0.0, (time.time() if now is None else now) - stamp.stat().st_mtime)


def fetch_advisories():
    """Fetch the advisory database, or fall back to a cache at most 24 h old.

    Advisory content stays fail-closed: a network blip may only reuse a
    recent database, never skip the check. Returns extra cargo-deny flags.
    """
    for attempt in range(ADVISORY_FETCH_ATTEMPTS):
        try:
            subprocess.run(['cargo-deny', 'fetch', 'db'], cwd=ROOT, check=True, timeout=300)
            databases = advisory_databases()
            databases.mkdir(parents=True, exist_ok=True)
            (databases / ADVISORY_FETCH_STAMP).write_text(time.strftime('%Y-%m-%dT%H:%M:%SZ\n', time.gmtime()))
            return []
        except (subprocess.CalledProcessError, subprocess.TimeoutExpired) as error:
            failure = error
            if attempt < len(ADVISORY_FETCH_BACKOFF):
                time.sleep(ADVISORY_FETCH_BACKOFF[attempt])
    age = advisory_cache_age()
    if age is None or age > ADVISORY_CACHE_MAX_AGE:
        held = 'no cached advisory database' if age is None else f'the cached advisory database is {age / 3600:.1f} h old'
        raise ValueError(
            f'advisory fetch failure (network, not code): {ADVISORY_FETCH_ATTEMPTS} fetch attempts '
            f'failed and {held}; the advisory gate needs a database at most 24 h old') from failure
    print(f'advisory fetch failed {ADVISORY_FETCH_ATTEMPTS} times; checking against the cached '
          f'advisory database, last fetched {age / 3600:.1f} h ago', flush=True)
    return ['--offline']


def deny_check(offline):
    """Run the dependency checks; a refetch blip inside them reruns offline.

    cargo-deny refetches the advisory database during an online check. When
    that refetch alone fails, the database fetched moments ago is current, so
    the same checks rerun offline. Any other failure, including an advisory
    or license finding, still fails.
    """
    command = ['cargo-deny', '--locked', *offline, '--all-features', 'check',
               'licenses', 'bans', 'sources', 'advisories']
    result = subprocess.run(command, cwd=ROOT, stderr=subprocess.PIPE, text=True, timeout=300)
    sys.stderr.write(result.stderr)
    if result.returncode == 0:
        return
    if offline or ADVISORY_REFETCH_FAILURE not in result.stderr:
        raise subprocess.CalledProcessError(result.returncode, command, stderr=result.stderr)
    print('advisory refetch inside the check failed; rerunning the checks offline against '
          'the database fetched for this run', flush=True)
    subprocess.run(['cargo-deny', '--locked', '--offline', *command[2:]], cwd=ROOT, check=True, timeout=300)


def main():
    import notices  # A sibling of this script, not of importers that load it by path.
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('mode', choices=['check', 'generate'])
    args = parser.parse_args()
    for command, version in [('cargo-deny', '0.20.2'), ('cargo-about', '0.9.2')]:
        if command == 'cargo-about' and args.mode == 'check':
            continue
        actual = subprocess.check_output([command, '--version'], text=True, timeout=15).strip()
        if actual != f'{command} {version}':
            raise ValueError(f'Install {command} {version}; found {actual}')
    # The advisory gate fails closed on content: an unreachable database may
    # reuse a recent cache, but never degrades into a silent pass or a hang.
    offline = fetch_advisories()
    deny_check(offline)
    notices.assemble(ROOT)  # Both modes refuse unusable third-party notice fragments.
    if args.mode == 'check':
        verify(ROOT)
        return
    with tempfile.TemporaryDirectory(prefix='grapt-licenses-') as temporary:
        metadata = Path(temporary) / 'about.json'
        subprocess.run(['cargo-about', 'generate', '--locked', '--workspace', '--all-features',
                        '--fail', '--config', 'about.toml', '--format', 'json',
                        '--output-file', str(metadata)], cwd=ROOT, check=True, timeout=300)
        if metadata.stat().st_size > 16 * MAX_NOTICE:
            raise ValueError('Dependency license metadata exceeds 16 MiB')
        contents = render(json.loads(metadata.read_text(encoding='utf-8')), ROOT)
    destination = ROOT / NOTICE
    destination.parent.mkdir(parents=True, exist_ok=True)
    pending = tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=destination.parent,
                                          delete=False)
    temporary = Path(pending.name)
    try:
        with pending:
            pending.write(contents)
        temporary.replace(destination)
    finally:
        temporary.unlink(missing_ok=True)


if __name__ == '__main__':
    main()
