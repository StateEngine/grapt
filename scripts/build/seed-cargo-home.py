#!/usr/bin/env python3
"""Seed a separate NVMe Cargo home from verified public cached dependencies."""
from operator_paths import cache_root
import argparse
import fcntl
import hashlib
from itertools import islice
import os
from pathlib import Path
import tempfile
import time
import tomllib

PUBLIC = 'registry+https://github.com/rust-lang/crates.io-index'


def seed(lock, source, destination):
    if source.resolve() == destination.resolve():
        raise ValueError('Preserve the original Cargo home; choose a separate destination')
    if lock.stat().st_size > 2 * 1024**2:
        raise ValueError('Cargo lockfile exceeds its bound')
    packages = tomllib.loads(lock.read_text()).get('package', [])
    if len(packages) > 4096:
        raise ValueError('Dependency inventory exceeds its bound')
    selected = [row for row in packages if row.get('source') == PUBLIC]
    registry = source / 'registry'
    deadline = time.monotonic() + 900
    copied = 0
    roots = {}
    for kind in ('cache', 'index'):
        with os.scandir(registry / kind) as entries:
            roots[kind] = list(islice(entries, 33))
        if len(roots[kind]) > 32 or any(not row.is_dir(follow_symlinks=False) for row in roots[kind]):
            raise ValueError('Unexpected public registry cache layout')
        roots[kind] = [row for row in roots[kind] if row.name.startswith('index.crates.io-')
                       and len(row.name.removeprefix('index.crates.io-')) == 16
                       and all(char in '0123456789abcdef' for char in row.name.removeprefix('index.crates.io-'))]
    destination.mkdir(parents=True, exist_ok=True)
    def copy(path, output, checksum=None):
        nonlocal copied
        if path.is_symlink() or not path.is_file() or path.stat().st_size > 512 * 1024**2:
            raise ValueError('Registry input is linked, absent, or oversized')
        output.parent.mkdir(parents=True, exist_ok=True)
        with tempfile.NamedTemporaryFile(dir=output.parent, delete=False) as stream:
            staged = Path(stream.name)
            try:
                with path.open('rb') as reader:
                    remaining = path.stat().st_size
                    while chunk := reader.read(min(1024**2, remaining + 1)):
                        remaining -= len(chunk)
                        copied += len(chunk)
                        if remaining < 0 or copied > 8 * 1024**3 or time.monotonic() > deadline:
                            raise ValueError('Registry seed exceeded its copy or time bound')
                        stream.write(chunk)
                    if remaining:
                        raise ValueError('Registry input was truncated while copying')
                stream.flush()
                if checksum:
                    with staged.open('rb') as reader:
                        if hashlib.file_digest(reader, 'sha256').hexdigest() != checksum:
                            raise ValueError('Cached crate differs from Cargo.lock')
                os.replace(staged, output)
            finally:
                staged.unlink(missing_ok=True)
    # These are Cargo's cache locks, kept at the original home. Neither home
    # configuration nor credentials are read, copied, moved or replaced.
    locks = []
    try:
        for name in ('.package-cache', '.package-cache-mutate'):
            handle = (source / name).open('a')
            locks.append(handle)
            fcntl.flock(handle, fcntl.LOCK_SH | fcntl.LOCK_NB)
        for row in roots['index']:
            path = Path(row.path) / 'config.json'
            if path.is_file():
                copy(path, destination / 'registry/index' / row.name / 'config.json')
        for package in selected:
            name = package['name']
            if not name or any(char not in 'abcdefghijklmnopqrstuvwxyz0123456789_-' for char in name):
                raise ValueError('Unexpected registry package name')
            index = (f'1/{name}' if len(name) == 1 else f'2/{name}' if len(name) == 2 else
                     f'3/{name[0]}/{name}' if len(name) == 3 else f'{name[:2]}/{name[2:4]}/{name}')
            for row in roots['index']:
                path = Path(row.path) / '.cache' / index
                if path.is_file():
                    copy(path, destination / 'registry/index' / row.name / '.cache' / index)
            archive = f"{name}-{package['version']}.crate"
            for row in roots['cache']:
                path = Path(row.path) / archive
                if path.is_file():
                    copy(path, destination / 'registry/cache' / row.name / archive, package['checksum'])
        print(f'Public cached dependencies seeded at {destination}; missing entries remain ordinary Cargo downloads.')
    finally:
        for handle in locks:
            handle.close()


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--lockfile', type=Path, default=Path('Cargo.lock'))
    parser.add_argument('--source', type=Path, default=Path.home() / '.cargo')
    parser.add_argument('--destination', type=Path, default=cache_root()/'cargo-home')
    args = parser.parse_args()
    seed(args.lockfile, args.source, args.destination)
