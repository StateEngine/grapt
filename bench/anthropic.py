"""Direct Claude routing and a separate, bounded monthly spend record."""
from datetime import datetime, timezone
from decimal import Decimal
import fcntl
import json
import os
from pathlib import Path
import re
import stat
import sys
import tempfile

from budget import BudgetStop, amount, key_material
from capture import redact_bytes, redact_value


def adapters_for_route(adapters, route, model):
    if route not in {'anthropic-direct', 'openrouter'}:
        raise ValueError('Unsupported Claude benchmark route')
    if not re.fullmatch(r'[a-z0-9][a-z0-9.-]{0,127}', model):
        raise ValueError('Invalid direct Anthropic model ID')
    result = {}
    for name, original in adapters.items():
        row = dict(original)
        if row['auth'] == 'openrouter' and route == 'anthropic-direct':
            selected = 'anthropic/' + model
            row.update(auth='anthropic-api', model=selected, cohort='haiku-direct',
                       route='anthropic-direct')
            row['argv'] = [arg.replace(original['model'], selected) for arg in original['argv']]
        elif row['auth'] == 'openrouter':
            row['route'] = 'openrouter-compatible'
        result[name] = row
    return result


def environment(key_file, env):
    try:
        key = key_material(Path(key_file))
    except (OSError, ValueError, BudgetStop):
        raise BudgetStop('Anthropic key must be a private regular file within4KB') from None
    if not key.startswith('sk-ant-') or key.startswith('sk-ant-oat'):
        raise BudgetStop('Anthropic benchmark requires an API key, not OAuth')
    env['ANTHROPIC_API_KEY'] = key


def redact_evidence(root):
    """Check the disposable row, including stderr, diffs and agent files.

    A secret emitted as source invalidates the row. Scrub it before preserving
    failure evidence; native stdout/stderr is already redacted during capture.
    """
    count, total, redacted = 0, 0, False

    def private_name(path):
        nonlocal redacted
        masked = redact_value(path.name)
        if masked == path.name:
            return path
        destination = path.with_name(masked)
        for index in range(10000):
            if not destination.exists() and not destination.is_symlink():
                path.rename(destination)
                redacted = True
                return destination
            destination = path.with_name(f'redacted-entry-{index}')
        raise RuntimeError('Direct Anthropic redacted pathname bound reached')

    def unreadable(_error):
        raise RuntimeError('Direct Anthropic evidence directory is unreadable')

    for parent, directories, files in os.walk(root, topdown=True, followlinks=False, onerror=unreadable):
        count += len(directories) + len(files)
        if count > 10000:
            raise RuntimeError('Direct Anthropic evidence enumeration exceeds its bound')
        for index in range(len(directories)-1, -1, -1):
            path = Path(parent)/directories[index]
            if path.is_symlink():
                path.unlink()
                directories.pop(index)
                redacted = True
            else:
                directories[index] = private_name(path).name
        for name in files:
            path = Path(parent)/name
            metadata = path.lstat()
            if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                # FIFOs, sockets, links and devices are never opened or followed.
                path.unlink()
                redacted = True
                continue
            path = private_name(path)
            fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
            with os.fdopen(fd, 'rb') as file:
                metadata = os.fstat(file.fileno())
                if not stat.S_ISREG(metadata.st_mode) or metadata.st_nlink != 1:
                    raise RuntimeError('Direct Anthropic evidence changed file type')
                size = metadata.st_size
                if size > 32*1024*1024:
                    raise RuntimeError('Direct Anthropic evidence file exceeds its bound')
                body = file.read(32*1024*1024+1)
            total += size
            if len(body) > 32*1024*1024 or total > 256*1024*1024:
                raise RuntimeError('Direct Anthropic evidence bytes exceed their bound')
            masked = redact_bytes(body)
            if masked != body:
                # Replace this path, never write through a changed symlink/link.
                with tempfile.NamedTemporaryFile(dir=path.parent, delete=False) as output:
                    temporary = Path(output.name)
                    try:
                        output.write(masked)
                        output.close()
                        temporary.replace(path)
                    finally:
                        temporary.unlink(missing_ok=True)
                redacted = True
    return redacted


