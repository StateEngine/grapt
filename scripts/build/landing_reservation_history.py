"""Ordered owner history consumption; no publisher writes or inferred records."""
from contextlib import contextmanager
import fcntl
import hashlib
import json
import os
from pathlib import Path
import selectors
import stat
import subprocess
import sys
import tempfile
import time

import landing_reservation as reservation

LIMIT = 16384
MAX_RECORDS = 4096


def require(condition, reason):
    if not condition:
        raise ValueError(reason)


def digest(raw):
    return hashlib.sha256(raw).hexdigest()


def parse(raw):
    def nonfinite(_):
        raise ValueError('Nonfinite reservation evidence')
    try:
        return json.loads(raw, object_pairs_hook=reservation.object_fields, parse_constant=nonfinite)
    except (RecursionError, UnicodeError) as error:
        raise ValueError('Invalid bounded reservation evidence') from error


def directory(path):
    fd = os.open('/', os.O_RDONLY | os.O_DIRECTORY)
    try:
        for part in Path(os.path.abspath(path)).parts[1:]:
            following = os.open(part, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW, dir_fd=fd)
            os.close(fd)
            fd = following
        info = os.fstat(fd)
        require(info.st_uid == os.getuid() and not info.st_mode & 0o022,
                'Reservation evidence directory is not owner-controlled')
        return fd
    except BaseException:
        os.close(fd)
        raise


