"""Bounded local input extraction and package readback, before registry access."""
import hashlib
import json
from pathlib import Path, PurePosixPath
import stat
import zipfile


def fingerprint(path):
    digest = hashlib.sha256()
    count = 0
    with path.open('rb') as reader:
        while chunk := reader.read(1024 * 1024):
            count += len(chunk)
            if count > 2 * 1024**3:
                raise ValueError('Input exceeds two GiB')
            digest.update(chunk)
    return dict(sha256=digest.hexdigest(), bytes=count)


def extract(archive, destination, *, max_expanded=2 * 1024**3):
    # Reuses C10 native_package.extract's path/count/expanded-byte contract.
    with zipfile.ZipFile(archive) as source:
        rows = source.infolist()
        expanded = sum(row.file_size for row in rows)
        if len(rows) > 20000 or expanded > max_expanded:
            raise ValueError('Input archive exceeds its inventory bound')
        names = set()
        for row in rows:
            name = PurePosixPath(row.filename)
            if (row.orig_filename != row.filename or name.is_absolute()
                    or name.as_posix() != row.filename or ':' in row.filename
                    or '\\' in row.filename or '..' in name.parts or len(name.parts) > 24
                    or row.filename.casefold() in names or row.file_size > 160 * 1024**2
                    or stat.S_ISLNK(row.external_attr >> 16) or row.is_dir()):
                raise ValueError('Unsafe archive input')
            names.add(row.filename.casefold())
        destination.mkdir()
        for row in rows:
            output = destination.joinpath(*PurePosixPath(row.filename).parts)
            output.parent.mkdir(parents=True, exist_ok=True)
            with source.open(row) as reader, output.open('xb') as writer:
                remaining = row.file_size
                while chunk := reader.read(min(1024**2, remaining + 1)):
                    remaining -= len(chunk)
                    if remaining < 0:
                        raise ValueError('Expanded input exceeds its attested size')
                    writer.write(chunk)
                if remaining:
                    raise ValueError('Input is truncated')

    return expanded


def verify_package(root, expected):
    path = root / 'release-manifest.json'
    if path.stat().st_size > 4 * 1024**2:
        raise ValueError('Package manifest exceeds its bound')
    manifest = json.loads(path.read_bytes())
    if (manifest['engine_version'] != expected or manifest['target'] != 'x86_64-pc-windows-msvc'
            or not manifest['assembly_provenance']['clean']
            or manifest['stateengine']['status'] != 'included'):
        raise ValueError('Input package is not the expected clean bundled Windows build')
    rows = dict(manifest['binaries'], **manifest['stateengine']['assets'])
    rows.update(manifest.get('plugin_runtime', {}).get('assets', {}))
    for name, row in rows.items():
        relative = PurePosixPath(name)
        if relative.is_absolute() or '..' in relative.parts or '\\' in name or ':' in name:
            raise ValueError('Unsafe manifest member')
        actual = fingerprint(root.joinpath(*relative.parts))
        if actual != {key: row[key] for key in ('sha256', 'bytes')}:
            raise ValueError('Packaged bytes differ from the manifest: ' + name)
    return manifest
