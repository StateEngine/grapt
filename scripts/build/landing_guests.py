"""Read the two VM owners' process pins; never control either guest."""
import os
from pathlib import Path

import durable_jobs
import operator_paths

ROOTS = (operator_paths.guest_root()/'winvm', operator_paths.guest_root()/'winvm-secode')
RESERVE = 28 * 1024**3
MARGIN = 8 * 1024**3
ADMISSION_FLOOR = RESERVE + MARGIN


def bounded(path, maximum):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        raw = source.read(maximum + 1)
    if len(raw) > maximum:
        raise ValueError('Landing guest process sample exceeds its bound')
    return raw


def birth(process):
    raw = bounded(process/'stat', 8192)
    fields = raw.rsplit(b') ', 1)[1].split()
    if len(fields) < 22:
        raise ValueError('Landing guest process state is incomplete')
    return fields[19].decode(), fields[0]


def memory_budget(argv):
    positions = [index for index, word in enumerate(argv) if word == b'-m']
    if len(positions) != 1 or positions[0]+1 >= len(argv):
        raise ValueError('Landing guest needs one bounded MiB memory selection')
    selected = argv[positions[0]+1]
    if not selected.isdigit() or not 1 <= int(selected) <= 1048576:
        raise ValueError('Landing guest needs one bounded MiB memory selection')
    return int(selected)*1024**2


def resident(process, start):
    _, separator, tail = bounded(process/'stat', 8192).rpartition(b') ')
    fields = tail.split()
    if not separator or len(fields) < 22 or fields[19].decode() != start:
        raise ValueError('Landing guest memory process birth changed')
    if fields[0] in (b'Z', b'X'):
        raise ProcessLookupError('Landing guest exited during memory sampling')
    if not fields[21].isdigit():
        raise ValueError('Landing guest resident memory is invalid')
    return int(fields[21])*os.sysconf('SC_PAGE_SIZE')


def active(roots=ROOTS, proc=Path('/proc')):
    guests = []
    for root in roots:
        try:
            pin = durable_jobs.read(root/'state/qemu.json')
        except FileNotFoundError:
            continue
        if (not isinstance(pin, dict) or set(pin) != {'pid', 'start', 'cmdline'}
                or type(pin['pid']) is not int or not 0 < pin['pid'] < 2**31
                or not isinstance(pin['start'], str) or not pin['start'].isdigit()
                or len(pin['start']) > 32 or not isinstance(pin['cmdline'], str)
                or len(pin['cmdline']) > 16384):
            raise ValueError('Invalid landing guest process pin')
        process = proc/str(pin['pid'])
        try:
            before, state = birth(process)
            command = bounded(process/'cmdline', 16384)
            after, final = birth(process)
        except (FileNotFoundError, ProcessLookupError):
            continue
        if before != pin['start'] or after != pin['start']:
            raise ValueError('Landing guest process birth changed')
        if state in (b'Z', b'X') or final in (b'Z', b'X'):
            continue
        argv = command.rstrip(b'\0').split(b'\0')
        pidfile = os.fsencode(root/'state/qemu.pid')
        if (not argv or not Path(os.fsdecode(argv[0])).name.startswith('qemu-system-')
                or command.replace(b'\0', b' ').decode() != pin['cmdline']
                or not any(argv[index:index+2] == [b'-pidfile', pidfile]
                           for index in range(len(argv)-1))):
            raise ValueError('Landing guest command differs from its owner pin')
        try:
            executable = (process/'exe').resolve(strict=True).name
            final_birth, final_state = birth(process)
        except (FileNotFoundError, ProcessLookupError):
            continue
        if final_birth != pin['start']:
            raise ValueError('Landing guest process changed after its sample')
        if final_state in (b'Z', b'X'):
            continue
        if not executable.startswith('qemu-system-'):
            raise ValueError('Landing guest executable is not QEMU')
        budget = memory_budget(argv)
        try:
            rss = resident(process, pin['start'])
        except (FileNotFoundError, ProcessLookupError):
            continue
        guests.append(dict(root=str(root), pid=pin['pid'], start=pin['start'],
                           memory_bytes=budget, rss=rss))
    return guests