def discard_evidence(root):
    """Remove only this disposable row, repairing agent-changed permissions.

    All descendant operations use directory descriptors. Links and special files
    are unlinked without opening them; no permissions outside the row are changed.
    """
    root = Path(root)
    count = 0
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_NONBLOCK

    def remove(parent, name, depth):
        nonlocal count
        count += 1
        if count > 10000 or depth > 64:
            raise RuntimeError('Direct Anthropic evidence disposal exceeds its bound')
        metadata = os.stat(name, dir_fd=parent, follow_symlinks=False)
        if not stat.S_ISDIR(metadata.st_mode):
            os.unlink(name, dir_fd=parent)
            return
        try:
            directory = os.open(name, flags, dir_fd=parent)
        except PermissionError:
            # chmod refuses a replacement symlink rather than following it.
            os.chmod(name, 0o700, dir_fd=parent, follow_symlinks=False)
            directory = os.open(name, flags, dir_fd=parent)
        try:
            os.fchmod(directory, 0o700)
            with os.scandir(directory) as entries:
                for entry in entries:
                    remove(directory, entry.name, depth+1)
        finally:
            os.close(directory)
        os.rmdir(name, dir_fd=parent)

    parent = os.open(root.parent, flags)
    try:
        remove(parent, root.name, 0)
    finally:
        os.close(parent)


def quarantine_directory(evidence):
    """Prepare private containment on the row's filesystem before reading a key."""
    evidence = Path(evidence).resolve()
    evidence.mkdir(parents=True, exist_ok=True)
    candidates = [Path.home()/'.cache/grapt/bench-quarantine',
                  Path(tempfile.gettempdir())/f'grapt-bench-quarantine-{os.getuid()}']
    for path in candidates:
        if path.is_relative_to(evidence):
            continue
        path.mkdir(parents=True, exist_ok=True, mode=0o700)
        metadata = path.lstat()
        if (not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid()
                or metadata.st_mode & 0o777 != 0o700):
            raise RuntimeError('Direct Anthropic quarantine must be an owned private directory')
        if metadata.st_dev == evidence.stat().st_dev:
            return path
    raise RuntimeError('Direct Anthropic evidence needs same-filesystem private quarantine')


def contain_evidence(row, quarantine):
    """Dispose normally, or atomically move unremovable data outside evidence."""
    try:
        discard_evidence(row)
        return
    except (OSError, ValueError, RuntimeError):
        pass
    container = Path(tempfile.mkdtemp(prefix='row-', dir=quarantine))
    isolated = container/'evidence'
    Path(row).rename(isolated)
    try:
        discard_evidence(isolated)
        container.rmdir()
    except (OSError, ValueError, RuntimeError):
        print(f'Direct Anthropic row quarantined for manual purge: {container}', file=sys.stderr)


class MonthlySpend:
    """Known spend is a running total; missing billing stays unknown.

    The provider enforces the $200 cap with no payment card. This record
    never invents a price from token counts or resets the OpenRouter baseline.
    """
    def __init__(self, path, month=None):
        self.path = Path(path)
        self.month = month or datetime.now(timezone.utc).strftime('%Y-%m')

    def _update(self, record=None):
        self.path.parent.mkdir(parents=True, exist_ok=True, mode=0o700)
        if self.path.is_symlink():
            raise BudgetStop('Anthropic monthly ledger cannot be a symlink')
        fd = os.open(self.path, os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW, 0o600)
        with os.fdopen(fd, 'r+', encoding='utf-8') as file:
            os.fchmod(file.fileno(), 0o600)
            fcntl.flock(file, fcntl.LOCK_EX)
            file.seek(0)
            raw = file.read(1024*1024+1)
            if len(raw) > 1024*1024:
                raise BudgetStop('Anthropic monthly ledger exceeds1MB')
            value = json.loads(raw) if raw else {
                'provider': 'anthropic-direct', 'month': self.month,
                'cap_usd': 200, 'billed_total_usd': '0', 'rows': []}
            if (value.get('provider') != 'anthropic-direct' or value.get('month') != self.month
                    or value.get('cap_usd') != 200 or not isinstance(value.get('rows'), list)):
                raise BudgetStop('Anthropic monthly ledger identity differs')
            total = amount(value['billed_total_usd'])
            if record is None and (total >= Decimal('200') or value.get('provider_exhausted')):
                raise BudgetStop('Anthropic monthly cap exhausted; Claude rows stop until next month')
            if record is not None:
                if len(value['rows']) >= 4096:
                    raise BudgetStop('Anthropic monthly ledger row bound reached')
                value['rows'].append({
                    'task': record['task'], 'harness': record['harness'],
                    'status': record['status'], 'billed_usd': None,
                    'api_equivalent_cost_usd': record.get('api_equivalent_cost_usd'),
                    'billing': 'unknown; reconcile with provider billing'})
            file.seek(0)
            file.write(json.dumps(value)+'\n')
            file.truncate()
            file.flush()
            os.fsync(file.fileno())
            return {'route': 'anthropic-direct', 'month': self.month,
                    'billed_total_usd': str(total), 'unreconciled_rows': sum(
                        row.get('billed_usd') is None for row in value['rows'])}

    def check(self):
        return self._update()

    def record(self, result):
        return self._update(result)
