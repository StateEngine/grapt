"""Read the host's reservation; only its joined-stop record releases memory."""
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import tempfile
import uuid

import landing_guests
import operator_paths

PATH = operator_paths.guest_root()/'winvm-owner'/'state.json'
ROOTS = dict(zip(('stateengine-winvm', 'secode-winvm'), landing_guests.ROOTS))
FIELDS = {'schema', 'phase', 'boot_id', 'generation', 'revision', 'owner', 'guest',
          'memory_gib', 'owner_process', 'qemu', 'swtpm', 'launcher', 'boot_intent',
          'purpose', 'admission_ref', 'adopted', 'clearance', 'updated_utc'}
PINS = ('owner_process', 'qemu', 'swtpm', 'launcher')


def text(value, pattern):
    if not isinstance(value, str) or not re.fullmatch(pattern, value):
        raise ValueError('Invalid reservation text field')


def token(value):
    text(value, '[0-9a-fA-F]{8}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{4}-[0-9a-fA-F]{12}')
    uuid.UUID(value)


def pin(value, boot):
    if (not isinstance(value, dict) or set(value) != {'pid', 'start', 'boot_id', 'cmdline_sha256'}
            or type(value['pid']) is not int or not 0 < value['pid'] < 2**31
            or value['boot_id'] != boot):
        raise ValueError('Invalid reservation process pin')
    text(value['start'], '[0-9]{1,32}')
    text(value['cmdline_sha256'], '[0-9a-f]{64}')


def record(value, boot):
    if (not isinstance(value, dict) or set(value) != FIELDS
            or value['schema'] != 'guest-owner/v1' or value['boot_id'] != boot
            or type(value['revision']) is not int or value['revision'] < 1
            or type(value['adopted']) is not bool):
        raise ValueError('Invalid reservation schema, boot or revision')
    token(value['generation'])
    if not isinstance(value['updated_utc'], str) or len(value['updated_utc']) > 80:
        raise ValueError('Invalid reservation diagnostic timestamp')
    if datetime.fromisoformat(value['updated_utc']).utcoffset() is None:
        raise ValueError('Reservation timestamp has no timezone')
    phase = value['phase']
    if phase == 'idle':
        empty = (*PINS, 'owner', 'guest', 'memory_gib', 'boot_intent', 'purpose', 'admission_ref')
        if any(value[key] is not None for key in empty):
            raise ValueError('Idle reservation retains active authority')
        clearance = value['clearance']
        if (not isinstance(clearance, dict) or set(clearance) !=
                {'kind', 'joined_stop_ref', 'stopped_generation', 'stopped_guest', 'observer'}):
            raise ValueError('Idle reservation needs outside clearance')
        pin(clearance['observer'], boot)
        if clearance['kind'] == 'initial-observation':
            if any(clearance[key] is not None for key in
                   ('joined_stop_ref', 'stopped_generation', 'stopped_guest')):
                raise ValueError('Initial observation cannot claim a joined stop')
        elif clearance['kind'] in ('joined-stop', 'cancelled-admission'):
            text(clearance['joined_stop_ref'], r'(?:msg-[0-9a-f]{24}|spool:[0-9]{8}/[A-Za-z0-9][A-Za-z0-9_.-]{0,100}/reservation-clearance\.json#sha256=[0-9a-f]{64})')
            if (clearance['stopped_generation'] != value['generation']
                    or not isinstance(clearance['stopped_guest'], str)
                    or clearance['stopped_guest'] not in ROOTS):
                raise ValueError('Reservation clearance has no matching generation')
        else:
            raise ValueError('Invalid reservation clearance kind')
    elif phase in ('admitted', 'booting', 'running'):
        text(value['owner'], '[A-Za-z0-9_-]{1,80}')
        text(value['purpose'], r'[^\x00]{1,200}')
        text(value['admission_ref'], 'msg-[0-9a-f]{24}')
        token(value['boot_intent'])
        if (not isinstance(value['guest'], str) or value['guest'] not in ROOTS
                or type(value['memory_gib']) is not int or value['memory_gib'] not in (8, 12, 16, 24)
                or value['clearance'] is not None):
            raise ValueError('Invalid active reservation')
        for key in PINS:
            required = key == 'owner_process' or key == 'launcher' and phase != 'admitted' or phase == 'running'
            if required:
                pin(value[key], boot)
            elif value[key] is not None:
                raise ValueError('Reservation contains a premature process pin')
    else:
        raise ValueError('Unknown reservation phase')
    return value


def object_fields(pairs):
    result = {}
    for key, value in pairs:
        if key in result:
            raise ValueError('Duplicate reservation field')
        result[key] = value
    return result


def load(path):
    parent = path.parent.lstat()
    if not stat.S_ISDIR(parent.st_mode) or parent.st_uid != os.getuid() or parent.st_mode & 0o022:
        raise ValueError('Reservation parent is not private to its writer')
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid()
                or info.st_mode & 0o022 or info.st_size > 16384):
            raise ValueError('Reservation is not a bounded host-owned regular file')
        raw = source.read(16385)
    if len(raw) > 16384:
        raise ValueError('Reservation exceeds its bound')
    try:
        return json.loads(raw, object_pairs_hook=object_fields)
    except RecursionError as error:
        raise ValueError('Reservation nesting exceeds the parser bound') from error


