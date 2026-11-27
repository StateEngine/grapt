"""One host ticket queue; the existing flock remains the execution boundary."""
from contextlib import contextmanager
import errno
import fcntl
import json
import math
import os
from pathlib import Path
import re
import select
import socket
import subprocess
import time
from heavy_runs import Admission, OWNER, Runs, Placements, metadata
from heavy_drain import Drain, seconds, MAX_HOLD_SECONDS, WAIT_SECONDS

MAX_TICKETS = 1024
MAX_STATE = 1024 * 1024
# Diagnostic only: how many lock observations a queue keeps, so a failed count
# assertion can name the sample behind its own number.
INVENTORY_LIMIT = 64


def identity(pid):
    """Boot and /proc start ticks distinguish a live process from a reused PID."""
    try:
        with Path(f'/proc/{pid}/stat').open() as stream:
            raw = stream.read(8193)
        if len(raw) > 8192:
            raise ValueError('Process identity exceeds its bound')
        fields = raw.rsplit(')', 1)[1].split()
        if fields[0] == 'Z':
            return None
        boot = Path('/proc/sys/kernel/random/boot_id').read_text().strip()
        return f'{boot}:{fields[19]}'
    except OSError as error:
        if error.errno in (errno.ENOENT, errno.ESRCH):
            return None
        raise


def order(ticket):
    return ticket['class'], ticket['enqueued_ns'], ticket['id']


def ram_available():
    import ram_targets
    try:
        ram_targets.capacity()
        return True
    except BlockingIOError:
        return None  # A peer is updating reservations; capacity is still unknown.
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
        return False


