"""Bounded resource allocations beside the legacy-compatible ticket queue."""
from operator_paths import nvme_mount
from contextlib import contextmanager
import json
import math
import os
from pathlib import Path
import re
import stat
import statistics
import time

OWNER = re.compile(r'[a-zA-Z0-9_.:@-]{1,128}')
MAX_BYTES = 256 * 1024
MAX_RECENT = 128


@contextmanager
def metadata(path, flags, mode):
    descriptor = os.open(path, flags | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    try:
        if not stat.S_ISREG(os.fstat(descriptor).st_mode):
            raise ValueError('Heavy queue metadata must be a regular file')
        stream = os.fdopen(descriptor, mode)
    except BaseException:
        os.close(descriptor)
        raise
    with stream:
        yield stream


class Runs:
    """All access is under Queue.locked; old queue clients leave this file alone."""
    def __init__(self, directory, tickets):
        self.directory = directory
        try:
            with metadata(directory / 'runs.json', os.O_RDONLY, 'rb') as source:
                raw = source.read(MAX_BYTES + 1)
        except FileNotFoundError:
            raw = b'{"version":1,"active":{},"recent":[]}'
        if len(raw) > MAX_BYTES:
            raise ValueError('Heavy resource state exceeds its bound')
        value = json.loads(raw)
        if (not isinstance(value, dict) or set(value) != {'version', 'active', 'recent'}
                or value['version'] != 1 or not isinstance(value['active'], dict)
                or len(value['active']) > 1024 or not isinstance(value['recent'], list)
                or len(value['recent']) > MAX_RECENT):
            raise ValueError('Invalid heavy resource state')
        self.active = value['active']
        self.recent = value['recent']
        for key, row in self.active.items():
            if (not re.fullmatch(r'[0-9-]{1,80}', key)
                    or not isinstance(row, dict)
                    or set(row) != {'slot', 'budget', 'path', 'ram_row', 'since', 'held', 'concurrent', 'targets'}
                    or row['slot'] not in ('ram', 'disk', 'waiting')
                    or type(row['budget']) is not int or not 0 <= row['budget'] <= 24 * 1024**3
                    or (row['path'] is not None and (not isinstance(row['path'], str)
                                                    or not Path(row['path']).is_absolute()))
                    or (row['ram_row'] is not None and (not isinstance(row['ram_row'], dict)
                                                       or len(json.dumps(row['ram_row'])) > 4096))
                    or (row['since'] is not None and (type(row['since']) not in (int, float)
                                                     or not math.isfinite(row['since'])))
                    or type(row['held']) not in (int, float) or not math.isfinite(row['held'])
                    or row['held'] < 0 or type(row['concurrent']) is not bool
                    or not isinstance(row['targets'], list) or len(row['targets']) > 2
                    or any(mode not in ('ram', 'disk') for mode in row['targets'])):
                raise ValueError('Invalid heavy resource allocation')
            if ((row['slot'] == 'ram' and (not row['budget'] or row['path'] is None
                                           or row['ram_row'] is None or row['since'] is None))
                    or (row['slot'] != 'ram' and (row['budget'] or row['path'] is not None
                                                  or row['ram_row'] is not None))
                    or (row['slot'] == 'waiting') != (row['since'] is None)):
                raise ValueError('Inconsistent heavy resource allocation')
        for row in self.recent:
            if (not isinstance(row, dict) or set(row) != {'id', 'owner', 'hold_seconds', 'targets', 'concurrent', 'exit'}
                    or not isinstance(row['id'], str) or not re.fullmatch(r'[0-9-]{1,80}', row['id'])
                    or not isinstance(row['owner'], str) or not OWNER.fullmatch(row['owner'])
                    or type(row['hold_seconds']) not in (int, float)
                    or not math.isfinite(row['hold_seconds']) or row['hold_seconds'] < 0
                    or not isinstance(row['targets'], list) or len(row['targets']) > 2
                    or any(mode not in ('ram', 'disk') for mode in row['targets'])
                    or type(row['concurrent']) is not bool
                    or (row['exit'] is not None and type(row['exit']) is not int)):
                raise ValueError('Invalid heavy run history')
        live = {t['id'] for t in tickets}
        self.active = {key: row for key, row in self.active.items() if key in live}

    def projection(self, owner):
        """Use successful measured holds, with a finite bootstrap estimate."""
        def kind(value):
            match = re.search(r'(?:^|[-_.:])(full|affected|composition|nightly|gate)(?:[-_.:]|$)', value, re.IGNORECASE)
            if match is None:
                return None
            return 'full' if match[1].lower() == 'nightly' else match[1].lower()
        measured = [row for row in self.recent if row['exit'] == 0 and row['hold_seconds'] > 0]
        same_owner = [row['hold_seconds'] for row in measured if row['owner'] == owner]
        selected = kind(owner)
        same_kind = [row['hold_seconds'] for row in measured
                     if selected is not None and kind(row['owner']) == selected]
        full_gates = [row['hold_seconds'] for row in measured if kind(row['owner']) == 'full']
        other_gates = [row['hold_seconds'] for row in measured if kind(row['owner']) is not None]
        values = same_owner or same_kind or full_gates or other_gates
        # A new queue has no measurement. This scheduling estimate never extends
        # a hold cap or the independent hard bound on exclusive wait.
        return min(43200, max(1, statistics.median(values[-31:]))) if values else 2700

    def save(self):
        raw = json.dumps({'version': 1, 'active': self.active, 'recent': self.recent},
                         separators=(',', ':'))
        if len(raw.encode()) > MAX_BYTES:
            raise ValueError('Heavy resource state exceeds its bound')
        staging = self.directory / 'runs.next'
        with metadata(staging, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 'w') as output:
            output.write(raw)
            output.flush()
        staging.replace(self.directory / 'runs.json')

    def holders(self, tickets):
        return [t for t in tickets if (self.active.get(t['id'], {}).get('slot') in ('ram', 'disk')
                                     or (t['holder'] and t['id'] not in self.active))]

    def anchor(self, tickets):
        # v1 clients accept at most one holder. Every actual RAM holder still
        # retains its shared kernel flock; this flag is only their compatibility anchor.
        holders = self.holders(tickets)
        first = holders[0]['id'] if holders else None
        for ticket in tickets:
            ticket['holder'] = ticket['id'] == first

    def start(self, ticket, slot, reservation, tickets):
        row = self.active.setdefault(ticket['id'], {
            'slot': 'waiting', 'budget': 0, 'path': None, 'ram_row': None, 'since': None,
            'held': 0.0, 'concurrent': False, 'targets': []})
        row.update(slot=slot, since=time.monotonic(),
                   budget=reservation[1]['budget'] if reservation else 0,
                   path=str(reservation[0]) if reservation else None,
                   ram_row=reservation[1] if reservation else None)
        if len(self.holders(tickets)) > 1:
            for peer in self.holders(tickets):
                if peer['id'] in self.active:
                    self.active[peer['id']]['concurrent'] = True

    def pause(self, ticket):
        row = self.active[ticket['id']]
        if row['since'] is not None:
            row['held'] += max(0, time.monotonic() - row['since'])
        row.update(slot='waiting', budget=0, path=None, ram_row=None, since=None)

    def finish(self, ticket, code):
        row = self.active.pop(ticket['id'], None)
        if row is not None:
            held = row['held'] + (max(0, time.monotonic() - row['since'])
                                  if row['since'] is not None else 0)
            self.recent.append({'id': ticket['id'], 'owner': ticket['owner'],
                                'hold_seconds': held, 'targets': row['targets'],
                                'concurrent': row['concurrent'], 'exit': code})
            self.recent = self.recent[-MAX_RECENT:]


class Admission:
    """Restore requested priorities without changing legacy ticket/run formats."""
    def __init__(self, directory):
        self.path = directory/'admission.json'
        try:
            with metadata(self.path, os.O_RDONLY, 'rb') as stream:
                raw = stream.read(MAX_BYTES + 1)
        except FileNotFoundError:
            raw = b'{"version":1,"requests":{},"head":null}'
        if len(raw) > MAX_BYTES:
            raise ValueError('Heavy admission state exceeds its bound')
        value = json.loads(raw)
        keys = {'version', 'requests', 'head'}
        if (not isinstance(value, dict) or set(value) not in (keys, keys | {'priorities'})
                or value['version'] != 1 or not isinstance(value['requests'], dict)
                or len(value['requests']) > 1024
                or any(not re.fullmatch(r'[0-9-]{1,80}', key) or type(ram) is not bool
                       for key, ram in value['requests'].items())
                or (value['head'] is not None and (not isinstance(value['head'], str)
                    or not re.fullmatch(r'[0-9-]{1,80}', value['head'])))):
            raise ValueError('Invalid heavy admission state; preserve the queue')
        priorities = value.get('priorities', {})
        if (not isinstance(priorities, dict) or len(priorities) > 1024
                or any(not re.fullmatch(r'[0-9-]{1,80}', key) or type(priority) is not int
                       or priority not in (0, 1) for key, priority in priorities.items())):
            raise ValueError('Invalid heavy admission priorities; preserve the queue')
        self.priorities = dict(priorities)
        self.requests = dict(value['requests'])
        self.head = value['head']
        self.original = value

    def pending(self, tickets, held, order):
        live = {t['id'] for t in tickets}
        self.requests = {key: ram for key, ram in self.requests.items() if key in live}
        self.priorities = {key: priority for key, priority in self.priorities.items() if key in live}
        for ticket in tickets:
            # Upgrade already-waiting maintenance too: its enqueue-time disk
            # sample can predate the floor breach it must recover from.
            if ticket['owner'] == 'disk-hygiene':
                self.priorities[ticket['id']] = 0
            ticket['class'] = self.priorities.get(ticket['id'], ticket['class'])
        pending = sorted((t for t in tickets if t['id'] not in held), key=order)
        # Capacity admission chooses the oldest runnable request. A persisted
        # exclusive drain must not fence a different available resource.
        self.head = None
        return pending

    def save(self):
        value = {'version': 1, 'requests': self.requests, 'head': self.head,
                 'priorities': self.priorities}
        if value == self.original:
            return
        raw = json.dumps(value, separators=(',',':'))
        if len(raw.encode()) > MAX_BYTES:
            raise ValueError('Heavy admission state exceeds its bound')
        with metadata(self.path.with_suffix('.next'), os.O_CREAT|os.O_WRONLY|os.O_TRUNC, 'w') as stream:
            stream.write(raw)
            stream.flush()
        self.path.with_suffix('.next').replace(self.path)
        self.original = json.loads(raw)


NVME_ROOT = nvme_mount()


def nvme_target(path):
    """Accept the NVMe mount and its bind aliases, including not-yet-built targets."""
    if NVME_ROOT is None or not os.path.ismount(NVME_ROOT):
        return None
    resolved = Path(path).resolve()
    existing = resolved
    while not existing.exists() and existing != existing.parent:
        existing = existing.parent
    try:
        if existing.stat().st_dev == NVME_ROOT.stat().st_dev:
            return resolved
    except FileNotFoundError:
        pass
    return None


class Placements:
    """NVMe intents are separate so already-loaded v1 clients remain readable."""
    def __init__(self, directory, tickets):
        self.path = directory/'placements.json'
        try:
            with metadata(self.path, os.O_RDONLY, 'rb') as source:
                raw = source.read(MAX_BYTES + 1)
        except FileNotFoundError:
            raw = b'{"version":1,"nvme":{}}'
        if len(raw) > MAX_BYTES:
            raise ValueError('Heavy placement state exceeds its bound')
        value = json.loads(raw)
        if (not isinstance(value, dict) or set(value) != {'version', 'nvme'}
                or value['version'] != 1 or not isinstance(value['nvme'], dict)
                or len(value['nvme']) > 1024
                or any(not re.fullmatch(r'[0-9-]{1,80}', key)
                       or not isinstance(path, str) or len(path) > 4096
                       or not Path(path).is_absolute() for key,path in value['nvme'].items())):
            raise ValueError('Invalid heavy placement state')
        live = {t['id'] for t in tickets}
        self.nvme = {key:path for key,path in value['nvme'].items() if key in live}

    def save(self):
        raw = json.dumps({'version':1, 'nvme':self.nvme}, separators=(',', ':'))
        if len(raw.encode()) > MAX_BYTES:
            raise ValueError('Heavy placement state exceeds its bound')
        staging = self.path.with_suffix('.next')
        with metadata(staging, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 'w') as output:
            output.write(raw)
        staging.replace(self.path)

    def shared(self, ticket):
        path = self.nvme.get(ticket['id'])
        return path is not None and nvme_target(path) is not None