def read(path):
    path = Path(path)
    parent = directory(path.parent)
    try:
        fd = os.open(path.name, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
        with os.fdopen(fd, 'rb') as source:
            before = os.fstat(source.fileno())
            require(stat.S_ISREG(before.st_mode) and before.st_uid == os.getuid()
                    and before.st_nlink == 1 and not before.st_mode & 0o022
                    and before.st_size <= LIMIT, 'Reservation evidence is not a bounded owner-controlled file')
            raw = source.read(LIMIT+1)
            after = os.fstat(source.fileno())
            require(len(raw) <= LIMIT and (before.st_size, before.st_mtime_ns, before.st_ctime_ns)
                    == (after.st_size, after.st_mtime_ns, after.st_ctime_ns), 'Reservation evidence changed during capture')
        return raw, parse(raw)
    finally:
        os.close(parent)


@contextmanager
def locked(reader):
    parent = directory(reader.path.parent)
    try:
        fd = os.open('owner.lock', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK, dir_fd=parent)
    finally:
        os.close(parent)
    with os.fdopen(fd, 'rb') as source:
        info = os.fstat(source.fileno())
        require(stat.S_ISREG(info.st_mode) and info.st_uid == os.getuid() and info.st_nlink == 1
                and not info.st_mode & 0o022, 'Existing owner lock custody differs')
        fcntl.flock(source, fcntl.LOCK_SH | fcntl.LOCK_NB)
        yield


def load_cursor(reader):
    """Retain a well-formed dead-boot cursor as a boot transition.

    A host reboot ends every process of the previous boot, so a cursor that
    is valid as a record of its own token-shaped boot owes no reserve; it is
    archived under the owner lock and the reader starts fresh. Same-boot
    validation stays byte-strict and a malformed cursor is never transitioned."""
    try:
        raw, value = read(reader.cursor)
    except FileNotFoundError:
        retained = reader.cursor.parent/(reader.cursor.name+'.history')
        require(not (reader.initialized and reader.previous is not None)
                and not retained.exists() and not retained.is_symlink(), 'Retained reader cursor is missing; preserve its history')
        reader.previous = None
        return None
    own_boot = value['boot_id'] if isinstance(value, dict) else None
    if own_boot == reader.boot:
        reader.previous = reservation.record(value, reader.boot)
        return raw
    try:
        reservation.token(own_boot)
        reservation.record(value, own_boot)
    except (TypeError, ValueError) as error:
        raise ValueError('Dead-boot cursor is malformed; preserve it') from error
    with locked(reader):
        fresh_raw, _ = read(reader.cursor)
        require(digest(fresh_raw) == digest(raw), 'Reader cursor changed before boot transition')
        transition = dict(schema='guest-owner-boot-transition/v1',
                          previous_boot_id=own_boot,
                          current_boot_id=reader.boot,
                          retained_raw_sha256=digest(raw),
                          previous_revision=value['revision'],
                          previous_generation=value['generation'])
        retain(reader, [('original.raw', raw),
                        ('boot-transition.json', (json.dumps(transition, sort_keys=True)+'\n').encode())])
    reader.previous = None
    return raw


def refresh(reader):
    reader.boot = reservation.landing_guests.bounded(reader.proc/'sys/kernel/random/boot_id', 128).decode().strip()
    reservation.token(reader.boot)
    raw = load_cursor(reader)
    reader.initialized = True
    return raw


def publication(reader, value):
    return reader.path.parent/'publications'/reader.boot/(str(value['revision'])+'-'+value['generation']+'.json')


def query(reader, revision, generation, raw_sha256):
    # The public Store API accepts an explicit directory; never fall through to
    # a publisher CLI's host default when exercising an isolated owner store.
    code = ('import sys,json;from pathlib import Path;sys.path.insert(0,sys.argv[1]);'
            'import guest_owner;store=guest_owner.Store(Path(sys.argv[1]));'
            'print(json.dumps(store.history(int(sys.argv[2]),sys.argv[3],sys.argv[4])))')
    command = [sys.executable, '-B', '-c', code, str(reader.path.parent), str(revision), generation, raw_sha256]
    try:
        with subprocess.Popen(command, stdout=subprocess.PIPE, stderr=subprocess.STDOUT) as child:
            try:
                result = bytearray()
                deadline = time.monotonic()+10
                with selectors.DefaultSelector() as selector:
                    selector.register(child.stdout, selectors.EVENT_READ)
                    while selector.get_map():
                        remaining = deadline-time.monotonic()
                        require(remaining > 0, 'Owner history lookup timed out')
                        ready = selector.select(remaining)
                        require(bool(ready), 'Owner history lookup timed out')
                        chunk = os.read(child.stdout.fileno(), 4096)
                        if not chunk:
                            selector.unregister(child.stdout)
                        result.extend(chunk)
                        require(len(result) <= 32768, 'Owner history response exceeds its bound')
                require(child.wait(timeout=max(0.01, deadline-time.monotonic())) == 0,
                        'Owner history lookup refused; preserve the reader cursor')
                return parse(result)
            finally:
                if child.poll() is None:
                    child.kill()
                    child.wait()
    except subprocess.SubprocessError as error:
        raise ValueError('Owner history lookup failed or timed out; preserve the reader cursor') from error


def chain(reader, current, current_raw, history):
    prior = reader.previous
    anchor_raw, anchor = read(publication(reader, prior))
    require(reservation.record(anchor, reader.boot) == prior, 'Authentic publication anchor differs from retained cursor')
    saved = [('anchor.raw', anchor_raw)]
    cleared = []
    generations = {prior['generation']}
    require(0 < current['revision']-prior['revision'] <= MAX_RECORDS, 'History catch-up exceeds its bound')
    while prior['revision'] < current['revision']:
        page = history(prior['revision'], prior['generation'], digest(anchor_raw))
        require(isinstance(page, dict) and set(page) == {'schema', 'boot_id', 'anchor_revision', 'records',
                'has_more', 'current_revision', 'current_path', 'current_raw_sha256'}, 'Owner history response fields differ')
        require(page['schema'] == 'guest-owner-history/v1' and page['boot_id'] == reader.boot
                and type(page['anchor_revision']) is int and page['anchor_revision'] == prior['revision']
                and type(page['current_revision']) is int and page['current_revision'] == current['revision']
                and page['current_path'] == str(reader.path) and page['current_raw_sha256'] == digest(current_raw)
                and type(page['has_more']) is bool, 'Owner history response is stale or mismatched')
        rows = page['records']
        require(isinstance(rows, list) and 1 <= len(rows) <= 32, 'Owner history page is incomplete or unbounded')
        for row in rows:
            require(isinstance(row, dict) and set(row) == {'path', 'revision', 'generation', 'phase', 'raw_sha256'},
                    'Owner publication entry fields differ')
            require(type(row['revision']) is int and row['revision'] == prior['revision']+1
                    and row['revision'] <= current['revision'], 'Owner publication order has a gap')
            require(isinstance(row['path'], str), 'Owner publication path is invalid')
            reservation.token(row['generation'])
            path = Path(row['path'])
            expected = reader.path if row['revision'] == current['revision'] else publication(reader, row)
            require(path == expected, 'Owner publication path differs from owned storage')
            raw, value = read(path)
            reservation.record(value, reader.boot)
            require(all(value[key] == row[key] for key in ('revision', 'generation', 'phase'))
                    and digest(raw) == row['raw_sha256'], 'Owner publication bytes or path differ')
            reservation.transition(prior, value)
            if value['generation'] != prior['generation']:
                require(prior['phase'] == 'idle' and value['phase'] != 'idle'
                        and value['generation'] not in generations, 'Owner history reuses or skips a generation')
                generations.add(value['generation'])
            if value['phase'] == 'idle':
                absent_pins(reader, prior)
                if prior['phase'] != 'idle':
                    cleared.append(prior)
            saved.append((str(value['revision'])+'.raw', raw))
            prior, anchor_raw = value, raw
        require(page['has_more'] == (prior['revision'] < current['revision']), 'Incomplete owner history pagination')
    require(prior == current and anchor_raw == current_raw, 'Complete history does not reach current authority')
    return saved, cleared


def absent_pins(reader, prior):
    require(not any(prior[key] is not None and reservation.matches(prior[key], reader.proc)
                    for key in ('launcher', 'qemu', 'swtpm')), 'Historical terminal still has a live controller or guest')


def absent_guests(reader):
    for index, process in enumerate(reader.proc.iterdir()):
        require(index < 131072, 'Outside guest census exceeds its bound')
        if not process.name.isdecimal():
            continue
        try:
            before, state = reservation.landing_guests.birth(process)
            args = reservation.landing_guests.bounded(process/'cmdline', LIMIT).rstrip(b'\0').split(b'\0')
            after, final = reservation.landing_guests.birth(process)
        except (FileNotFoundError, ProcessLookupError):
            continue
        require(before == after, 'Outside process birth changed during census')
        if state in (b'Z', b'X') or final in (b'Z', b'X'):
            continue
        for root in reservation.ROOTS.values():
            for pair in ((b'-qmp', os.fsencode(f'unix:{root}/state/qmp.sock,server=on,wait=off')),
                         (b'--tpmstate', os.fsencode(f'dir={root}/state/tpm'))):
                require(not any(tuple(args[i:i+2]) == pair for i in range(len(args)-1)),
                        'Outside census still has a live guest or TPM')


def retain(reader, files):
    root = reader.cursor.parent/(reader.cursor.name+'.history')
    try:
        root.mkdir(mode=0o700)
        parent = directory(root.parent)
        try:
            os.fsync(parent)
        finally:
            os.close(parent)
    except FileExistsError:
        pass
    fd = directory(root)
    try:
        require(not os.fstat(fd).st_mode & 0o077, 'Reader history storage is not private')
    finally:
        os.close(fd)
    for name, raw in files:
        target = root/(digest(raw)+'.json')
        try:
            existing, _ = read(target)
            require(existing == raw, 'Retained reader proof conflicts')
        except FileNotFoundError:
            descriptor, temporary = tempfile.mkstemp(prefix='.history-', dir=root)
            try:
                with os.fdopen(descriptor, 'wb') as output:
                    output.write(raw)
                    output.flush()
                    os.fchmod(output.fileno(), 0o400)
                    os.fsync(output.fileno())
                os.replace(temporary, target)
            finally:
                Path(temporary).unlink(missing_ok=True)
    fd = directory(root)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def commit(reader, original, current, current_raw, files, census, cleared):
    # Caller holds the existing owner lock; neither lookup nor refusal advances a cursor.
    def observe_current():
        boot = reservation.landing_guests.bounded(reader.proc/'sys/kernel/random/boot_id', 128).decode().strip()
        require(boot == reader.boot and read(reader.cursor)[0] == original and read(reader.path)[0] == current_raw,
                'Reader or current authority changed during recovery')
        with tempfile.TemporaryDirectory(prefix='.reader-', dir=reader.cursor.parent) as temporary:
            probe = reservation.Reader(Path(temporary)/'cursor.json', reader.path, reader.proc)
            probe.initialized, probe.boot, probe.previous = True, reader.boot, current
            result = probe.merge(census())
        require(read(reader.path)[0] == current_raw and read(reader.cursor)[0] == original,
                'Reader or current authority changed before commit')
        if current['phase'] == 'idle':
            absent_guests(reader)
        for prior in cleared:
            absent_pins(reader, prior)
        return result

    observe_current()
    retain(reader, [('original.raw', original), *files])
    result = observe_current()
    reservation.save(reader.cursor, current)
    reader.previous = current
    return result


def observe(reader, history, census):
    original = refresh(reader)
    current_raw, current = read(reader.path)
    reservation.record(current, reader.boot)
    if reader.previous is None or current['revision'] <= reader.previous['revision']+1:
        with locked(reader):
            require(read(reader.path)[0] == current_raw, 'Current authority changed before observation')
            if original is not None:
                require(read(reader.cursor)[0] == original, 'Reader changed before observation')
            return reader.merge(census())
    history = history or (lambda revision, generation, sha: query(reader, revision, generation, sha))
    files, cleared = chain(reader, current, current_raw, history)
    with locked(reader):
        return commit(reader, original, current, current_raw, files, census, cleared)
