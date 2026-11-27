"""Reclaim only idle one-off evidence/date directories in the SE Code cache.

This guards against deleting in-use work by accident. A hostile process running
as the same user is out of scope: it could delete the target itself.
"""
import os
from pathlib import Path
import re
import stat
import subprocess
import sys

import evict_targets as targets
import cargo_observation


ONE_OFF = re.compile(r'.+(?:-evidence|-20[0-9]{6})')


PTRACE_GATED = {'cwd', 'exe', 'environ', 'descriptors'}

APPROVED_SERVICES = (
    # (comm, accepted argv[0] spellings, argv[0] carries a title after the spelling)
    (b'systemd', ((b'/usr/lib/systemd/systemd', b'--user'),), False),
    (b'(sd-pam)', ((b'(sd-pam)',),), False),
    (b'ssh-agent', ((b'/usr/bin/ssh-agent',), (b'ssh-agent',)), False),
    # sshd-session retitles argv[0] as "sshd-session: <peer>", so its accepted
    # spelling is the literal prefix and the title after it must be non-empty.
    (b'sshd-session', (b'sshd-session: ',), True),
)


def approved_service(process, arguments):
    """The closed set of services that hide their kernel sources by design.

    The exemption is identified by comm and argv alone. It is never reached
    unless every ptrace-gated source was hidden together, and the executable
    link is one of those sources, so there is no readable exe to compare here.
    Matching is exact -- comm equal, argv a listed spelling or a listed prefix
    with a non-empty title after it -- so no lookalike inherits it.
    """
    try:
        with (process/'comm').open('rb') as handle:
            comm = handle.read(257).removesuffix(b'\n')
        if len(comm) > 256 or not arguments:
            return None
        for expected_comm, prefixes, needs_title in APPROVED_SERVICES:
            if comm != expected_comm:
                continue
            for prefix in prefixes:
                if needs_title:
                    # A retitled argv[0] must begin with this prefix and carry a
                    # real title after it, not just the separator's whitespace.
                    if not arguments[0].startswith(prefix):
                        continue
                    title = arguments[0][len(prefix):].strip()
                    if not title:
                        continue
                elif tuple(arguments[:len(prefix)]) != prefix:
                    continue
                return os.fsdecode(expected_comm)
    except (OSError, ValueError, UnicodeError):
        return None
    return None


def process_values(path):
    with path.open('rb') as handle:
        raw = handle.read(1024 * 1024 + 1)
    if len(raw) > 1024 * 1024:
        raise ValueError('Process metadata exceeds its read bound')
    return raw.split(b'\0')


def candidates(cache, home, owners, deadline):
    if cache != home / '.cache/secode' or cache.is_symlink():
        return []
    result = []
    for entry in targets.bounded_entries(cache, targets.MAX_ROOT_ENTRIES, deadline):
        path = Path(entry.path)
        if (entry.is_dir(follow_symlinks=False) and ONE_OFF.fullmatch(entry.name) and 'qwen' not in entry.name.lower()
                and not any(path == owner or path in owner.parents for owner in owners.values())):
            result.append(path)
    return result


def idle(path, now, deadline):
    """Bound metadata reads; preserve repositories, ownership pins and model weights."""
    pending = [path]
    device = path.stat().st_dev
    count = 0
    while pending:
        directory = pending.pop()
        if (directory/'HEAD').is_file() and (directory/'objects').is_dir():
            return False  # Bare Git repositories have no .git child.
        for entry in targets.bounded_entries(directory, targets.MAX_TARGET_ENTRIES, deadline):
            count += 1
            if count > targets.MAX_TARGET_ENTRIES:
                raise RuntimeError('One-off cache inventory exceeds its entry bound')
            name = entry.name.lower()
            if (name in {'.git', '.secode-retain', 'models', 'weights'} or 'qwen' in name
                    or name.endswith(('.gguf', '.safetensors', '.bin', '.pt', '.pth'))):
                return False
            info = entry.stat(follow_symlinks=False)
            if info.st_dev != device or now - info.st_mtime < targets.RECENT_SECONDS:
                return False
            if name == '.secode-worktree':
                if not stat.S_ISREG(info.st_mode) or info.st_size > 4096:
                    return False
                owner = Path(Path(entry.path).read_text().strip())
                if not owner.is_absolute() or owner.exists():
                    return False
            if stat.S_ISDIR(info.st_mode):
                pending.append(Path(entry.path))
    return now - path.stat().st_mtime >= targets.RECENT_SECONDS


def _refuse(sink, reason):
    """Record why a census went blind, without changing its None contract."""
    if sink is not None:
        sink.append(reason)