class Queue:
    def __init__(self, directory, heavy):
        self.directory = Path(directory)
        self.heavy = Path(heavy)
        self.directory.mkdir(mode=0o700, parents=True, exist_ok=True)

    def shared_limit(self):
        from heavy_hardware import ceiling
        return ceiling(self.directory)

    def legacy_shared_limit(self):
        # Four slots require a live monitor. A dead monitor returns admission
        # to three without stopping any already-running job.
        try:
            with metadata(self.directory/'capacity.json', os.O_RDONLY, 'r') as stream:
                raw = stream.read(1025)
        except FileNotFoundError:
            return 3
        if len(raw) > 1024:
            raise ValueError('Oversized shared-capacity record')
        row = json.loads(raw)
        if (not isinstance(row, dict) or row.get('version') != 1
                or type(row.get('limit')) is not int or row['limit'] not in (3, 4)):
            raise ValueError('Invalid shared-capacity record')
        if row['limit'] == 3:
            return 3
        expires = row.get('expires_unix')
        if type(expires) not in (int, float) or not 0 < expires < float('inf'):
            raise ValueError('Invalid shared-capacity expiry')
        return 4 if time.time() < expires else 3

    def capable_clients(self, tickets, own, *, hardware=False):
        # Old clients cannot enter a fourth slot. Preserve their FIFO position
        # among slots they can use, without leaving the new fourth slot idle.
        path = self.directory/('hardware-clients.json' if hardware else 'capacity-clients.json')
        try:
            with metadata(path, os.O_RDONLY, 'r') as stream:
                raw = stream.read(MAX_STATE + 1)
        except FileNotFoundError:
            raw = '{"version":1,"ids":[]}'
        if len(raw) > MAX_STATE:
            raise ValueError('Oversized capacity-client record')
        row = json.loads(raw)
        if (not isinstance(row, dict) or set(row) != {'version', 'ids'} or row['version'] != 1
                or not isinstance(row['ids'], list) or len(row['ids']) > MAX_TICKETS
                or any(not isinstance(key, str) or not re.fullmatch(r'[0-9-]{1,80}', key) for key in row['ids'])):
            raise ValueError('Invalid capacity-client record')
        clients = set(row['ids']) & {ticket['id'] for ticket in tickets}
        clients.add(own['id'])
        if clients != set(row['ids']):
            staging = path.with_suffix('.next')
            with metadata(staging, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 'w') as stream:
                json.dump({'version': 1, 'ids': sorted(clients)}, stream)
                stream.flush()
            staging.replace(path)
        return clients

    @contextmanager
    def locked(self, deadline):
        with metadata(self.directory/'mutex', os.O_CREAT | os.O_RDWR, 'a') as mutex:
            while True:
                try:
                    fcntl.flock(mutex, fcntl.LOCK_EX | fcntl.LOCK_NB)
                    break
                except BlockingIOError:
                    remaining = deadline - time.monotonic()
                    if remaining <= 0:
                        raise TimeoutError('heavy queue metadata lock not acquired before deadline')
                    time.sleep(min(0.02, remaining))
            yield

    def load(self):
        path = self.directory/'tickets.json'
        try:
            with metadata(path, os.O_RDONLY, 'rb') as stream:
                data = stream.read(MAX_STATE + 1)
        except FileNotFoundError:
            return []
        if len(data) > MAX_STATE:
            raise ValueError('Heavy queue exceeds its state bound')
        state = json.loads(data)
        if not isinstance(state, dict) or set(state) != {'version', 'tickets'} or state['version'] != 1:
            raise ValueError('Invalid heavy queue format; preserve the queue for diagnosis')
        tickets = state['tickets']
        if not isinstance(tickets, list) or len(tickets) > MAX_TICKETS:
            raise ValueError('Invalid heavy queue size')
        ids = set()
        for row in tickets:
            if (not isinstance(row, dict) or
                    set(row) != {'id', 'class', 'enqueued_ns', 'pid', 'start', 'owner', 'holder'} or
                    not isinstance(row['id'], str) or not re.fullmatch(r'[0-9-]{1,80}', row['id']) or
                    type(row['class']) is not int or row['class'] not in (0, 1) or
                    type(row['enqueued_ns']) is not int or row['enqueued_ns'] <= 0 or
                    type(row['pid']) is not int or row['pid'] <= 0 or
                    not isinstance(row['start'], str) or len(row['start']) > 100 or
                    not isinstance(row['owner'], str) or not OWNER.fullmatch(row['owner']) or
                    type(row['holder']) is not bool or row['id'] in ids):
                raise ValueError('Invalid heavy queue ticket')
            ids.add(row['id'])
        if sum(row['holder'] for row in tickets) > 1:
            raise ValueError('Heavy queue has multiple holders')
        return tickets

    def save(self, tickets):
        # One fixed staging path under the metadata lock bounds crash debris.
        staging = self.directory/'tickets.next'
        with metadata(staging, os.O_CREAT | os.O_WRONLY | os.O_TRUNC, 'w') as stream:
            json.dump({'version': 1, 'tickets': tickets}, stream, separators=(',', ':'))
            stream.flush()
        staging.replace(self.directory/'tickets.json')

    def reap(self, tickets):
        live = []
        for ticket in tickets:
            if identity(ticket['pid']) == ticket['start']:
                live.append(ticket)
        return live

    def state(self):
        before = self.load()
        tickets = self.reap(before)
        runs = Runs(self.directory, tickets)
        runs.anchor(tickets)
        if tickets != before:
            self.save(tickets)
            self.notify(tickets)
        return tickets, runs

    def live(self):
        return self.state()[0]

    def notify(self, tickets):
        with socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM) as sender:
            sender.setblocking(False)
            for ticket in tickets:
                try:
                    sender.sendto(b'!', self.address(ticket))
                except BlockingIOError:
                    pass  # An unread notification already guarantees a wake.
                except OSError as error:
                    if error.errno not in (2, 111):
                        raise

    def address(self, ticket):
        # Linux abstract sockets disappear with the process, including SIGKILL.
        return '\0secode-heavy-' + ticket['id']

    def enqueue(self, priority, owner, deadline, *, ram=None, nvme=None, max_hold=None, projected=None, refund=None, jobs=8, gate_log=None):
        if max_hold is not None and (not seconds(max_hold) or not 0 < max_hold <= MAX_HOLD_SECONDS
                                     or projected is not None and (not seconds(projected) or not 0 < projected <= 43200)):
            raise ValueError('Exclusive max hold must be in (0, 1800]; projection in (0, 43200]')
        if ram is not None and type(ram) is not bool:
            raise ValueError('Heavy RAM intent must be a boolean')
        if type(priority) is not int or priority not in (0, 1) or not OWNER.fullmatch(owner):
            raise ValueError('Use class 0 or 1 and a plain owner of at most 128 characters')
        pid = os.getpid()
        started = identity(pid)
        if started is None:
            raise RuntimeError('Cannot establish the heavy job process identity')
        with self.locked(deadline):
            tickets, runs = self.state()
            admission = Admission(self.directory)
            held = {t['id'] for t in runs.holders(tickets)}
            admission.pending(tickets, held, order)  # Preserve the head before this arrival.
            if len(tickets) >= MAX_TICKETS:
                raise RuntimeError('Heavy queue is full (1024 jobs)')
            # The metadata lock provides an unambiguous enqueue ordering.
            stamp = max(time.monotonic_ns(), max((t['enqueued_ns'] for t in tickets), default=0) + 1)
            ticket = dict(id=f'{pid}-{stamp}', **{'class': priority}, enqueued_ns=stamp,
                          pid=pid, start=started, owner=owner, holder=False)
            notification = socket.socket(socket.AF_UNIX, socket.SOCK_DGRAM)
            try:
                notification.bind(self.address(ticket))
                notification.setblocking(False)
                tickets.append(ticket)
                from heavy_hardware import Profiles
                Profiles(self.directory, tickets, runs).register(ticket, jobs, gate_log)
                drain = Drain(self.directory, tickets)
                if refund is not None:
                    drain.redeem(refund, ticket)
                if max_hold is not None:
                    drain.declare(ticket, max_hold, runs.projection(owner) if projected is None else projected)
                placements = Placements(self.directory, tickets)
                if nvme is not None:
                    from heavy_runs import nvme_target
                    target = nvme_target(nvme)
                    if target is None:
                        raise ValueError('Shared disk target is not on the NVMe filesystem')
                    placements.nvme[ticket['id']] = str(target)
                placements.save()
                if ram is not None:
                    admission.requests[ticket['id']] = ram
                admission.pending(tickets, held, order)
                admission.save()
                self.save(tickets)
                self.notify(tickets)
            except BaseException:
                notification.close()
                raise
            return ticket, notification

    def remove(self, ticket, code=None, *, refund_pending=False):
        # Cleanup has a separate small budget even when acquisition timed out.
        with self.locked(time.monotonic() + 5):
            tickets, runs = self.state()
            drain = Drain(self.directory, tickets)
            refunded = False
            if refund_pending and ticket['id'] not in runs.active:
                row = next((row for row in tickets if row['id']==ticket['id']), None)
                if row is None or any(row[key]!=ticket[key] for key in ('owner','pid','start')):
                    raise ValueError('Expired waiter identity changed; preserve its ticket')
                drain.refund(row, event='wait-timeout-refund')
                refunded = True
            if drain.state['active'] and drain.state['active']['ticket'] == ticket['id']:
                drain.finish(time.monotonic(), 'ticket-removed')
                drain.save()
            runs.finish(ticket, code)
            from heavy_hardware import Profiles
            Profiles(self.directory, tickets, runs).finish(ticket, code)
            tickets = [t for t in tickets if t['id'] != ticket['id']]
            admission = Admission(self.directory)
            admission.pending(tickets, {t['id'] for t in runs.holders(tickets)}, order)
            admission.save()
            runs.anchor(tickets)
            runs.save()
            self.save(tickets)
            self.notify(tickets)
            return refunded

    def snapshot(self, deadline, detailed=False):
        if detailed:
            import heavy_sampling
            sampled_at = time.monotonic()
            forest = heavy_sampling.processes()
            hardware = heavy_sampling.host()
            can_ram = any(Admission(self.directory).requests.values()) and ram_available()
            with self.heavy.open('a') as handle:
                try:
                    count = self.shared_locks(handle)
                except BlockingIOError:
                    count = None
        with self.locked(deadline):
            if detailed and time.monotonic() - sampled_at > 2:
                raise TimeoutError('Status hardware sample aged while waiting for queue metadata')
            tickets, runs = self.state()
            holders = {t['id'] for t in runs.holders(tickets)}
            admission = Admission(self.directory)
            positions = {t['id']: i for i, t in enumerate(
                admission.pending(tickets, holders, order), 1)}
            if detailed:
                import external_holders
                limit = self.shared_limit()
                profiles, external, count, charged, host = self.resources(
                    tickets, runs, count, forest, hardware, limit)
                peers = profiles.peers(runs.holders(tickets), count, external) if count is not None else []
                placements = Placements(self.directory, tickets)
            result = []
            for ticket in tickets:
                row = dict(ticket, holder=ticket['id'] in holders)
                row['class'] = admission.priorities.get(ticket['id'], ticket['class'])
                if detailed and ticket['id'] in positions:
                    row['position'] = positions[ticket['id']]
                    ram = admission.requests.get(ticket['id'], False)
                    shared = placements.shared(ticket)
                    limit = self.shared_limit() + int(ticket['class'] == 0 or any(
                        t['class'] == 0 for t in tickets if t['id'] in holders))
                    charged = external_holders.charged_count(count, external, limit)
                    row['placement'] = 'RAM requested' if ram else 'NVMe shared' if shared else 'exclusive'
                    row['resource_refusal'] = ('shared lease census unavailable' if count is None else
                        'RAM target metadata is busy' if ram and can_ram is None else
                        profiles.placement(ticket, host, peers, charged, limit,
                            ram=ram and can_ram, shared=shared)[1])
                    row['hardware'] = hardware
                if detailed and ticket['id'] in runs.active:
                    run = runs.active[ticket['id']]
                    seconds = run['held'] + (max(0, time.monotonic() - run['since'])
                                              if run['since'] is not None else 0)
                    row.update(mode=run['slot'], budget=run['budget'],
                               concurrent=run['concurrent'], hold_seconds=seconds)
                result.append(row)
            if detailed and time.monotonic() - sampled_at > 2:
                raise TimeoutError('Status hardware sample aged during queue pricing')
            return result

    def holder(self, tickets):
        holders = [t for t in tickets if t['holder']]
        if holders:
            return ', '.join(f"{t['owner']} pid {t['pid']}" for t in holders)
        with self.heavy.open('a') as probe:
            try:
                fcntl.flock(probe, fcntl.LOCK_EX | fcntl.LOCK_NB)
            except BlockingIOError:
                info = os.fstat(probe.fileno())
                key = f'{os.major(info.st_dev):02x}:{os.minor(info.st_dev):02x}:{info.st_ino}'
                with Path('/proc/locks').open() as stream:
                    for index, line in enumerate(stream):
                        if index >= 65536:
                            break
                        fields = line.split()
                        if len(fields) > 5 and fields[1] == 'FLOCK' and fields[5] == key:
                            return f'legacy flock pid {fields[4]} (owner unknown)'
                return 'legacy flock (owner unknown)'
        return 'none'

    def inherited_shared_leases(self, key, proc=Path('/proc'), processes=None):
        # Count admitted lease identities through the descriptors actually held
        # by descendants, including children whose original wrapper was killed.
        # Unlike the global /proc/locks iterator, each fdinfo read is local to
        # one file description and unrelated flock churn cannot shift it.
        leases = set()
        directory, heavy = str(self.directory.resolve()), str(self.heavy.resolve())
        for index, process in enumerate(path for path in proc.iterdir() if path.name.isdigit()):
            if index >= 32768:
                raise ValueError('Heavy process inventory exceeds its bound')
            try:
                if process.stat().st_uid != os.getuid():
                    continue
                with (process / 'environ').open('rb') as stream:
                    raw = stream.read(MAX_STATE + 1)
            except (FileNotFoundError, ProcessLookupError, PermissionError):
                continue
            prefix = b'SECODE_HEAVY_LEASE='
            # Discard all other environment data; only scheduler metadata is used.
            value = next((item[len(prefix):] for item in raw.split(b'\0') if item.startswith(prefix)), None)
            if value is None or len(value) > 8192:
                continue
            try:
                claim = json.loads(value)
            except (ValueError, UnicodeDecodeError):
                continue
            if (not isinstance(claim, dict) or claim.get('queue') != directory
                    or claim.get('heavy') != heavy):
                continue
            if (type(claim.get('fd')) is not int or not 0 <= claim['fd'] <= 1048576
                    or not isinstance(claim.get('ticket'), str)
                    or not re.fullmatch(r'[0-9-]{1,80}', claim['ticket'])):
                raise ValueError('Invalid heavy descriptor claim')
            try:
                with (process / 'fdinfo' / str(claim['fd'])).open() as stream:
                    info = stream.read(65537)
            except (FileNotFoundError, ProcessLookupError):
                continue  # An environment-only claim does not retain a flock.
            except PermissionError:
                raise BlockingIOError('Matching heavy descriptor is unreadable') from None
            if len(info) > 65536:
                raise ValueError('Heavy descriptor inventory exceeds its bound')
            locks = [fields for fields in map(str.split, info.splitlines())
                     if len(fields) > 6 and fields[:1] == ['lock:']
                     and fields[2:5] == ['FLOCK', 'ADVISORY', 'READ'] and fields[6] == key]
            if locks:
                leases.add(claim['ticket'])
                if processes is not None:
                    processes.add(int(process.name))
                    # The wrapper owns the original flock but exports its claim
                    # only to children. Corroborate that owner before folding an
                    # inner raw flock into the same inherited lease identity.
                    pid, started = claim.get('pid'), claim.get('start')
                    try:
                        # A retained file description still names its original
                        # flock owner after that wrapper exits. Fold that raw
                        # entry only when fdinfo corroborates it and the PID is
                        # gone; a reused live PID retains its separate charge.
                        if (type(pid) is int and pid > 0 and
                                any(fields[5] == str(pid) for fields in locks) and
                                identity(pid) is None):
                            processes.add(pid)
                        with (process / 'stat').open() as stream:
                            status = stream.read(8193)
                        parent = status.rsplit(')', 1)[-1].split()
                        if (len(status) <= 8192 and len(parent) > 1
                                and type(pid) is int and pid > 0 and parent[1] == str(pid)
                                and isinstance(started, str)
                                and identity(pid) == started):
                            processes.add(pid)
                    except (OSError, ValueError, IndexError):
                        # Keep the descriptor-corroborated lease, without folding
                        # its wrapper when this later observation is unavailable.
                        continue
        return leases

    def shared_locks(self, handle, held=()):
        # Keep raw-flock compatibility, and corroborate governed jobs through
        # inherited fdinfo so paginated global reads cannot hide their orphans.
        info = os.fstat(handle.fileno())
        key = f'{os.major(info.st_dev):02x}:{os.minor(info.st_dev):02x}:{info.st_ino}'
        owners = []
        with Path('/proc/locks').open() as stream:
            for index, line in enumerate(stream):
                if index >= 65536 or len(line) > 4096:
                    raise ValueError('Kernel lock inventory exceeds its bound')
                fields = line.split()
                if len(fields) > 5 and fields[1:4] == ['FLOCK', 'ADVISORY', 'READ'] and fields[5] == key:
                    owners.append(int(fields[4]))
        governed = set()
        # Diagnostic only: a pid whose /proc entry is unreadable must not turn an
        # otherwise valid count into an error, so this never propagates.
        def snapshot(pids):
            observed = {}
            for pid in pids[:64]:
                try:
                    observed[pid] = identity(pid)
                except (OSError, ValueError) as error:
                    observed[pid] = f'unreadable:{type(error).__name__}'
            return observed
        before = snapshot(owners)
        inherited = self.inherited_shared_leases(key, processes=governed)
        after = snapshot(owners)
        self.sampled_inherited = inherited
        self.external_pids = set(owners) - governed
        # A command may take additional shared flocks while retaining its
        # scheduler descriptor. Count that governed lease once; unverifiable
        # raw locks keep their independent conservative reservations.
        raw = sum(owner not in governed for owner in owners)
        count = max(raw + len(inherited), len(set(held) | inherited))
        # Keep each observation beside its own count, bounded, so a failing
        # assertion shows what the counter saw for the number it rejected
        # rather than whichever sample happened to run last.
        kept = getattr(self, 'sampled_lock_inventories', [])
        kept.append((count, {
            'key': key, 'owners': owners[:64], 'governed': sorted(governed)[:64],
            'inherited': sorted(inherited)[:64], 'held': sorted(set(held))[:64],
            'raw': raw, 'count': count,
            'start_before': before, 'start_after': after,
        }))
        del kept[:-INVENTORY_LIMIT]
        self.sampled_lock_inventories = kept
        self.sampled_lock_inventory = kept[-1][1]
        return count

    def inventory_for(self, count):
        """The observation behind `count`, or None when no sample produced it."""
        for sampled, inventory in reversed(getattr(self, 'sampled_lock_inventories', [])):
            if sampled == count:
                return inventory
        return None

    def resources(self, tickets, runs, sampled_count, forest, hardware, limit):
        """Price the same live holders for admission and status under the mutex."""
        from heavy_hardware import Profiles
        import external_holders
        holders = runs.holders(tickets)
        held = {row['id'] for row in holders}
        folded = {row['pid'] for row in holders
                  if row['id'] in getattr(self, 'sampled_inherited', set())
                  and row['pid'] in getattr(self, 'external_pids', set())
                  and identity(row['pid']) == row['start']}
        if sampled_count is not None:
            sampled_count -= len(folded)
        if folded:
            self.external_pids.difference_update(folded)
        count = (None if sampled_count is None else max(sampled_count,
                 len(held | getattr(self, 'sampled_inherited', set()))))
        profiles = Profiles(self.directory, tickets, runs)
        profiles.observe(forest)
        external = (profiles.external(getattr(self, 'external_pids', set()), holders,
                    hardware, limit, forest) if count is not None else None)
        if external:
            count = max(count, len(holders) + external['count'])
        charged = external_holders.charged_count(count, external, limit)
        host = dict(hardware, load=external['load']) if external else hardware
        return profiles, external, count, charged, host

    def acquire(self, ticket, descriptor, handle, deadline, wait, reserve=None):
        import heavy_sampling
        last_blocker = None
        while True:
            # Process descriptor I/O must not own the metadata mutex: running
            # jobs need that short lock to record placement and finish cleanup.
            sampled_at = time.monotonic()
            try:
                sampled_count = self.shared_locks(handle)
            except BlockingIOError:
                sampled_count = None
            forest = heavy_sampling.processes()
            can_ram = (reserve is not None or any(Admission(self.directory).requests.values())) and ram_available()
            # Let brief contention finish after the queue deadline so expiry
            # retains its named position and cleanup can preserve retry credit.
            with self.locked(max(deadline, time.monotonic()+5)):
                if time.monotonic() - sampled_at > 2:
                    if time.monotonic() < deadline:
                        continue
                    raise TimeoutError('Heavy hardware sample aged before admission deadline')
                tickets, runs = self.state()
                holders = runs.holders(tickets)
                held = {t['id'] for t in holders}
                admission = Admission(self.directory)
                admission.requests[ticket['id']] = reserve is not None
                before = [t['class'] for t in tickets]
                pending = admission.pending(tickets, held, order)
                admission.save()
                if before != [t['class'] for t in tickets]:
                    self.save(tickets)
                    self.notify(tickets)
                remaining = deadline - time.monotonic()
                placements = Placements(self.directory, tickets)
                # One landing may use its reserved slot; every resource guard applies.
                ordinary_limit = self.shared_limit()
                limit = ordinary_limit + int(ticket['class'] == 0 or any(
                    row['class'] == 0 for row in holders))
                capable = self.capable_clients(tickets, ticket)
                hardware_clients = self.capable_clients(tickets, ticket, hardware=True)
                from heavy_hardware import refusal, ceiling
                legacy_limit = ceiling(self.directory, legacy=True)
                hardware = heavy_sampling.host()
                profiles, external, shared_count, charged_count, charged_hardware = self.resources(
                    tickets, runs, sampled_count, forest, hardware, limit)
                shareable = (charged_count is not None and len(holders) < limit and charged_count < limit and all(
                    runs.active.get(t['id'], {}).get('slot') == 'ram' or placements.shared(t)
                    for t in holders))
                def shared_request(row):
                    return (admission.requests.get(row['id'], False) and can_ram is not False
                            or placements.shared(row))
                modes = {}
                reasons = {}
                def projection(row, ram=False):
                    return dict(profiles.projection(row, ram),
                                root_writes=not ram and not placements.shared(row))
                def fits(row):
                    if shared_count is None:
                        return False
                    peers = profiles.peers(holders, shared_count, external)
                    ram = admission.requests.get(row['id'], False) and can_ram is not False
                    modes[row['id']], reasons[row['id']] = profiles.placement(row,
                        charged_hardware, peers, charged_count, limit, ram=ram, shared=placements.shared(row))
                    return reasons[row['id']] is None
                drain = Drain(self.directory, tickets)
                draining = None if shared_count is None else drain.update(
                    pending, holders, runs, shared_request, time.monotonic(), limit, shared_count)
                # An aged exclusive at the head of ordinary work admits no new
                # shared holder: shared arrivals must not keep refilling the
                # slot it waits for. Landing composition (class 0) still passes.
                head = next((t for t in pending if t['class'] != 0), None)
                waiting = (head['id'] if head is not None and not shared_request(head)
                           and head['id'] in drain.state['claims']
                           and time.monotonic() - head['enqueued_ns'] / 1e9 >= WAIT_SECONDS else None)
                def supported_slots(row):
                    # Protocol support is separate from learned resource history.
                    # A waiting old launcher must never fence a slot it cannot enter.
                    if row['id'] in hardware_clients:
                        return ordinary_limit + int(row['class'] == 0 or any(
                            held['class'] == 0 for held in holders))
                    if profiles.capable(row):
                        return min(ordinary_limit, legacy_limit)
                    return min(ordinary_limit, 4 if row['id'] in capable else 3)

                runnable = [t for t in pending
                            if (waiting is None or t['class'] == 0 or t['id'] == waiting)
                            and (not holders and shared_count == 0
                                 or shareable and shared_request(t)
                                 and (draining is None or t['class'] == 0)
                                 and max(len(holders), shared_count) < supported_slots(t))]
                if drain.shared_window(time.monotonic()):
                    runnable.sort(key=lambda row: (row['class'], not shared_request(row), order(row)))
                # Resource refusal keeps the eligible head in place. Only the
                # legacy slot-capability exception above may bypass a waiter.
                eligible = remaining > 0 and runnable and runnable[0]['id'] == ticket['id']
                if eligible and time.monotonic() - sampled_at > 2:
                    continue  # Never authorize execution with facts aged by pricing itself.
                resource_fits = fits(ticket) if eligible else False
                if eligible and reserve is not None and can_ram is None:
                    resource_fits = False
                    reasons[ticket['id']] = 'RAM target metadata is busy'
                if reasons.get(ticket['id']) is not None:
                    last_blocker = (
                        reasons[ticket['id']],
                        charged_hardware,
                        profiles.projection(ticket, modes.get(ticket['id'], False)),
                    )
                elif eligible and resource_fits:
                    last_blocker = None
                if eligible:
                    import disk_stall
                    try:
                        disk_stall.observe(self.directory, ticket, hardware, reasons.get(ticket['id']))
                    except (OSError, ValueError) as error:
                        # Monitoring failure is loud, but must not stop resource recovery.
                        import sys
                        print(f'Disk stall monitor failed: {error}', file=sys.stderr, flush=True)
                if eligible and resource_fits:
                    shared = shared_request(ticket)
                    try:
                        fcntl.flock(handle, (fcntl.LOCK_SH if shared else fcntl.LOCK_EX) | fcntl.LOCK_NB)
                    except BlockingIOError:
                        pass  # A legacy raw-flock job still owns the actual slot.
                    else:
                        reservation = None
                        admitted = reserve is None
                        if reserve is not None:
                            try:
                                if modes[ticket['id']]:
                                    reservation = reserve()
                                    admitted = True
                                elif placements.shared(ticket):
                                    admitted = True
                                else:
                                    raise ValueError('Hardware declined RAM placement')
                            except BlockingIOError:
                                pass  # Retry reservation contention without changing placement.
                            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError):
                                fallback_hardware = heavy_sampling.host()
                                disk_fits = refusal(fallback_hardware, projection(ticket),
                                                    profiles.peers(holders, shared_count), shared_count, limit,
                                                    weighted_cpu=fallback_hardware.get('weighted_cpu', False)) is None
                                if disk_fits and placements.shared(ticket):
                                    admitted = True  # NVMe fallback retains the shared flock.
                                elif disk_fits:
                                    fcntl.flock(handle, fcntl.LOCK_UN)
                                    if not holders:
                                        try:
                                            fcntl.flock(handle, fcntl.LOCK_EX | fcntl.LOCK_NB)
                                            admitted = True
                                        except BlockingIOError:
                                            pass
                        if not admitted:
                            fcntl.flock(handle, fcntl.LOCK_UN)
                        if admitted:
                            mode = 'ram' if reservation else 'disk'
                            runs.start(ticket, mode, reservation, tickets)
                            if mode == 'disk' and not placements.shared(ticket):
                                drain.exclusive_started(ticket, time.monotonic())
                            admission.pending(tickets, {t['id'] for t in runs.holders(tickets)}, order)
                            admission.save()
                            runs.anchor(tickets)
                            runs.save()
                            self.save(tickets)
                            self.notify(tickets)
                            return mode
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    position = next(i for i, t in enumerate(pending, 1) if t['id'] == ticket['id'])
                    actual = [dict(t, holder=t['id'] in held) for t in tickets]
                    reason = reasons.get(ticket['id'])
                    detail_host = charged_hardware
                    detail_candidate = profiles.projection(ticket, modes.get(ticket['id'], False))
                    fresh_fit_passed = False
                    if reason is None and runnable and runnable[0]['id'] == ticket['id']:
                        if reserve is not None and can_ram is None:
                            reason = 'RAM target metadata is busy'
                        else:
                            fits(ticket)
                            reason = reasons.get(ticket['id'])
                            detail_candidate = profiles.projection(ticket, modes.get(ticket['id'], False))
                            if reason is None:
                                fresh_fit_passed = True
                    if reason is None and not fresh_fit_passed and last_blocker is not None:
                        reason, detail_host, detail_candidate = last_blocker
                    detail = self.timeout_detail(
                        ticket, reason, detail_host, detail_candidate,
                        len(holders), limit, draining, waiting)
                    raise TimeoutError(f'heavy lock not acquired after {wait:g} s, '
                                       f'position {position} behind {self.holder(actual)}{detail}')
            # Notifications wake queued jobs immediately. The bounded fallback
            # detects SIGKILL, PID reuse and raw-flock releases without a daemon.
            if select.select([descriptor], [], [], min(1, max(0, remaining)))[0]:
                descriptor.recv(4096)

    def timeout_detail(self, ticket, reason, host, candidate, holder_count, limit, draining, waiting):
        if reason == 'load guard':
            cpu_limit = max(1, host['cores'] - max(1, math.ceil(host['cores'] / 12)))
            return (f'; blocked by load guard (load={host["load"]:.2f}, need={candidate["cpu"]:g}, '
                    f'cpu_limit={cpu_limit}, cores={host["cores"]}, holders={holder_count}/{limit})')
        if reason == 'available RAM and projected growth':
            from heavy_hardware import GIB
            return (f'; blocked by {reason} (avail={host["available"] / GIB:.1f}GiB, '
                    f'need={candidate["peak_rss"] / GIB:.1f}GiB, holders={holder_count}/{limit})')
        if reason == 'disk free-space floor':
            from heavy_hardware import GIB
            return (f'; blocked by {reason} (nvme_free={host["nvme_free"] / GIB:.1f}GiB, '
                    f'root_free={host["root_free"] / GIB:.1f}GiB)')
        if reason is not None:
            return f'; blocked by {reason} (holders={holder_count}/{limit})'
        if draining is not None:
            return f'; waiting for exclusive drain (holders={holder_count}/{limit})'
        if waiting is not None and waiting != ticket['id']:
            return f'; fenced behind aged exclusive waiter (holders={holder_count}/{limit})'
        return f'; holders={holder_count}/{limit}'

    def observe(self):
        """A running wrapper supplies samples even when no other ticket is waiting."""
        from heavy_hardware import Profiles
        import heavy_sampling
        sampled_at = time.monotonic()
        forest = heavy_sampling.processes()
        try:
            # Process census happens outside the mutex; lock contention skips telemetry.
            with self.locked(time.monotonic()):
                if time.monotonic() - sampled_at <= 2:
                    tickets, runs = self.state()
                    Profiles(self.directory, tickets, runs).observe(forest)
        except TimeoutError:
            pass

    def disk(self, ticket, descriptor, handle, wait, nvme=None):
        deadline = time.monotonic() + wait
        with self.locked(deadline):
            tickets, runs = self.state()
            placements = Placements(self.directory, tickets)
            if runs.active[ticket['id']]['slot'] != 'ram' and not placements.shared(ticket):
                return
            runs.pause(ticket)
            admission = Admission(self.directory)
            admission.requests[ticket['id']] = False
            placements = Placements(self.directory, tickets)
            placements.nvme.pop(ticket['id'], None)
            if nvme is not None:
                placements.nvme[ticket['id']] = str(nvme)
            placements.save()
            admission.pending(tickets, {t['id'] for t in runs.holders(tickets)}, order)
            admission.save()
            runs.anchor(tickets)
            runs.save()
            self.save(tickets)
            fcntl.flock(handle, fcntl.LOCK_UN)
            self.notify(tickets)
        self.acquire(ticket, descriptor, handle, deadline, wait)

    def lease(self, expected):
        # Writers atomically replace each file. A live owner's ticket and run
        # persist through shared/exclusive conversion, including its waiting row.
        # Reading them never needs admission's mutex or its reaping side effects.
        tickets = self.load()
        runs = Runs(self.directory, tickets)
        ticket = next((t for t in tickets if t['id'] == expected['id']), None)
        if (ticket is None or ticket['pid'] != expected['pid'] or ticket['start'] != expected['start']
                or ticket['id'] not in runs.active):
            raise ValueError('Inherited heavy ticket is not live')
        return ticket, dict(runs.active[ticket['id']])

    def descriptor_mode(self, descriptor):
        info = os.fstat(descriptor)
        key = f'{os.major(info.st_dev):02x}:{os.minor(info.st_dev):02x}:{info.st_ino}'
        with Path(f'/proc/self/fdinfo/{descriptor}').open() as stream:
            raw = stream.read(65537)
        if len(raw) > 65536:
            raise ValueError('Heavy descriptor inventory exceeds its bound')
        return next((fields[4] for fields in map(str.split, raw.splitlines())
                     if len(fields) > 6 and fields[0] == 'lock:'
                     and fields[2:4] == ['FLOCK', 'ADVISORY'] and fields[6] == key), None)

    def exclusive_budget(self, ticket, descriptor=None):
        if descriptor is None:
            # Already-loaded launchers retain their original call signature
            # during atomic installation of the new queue and launcher files.
            with self.locked(time.monotonic() + 5):
                tickets, runs = self.state()
                row = runs.active.get(ticket['id'])
                claim = Drain(self.directory, tickets).state['claims'].get(ticket['id'])
                if (row is None or claim is None or row['slot'] != 'disk'
                        or Placements(self.directory, tickets).shared(ticket)):
                    return None
                return row['since'], claim['max_hold']
        # Observe the kernel mode BEFORE the atomic run snapshot. Conversion
        # publishes waiting before unlocking, and active before running a child.
        # Thus a WRITE lock cannot pair with a stale shared run's start time.
        mode = self.descriptor_mode(descriptor)
        _, row = self.lease(ticket)
        if mode is None and row['slot'] != 'waiting':
            # An admission may have completed between the two observations.
            mode = self.descriptor_mode(descriptor)
            _, row = self.lease(ticket)
            if mode is None and row['slot'] != 'waiting':
                raise ValueError('Inherited heavy kernel lock is not held')
        claim = Drain(self.directory, [ticket]).state['claims'].get(ticket['id'])
        if mode != 'WRITE' or row['slot'] != 'disk' or claim is None:
            return None
        return row['since'], claim['max_hold']

    def refund(self, ticket):
        with self.locked(time.monotonic() + 5):
            tickets, _ = self.state()
            Drain(self.directory, tickets).refund(ticket)

    def target(self, ticket, mode):
        if mode not in ('ram', 'disk'):
            raise ValueError('Unknown heavy target mode')
        with self.locked(time.monotonic() + 5):
            _, runs = self.state()
            targets = runs.active[ticket['id']]['targets']
            if mode not in targets:
                targets.append(mode)
                runs.save()
