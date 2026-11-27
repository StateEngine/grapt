"""Remove workspace bookkeeping only when its resolved dependency graph is additive."""
from pathlib import Path
import subprocess
import tomllib


def scoped_paths(paths, root, base, head=None):
    if not base or not {'Cargo.toml', 'Cargo.lock'} & paths:
        return paths

    def manifest(name, revision=None):
        revision = revision or head
        raw = (subprocess.check_output(['git', 'show', f'{revision}:{name}'], cwd=root, timeout=10)
               if revision else (root/name).read_bytes())
        if len(raw) > 1024*1024:
            raise ValueError('Build policy manifest exceeds 1 MiB')
        return tomllib.loads(raw.decode())

    def records(lock):
        rows = lock.get('package', [])
        result = {(p['name'], p['version'], p.get('source')): p for p in rows}
        if len(rows) != len(result):
            raise ValueError('Duplicate lock package identity')
        return result

    def edges(package, graph):
        resolved = set()
        for dependency in package.get('dependencies', []):
            parts = dependency.split(' ', 2)
            matches = [key for key in graph if key[0] == parts[0]
                       and (len(parts) < 2 or key[1] == parts[1])
                       and (len(parts) < 3 or key[2] == parts[2].removeprefix('(').removesuffix(')'))]
            if len(matches) != 1 or matches[0] in resolved:
                raise ValueError('Ambiguous or duplicate resolved lock edge')
            resolved.add(matches[0])
        return resolved

    try:
        before, after = manifest('Cargo.toml', base), manifest('Cargo.toml')
        old_members = before.get('workspace', {}).pop('members', None)
        new_members = after.get('workspace', {}).pop('members', None)
        old_deps = before.get('workspace', {}).pop('dependencies', {})
        new_deps = after.get('workspace', {}).pop('dependencies', {})
        if (not isinstance(old_members, list) or not isinstance(new_members, list) or before != after or
                not set(old_members) <= set(new_members) or
                any(new_deps.get(name) != value for name, value in old_deps.items())):
            return paths
        result = paths - {'Cargo.toml'}
        if 'Cargo.lock' not in result:
            return result
        old_lock, new_lock = manifest('Cargo.lock', base), manifest('Cargo.lock')
        old, new = records(old_lock), records(new_lock)
        consumers = {manifest(member+'/Cargo.toml')['package']['name']: member+'/Cargo.toml'
                     for member in old_members}
        for key, value in old.items():
            current = new.get(key)
            if current is None or ({k:v for k,v in value.items() if k != 'dependencies'} !=
                                   {k:v for k,v in current.items() if k != 'dependencies'}):
                return paths
            prior, revised = edges(value, old), edges(current, new)
            if prior == revised:
                continue
            if key[2] is not None or consumers.get(key[0]) not in paths or not prior < revised:
                return paths
        if ({k:v for k,v in old_lock.items() if k != 'package'} !=
                {k:v for k,v in new_lock.items() if k != 'package'}):
            return paths
        return result - {'Cargo.lock'}
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError):
        return paths