def active(paths, deadline, proc, refused=None):
    """A queued script may name a cache directory while its cwd is elsewhere.

    Returns the candidate paths demonstrably referenced, or None when any process
    could not be read completely and every candidate must be spared.

    Partial readability is not evidence. A live child can hold its working
    directory in one candidate and an open file in another; if its descriptor
    census is unreadable, protecting only the directory it names would delete the
    file it is still using. So the rule is per process and all or nothing: a
    process is attributed only when every source the scanner reads -- status,
    cwd, exe, arguments, environment and descriptors -- was readable. Anything
    unreadable makes it blind, and every candidate is spared for this pass. The
    pass still runs, which is what the old whole-pass abort cost.
    """
    protected = set()
    blind = False
    for entry in targets.bounded_entries(proc, 32768, deadline):
        if not entry.name.isdecimal():
            continue
        process = Path(entry.path)
        try:
            if process.stat().st_uid != os.getuid():
                continue
            if (process/'stat').read_text().rsplit(')', 1)[-1].split()[0] in {'Z', 'X'}:
                if cargo_observation.exited(process, deadline):
                    continue
                _refuse(refused, f'pid {process.name} was exiting or unreadable')
                blind = True
                continue
            values = []
            executable = None
            hidden = []
            for name in ['cwd', 'exe']:
                try:
                    link = os.readlink(process/name)
                    values.append(os.fsencode(link))
                    if name == 'exe':
                        executable = link
                except PermissionError:
                    hidden.append(name)
            arguments = process_values(process/'cmdline')
            values.extend(arguments)
            # cmdline is always readable, and a target path in it protects the
            # candidate whatever else is hidden.
            try:
                values.extend(process_values(process/'environ'))
            except PermissionError:
                hidden.append('environ')
            try:
                for descriptor in targets.bounded_entries(process/'fd', 4096, deadline):
                    try:
                        values.append(os.fsencode(os.readlink(descriptor.path).removesuffix(' (deleted)')))
                    except FileNotFoundError:
                        continue  # A live process can close a descriptor during the scan.
                    except PermissionError:
                        if 'descriptors' not in hidden:
                            hidden.append('descriptors')
            except PermissionError:
                if 'descriptors' not in hidden:
                    hidden.append('descriptors')
            # A name is not an identity. The exemption belongs to the kernel
            # signature of a non-dumpable process: every ptrace-gated source --
            # cwd, exe, environ and the descriptor census -- fails together with a
            # permission error. That all-hidden pattern is what these services
            # look like, and it is also what identifies them when the exe link
            # itself is unreadable. A process that hides only some sources is
            # blind, whether or not its name is on the list, because a spoofed
            # name would otherwise license deleting what its readable half never
            # named.
            all_hidden = set(hidden) == PTRACE_GATED
            # The exe link is one of the gated sources, so an exemption always
            # arrives with it unreadable: comm and argv are what remain.
            service = approved_service(process, arguments) if all_hidden else None
            exempt = service is not None
            if exempt:
                # An exemption is a judgement, so every pass that uses one says
                # which process and which sources, making an unexpected match
                # visible instead of silent.
                print(f'Approved service {process.name} ({service}) exempt for: '
                      f'{", ".join(hidden)}', file=sys.stderr)
            elif hidden:
                # Say the real reason. When every gated source was hidden but the
                # identity was not approved, the hide IS the non-dumpable
                # signature and the refusal is the unapproved identity; when only
                # some sources were hidden there is no signature at all. Claiming
                # "not the signature" for the first case misled a reader into
                # looking for a partial hide that was not there.
                if all_hidden:
                    reason = (f'pid {process.name} hid every gated source '
                              f'({", ".join(sorted(hidden))}) but is not an approved '
                              f'service, so its identity is unproven')
                else:
                    reason = (f'pid {process.name} hid {", ".join(sorted(hidden))}, '
                              f'which is not the non-dumpable signature')
                _refuse(refused, reason)
                blind = True
                continue
            for path in paths:
                raw = os.fsencode(path)
                if any(value == raw or raw + b'/' in value or b'=' + raw in value
                       for value in values):
                    protected.add(path)
        except (FileNotFoundError, ProcessLookupError):
            if not cargo_observation.exited(process, deadline):
                _refuse(refused, f'pid {process.name} vanished while being read')
                blind = True
        except (OSError, ValueError, IndexError) as error:
            _refuse(refused, f'pid {process.name} was unreadable: '
                             f'{cargo_observation.describe(error)}')
            blind = True
    targets.remaining(deadline)
    return None if blind else protected


def reclaim(cache, home, owners, dry_run, now, deadline, proc):
    paths = candidates(cache, home, owners, deadline)
    if not paths:
        return []
    # An unattributable process spares the candidates that remain; it never
    # aborts the pass, because one process mid-exit used to cost the whole
    # hour's reclamation of every one-off directory. None is "unknown" and must
    # not be confused with an empty set, which means "nothing is protected".
    protected = active(paths, deadline, proc)
    if protected is None:
        protected = set(paths)
    removed = []
    for path in sorted(paths):
        targets.remaining(deadline)
        if path in protected or not idle(path, now, deadline):
            continue
        original = path.lstat()
        again = active(paths, deadline, proc)
        if again is None:
            again = set(paths)
        current = path.lstat()
        if (path in again or not stat.S_ISDIR(current.st_mode)
                or (original.st_dev, original.st_ino) != (current.st_dev, current.st_ino)
                or not idle(path, now, deadline)):
            continue
        if dry_run:
            print(f'Would remove {path}')
        else:
            subprocess.run(['rm', '-rf', '--one-file-system', '--', str(path)],
                           check=True, timeout=targets.remaining(deadline))
            if path.exists():
                raise RuntimeError(f'Incomplete one-off cache cleanup: {path}')
            print(f'Removed {path}', flush=True)
        removed.append(path)
    return removed
