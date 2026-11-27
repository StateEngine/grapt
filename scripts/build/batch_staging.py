"""Run the real bounded precheck on an immutable staging checkout."""
import hashlib
import subprocess
import sys
import time

import batch_compose as compose
import heavy


def qualify(root, base, log, timeout=7200):
    if heavy.inherited() is not None:
        raise ValueError('Staging gate must acquire its own scheduler lease')
    started = time.monotonic()
    before = hashlib.sha256((root / 'Cargo.lock').read_bytes()).digest()
    commands = [
        ['cargo', 'metadata', '--locked', '--format-version=1'],
        [sys.executable, '-B', 'scripts/build/heavy.py', '--nvme', '--owner', 'batch-landing', '--',
         'bash', 'scripts/gate.sh', 'affected', '--precheck', '--staging',
         '--base', base, '--timeout', str(timeout), '--log', str(log)],
        [sys.executable, '-B', 'scripts/release/licenses.py', 'check'],
    ]
    for command in commands:
        print('+ ' + subprocess.list2cmdline(command), flush=True)
        heavy.run(command, cwd=root, check=True, timeout=timeout + 300,
                  stdout=subprocess.DEVNULL if command[1] == 'metadata' else None)
    if hashlib.sha256((root / 'Cargo.lock').read_bytes()).digest() != before:
        raise ValueError('Cargo.lock drifted during staging qualification')
    compose.git(root, 'diff', '--exit-code', 'HEAD', '--')
    seconds = round(time.monotonic() - started, 3)
    print(f'Staging wall time: {seconds}s', flush=True)
    return seconds