def save(path, value):
    descriptor, temporary = tempfile.mkstemp(prefix='.reservation-', dir=path.parent)
    try:
        with os.fdopen(descriptor, 'w') as output:
            json.dump(value, output, allow_nan=False)
            output.flush()
            os.fsync(output.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        Path(temporary).unlink(missing_ok=True)


def matches(value, proc):
    process = proc/str(value['pid'])
    try:
        before, state = landing_guests.birth(process)
        command = landing_guests.bounded(process/'cmdline', 16384)
        after, final = landing_guests.birth(process)
    except (FileNotFoundError, ProcessLookupError):
        return False
    return (before == after == value['start'] and state not in (b'Z', b'X')
            and final not in (b'Z', b'X') and hashlib.sha256(command).hexdigest() == value['cmdline_sha256'])


def transition(prior, value):
    if prior is not None:
        if (value['revision'] < prior['revision']
                or value['revision'] == prior['revision'] and value != prior):
            raise ValueError('Reservation revision rolled back or changed in place')
        if value['phase'] == 'idle' and value['clearance']['kind'] == 'initial-observation' and value != prior:
            raise ValueError('Initial observation cannot erase a prior reservation')
        if prior['phase'] != 'idle':
            if value['generation'] != prior['generation']:
                raise ValueError('Retained admission has no matching generation clearance')
            if value['phase'] == 'idle':
                clearance = value['clearance']
                if (clearance['stopped_guest'] != prior['guest']
                        or clearance['stopped_generation'] != prior['generation']
                        or prior['phase'] != 'admitted' and clearance['kind'] != 'joined-stop'):
                    raise ValueError('Clearance does not match the retained admission')
        elif value['phase'] != 'idle' and value['generation'] == prior['generation']:
            raise ValueError('A cleared admission generation cannot be reused')
        if prior['phase'] != 'idle' and value['phase'] != 'idle':
            if any(value[key] != prior[key] for key in
                   ('owner', 'guest', 'memory_gib', 'boot_intent', 'admission_ref', 'owner_process')):
                raise ValueError('Reservation admission identity changed within its generation')
            phases = ('admitted', 'booting', 'running')
            if phases.index(value['phase']) < phases.index(prior['phase']):
                raise ValueError('Reservation phase regressed within its generation')
            if any(prior[key] is not None and value[key] != prior[key]
                   for key in ('qemu', 'swtpm', 'launcher')):
                raise ValueError('Reservation process identity changed within its generation')


class Reader:
    def __init__(self, cursor, path=PATH, proc=Path('/proc')):
        self.cursor, self.path, self.proc = Path(cursor), Path(path), Path(proc)
        self.previous = None
        self.initialized = False

    def merge(self, guests):
        if not self.initialized:
            self.boot = landing_guests.bounded(self.proc/'sys/kernel/random/boot_id', 128).decode().strip()
            token(self.boot)
            from landing_reservation_history import load_cursor
            load_cursor(self)
            self.initialized = True
        value = record(load(self.path), self.boot)
        prior = self.previous
        transition(prior, value)
        if value['phase'] == 'idle':
            if guests or prior is not None and any(prior[key] is not None and matches(prior[key], self.proc)
                                                   for key in ('qemu', 'swtpm', 'launcher')):
                raise ValueError('Idle clearance still has a live guest or controller')
            result = list(guests)
        else:
            required = ['owner_process']
            if value['phase'] == 'booting':
                required.append('launcher')
            if value['phase'] == 'running':
                required.extend(('qemu', 'swtpm'))
            if not all(matches(value[key], self.proc) for key in required):
                raise ValueError('Reservation process identity is absent or stale; retain reserve')
            root = str(ROOTS[value['guest']])
            selected = [guest for guest in guests if guest['root'] == root]
            budget, rss = value['memory_gib']*1024**3, 0
            if len(selected) > 1 or selected and selected[0]['memory_bytes'] != budget:
                raise ValueError('Reservation and observed guest RAM differ')
            if value['phase'] == 'running':
                if (len(selected) != 1 or selected[0]['pid'] != value['qemu']['pid']
                        or selected[0]['start'] != value['qemu']['start']
                        or selected[0]['memory_bytes'] != budget):
                    raise ValueError('Reservation and observed guest identity or RAM differ')
                rss = selected[0]['rss']
            result = [guest for guest in guests if guest['root'] != root]
            # Pre-boot or uncertain matching cannot borrow another process's
            # resident pages. The known reservation never owns an ordinary slot.
            result.append(dict(root=root, memory_bytes=budget, rss=rss,
                               reservation=value))
        if value != prior:
            save(self.cursor, value)
            self.previous = value
        return result

    def observe(self, history=None, census=landing_guests.active):
        """Consume complete authentic history before releasing a missed generation."""
        from landing_reservation_history import observe
        try:
            return observe(self, history, census)
        except (KeyError, TypeError, AttributeError, OverflowError, RuntimeError) as error:
            raise ValueError('Malformed owner history or reservation evidence') from error

    def reconcile(self, attestation, census=landing_guests.active):
        """Explicit consumption of owner authority for genuinely lost history only."""
        from landing_reservation_reconcile import reconcile
        try:
            return reconcile(self, Path(attestation), census)
        except (KeyError, TypeError, AttributeError, OverflowError) as error:
            raise ValueError('Malformed owner reconciliation evidence') from error
