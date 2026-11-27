#!/usr/bin/env python3
"""Assemble THIRD_PARTY_NOTICES.md from the plain fragments under notices/.

Fragments are concatenated byte for byte in sorted relative-path order, so each
upstream adaptation adds one file and nobody edits or commits the assembled text.
The generated THIRD_PARTY_NOTICES.md is untracked; check and packaging call
assemble() and never trust a stale copy.
"""
import os
from pathlib import Path
import stat
import sys

ROOT = Path(__file__).resolve().parents[2]
FRAGMENTS = Path('notices')
MAX_ENTRIES = 1024  # Files and directories together, so empty directories are bounded too.
MAX_FRAGMENT = 256 * 1024
MAX_NOTICES = 1024 * 1024


def assemble(root):
    base = root / FRAGMENTS
    if not stat.S_ISDIR(base.lstat().st_mode):
        raise ValueError(f'{FRAGMENTS} is not a plain directory')
    names, pending = [], [base]
    while pending:
        with os.scandir(pending.pop()) as entries:
            for entry in entries:
                path = Path(entry.path)
                names.append(path)
                if len(names) > MAX_ENTRIES:
                    raise ValueError(f'Notice fragments exceed {MAX_ENTRIES} entries')
                mode = path.lstat().st_mode
                if stat.S_ISDIR(mode):
                    pending.append(path)
                elif not stat.S_ISREG(mode) or path.suffix != '.md':
                    raise ValueError(f'Notice fragment must be a regular .md file: {entry.name}')
    parts, total = [], 0
    for path in sorted(names, key=lambda path: path.relative_to(base).as_posix()):
        if path.is_dir():
            continue
        name = path.relative_to(base).as_posix()
        with path.open('rb') as stream:
            contents = stream.read(MAX_FRAGMENT + 1)
        if not 0 < len(contents) <= MAX_FRAGMENT:
            raise ValueError(f'Notice fragment is empty or exceeds {MAX_FRAGMENT} bytes: {name}')
        total += len(contents)
        if total > MAX_NOTICES:
            raise ValueError('Assembled notices exceed the package bound')
        try:
            contents.decode('utf-8')
        except UnicodeDecodeError as error:
            raise ValueError(f'Notice fragment is not UTF-8: {name}') from error
        if not contents.endswith(b'\n'):
            raise ValueError(f'Notice fragment must end with a newline: {name}')
        parts.append(contents)
    if not parts:
        raise ValueError('No notice fragments')
    return b''.join(parts)


if __name__ == '__main__':
    try:
        (ROOT / 'THIRD_PARTY_NOTICES.md').write_bytes(assemble(ROOT))
    except (OSError, ValueError) as error:
        sys.exit(f'Notices refused: {error}')
