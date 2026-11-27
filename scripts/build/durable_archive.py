"""Retain stopped job records outside the bounded live submission index."""
import argparse
import fcntl
import json
import math
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import time
import durable_jobs as jobs


def pinned(path):
    try:
        (path/'retain-for-receipt').lstat()
        return True
    except FileNotFoundError:
        return False


def archive(token):
    """Move one explicitly selected joined job; retain its original records."""
    path = jobs.directory(token)
    destination = jobs.JOBS.parent/'completed-jobs'/token
    destination.parent.mkdir(mode=0o700, exist_ok=True)
    info = destination.parent.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Archive requires a private owned directory')
    if path == destination:
        return path
    flags = os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK
    descriptor = os.open(jobs.JOBS.parent/'completed-jobs.lock', flags, 0o600)
    with os.fdopen(descriptor, 'r+') as mutex:
        info = os.fstat(mutex.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError('Archive requires a private owned mutex')
        fcntl.flock(mutex, fcntl.LOCK_EX | fcntl.LOCK_NB)
        path = jobs.directory(token)
        if path == destination:
            return path
        descriptor = os.open(path/'lifecycle.lock', os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as lifecycle:
            info = os.fstat(lifecycle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid():
                raise ValueError('Archive requires the owned lifecycle lock')
            fcntl.flock(lifecycle, fcntl.LOCK_EX | fcntl.LOCK_NB)
            if pinned(path):
                raise ValueError('Preserve a job retained for a pending receipt')
            request = jobs.request_for(path, token)
            result = jobs.read(path/'result.json')
            if type(result) is not dict:
                raise ValueError('Archive requires a matching terminal result at least one minute old')
            finished = result.get('finished_unix')
            if (not isinstance(request.get('source'), str) or not re.fullmatch('[0-9a-f]{40}', request['source'])
                    or not isinstance(request.get('owner'), str) or not 0 < len(request['owner']) <= 256
                    or result.get('job') != token or result.get('source') != request.get('source')
                    or result.get('owner') != request.get('owner') or type(result.get('exit')) is not int
                    or not 0 <= result['exit'] <= 255 or type(finished) not in (int, float)
                    or not math.isfinite(finished) or not 60 <= time.time()-finished
                    or time.time()-(path/'result.json').stat().st_mtime < 60):
                raise ValueError('Archive requires a matching terminal result at least one minute old')
            if jobs.service_state(request['unit']) not in ('inactive', 'failed'):
                raise ValueError('Archive refuses a service that has not stopped')
            if destination.exists() or destination.is_symlink():
                raise FileExistsError('Preserve the existing archived job: '+str(destination))
            path.rename(destination)
            for parent in (jobs.JOBS, destination.parent):
                descriptor = os.open(parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
                try: os.fsync(descriptor)
                finally: os.close(descriptor)
    return destination

def eligible(path, now, heads):
    """Keep current proofs; only expired or superseded failures move automatically."""
    if pinned(path):
        return False
    try:
        info = (path/'result.json').lstat()
    except FileNotFoundError:
        return False  # An unfinished job has no terminal record.
    result = jobs.read(path/'result.json')
    if type(result) is not dict:
        raise ValueError('Invalid terminal result; preserve it')
    finished = result.get('finished_unix')
    if (type(finished) not in (int, float) or not math.isfinite(finished)
            or type(result.get('exit')) is not int or not 0 <= result['exit'] <= 255):
        raise ValueError('Invalid terminal result; preserve it')
    age = min(now-finished, now-info.st_mtime)
    if age >= 6*3600:
        return True
    if result['exit'] == 0:
        return age >= 2*3600
    if age < 3600:
        return False
    request = jobs.request_for(path, path.name)
    cwd = request.get('cwd')
    if not isinstance(cwd, str) or not 0 < len(cwd) <= 4096 or not Path(cwd).is_absolute():
        raise ValueError('Unknown checkout; preserve its terminal record')
    if cwd not in heads:
        try:
            Path(cwd).stat()
        except FileNotFoundError:
            heads[cwd] = None
        else:
            probe = subprocess.run(['git', '-C', cwd, 'rev-parse', 'HEAD'],
                                   stdout=subprocess.PIPE, stderr=subprocess.DEVNULL,
                                   text=True, timeout=3)
            if probe.returncode or not re.fullmatch('[0-9a-f]{40}\n?', probe.stdout):
                raise ValueError('Unreadable checkout HEAD; preserve its terminal record')
            heads[cwd] = probe.stdout.strip()
    return heads[cwd] != request.get('source')


def pressure(count, actor, now):
    if count < 224:
        return
    body = (f'Durable job history has {max(0, 256-count)} slots free. '
            'Automatic archival preserves active, recent, current-source and receipt-pinned jobs; '
            'inspect retained records before the submission index fills.')
    print(body, file=sys.stderr, flush=True)
    path = jobs.JOBS.parent/'job-history-pressure.json'
    try:
        previous = jobs.read(path)
    except FileNotFoundError:
        previous = {'attempted': 0, 'sent': 0}
    if (not isinstance(previous, dict) or set(previous) != {'attempted', 'sent'}
            or any(type(value) not in (int, float) or not math.isfinite(value)
                   or value < 0 for value in previous.values())):
        raise ValueError('Preserve invalid job-history notification state')
    if now-previous['attempted'] < 300 or now-previous['sent'] < 1800:
        return
    previous['attempted'] = now
    jobs.write(path, previous)
    from nightly_revert import registry, send
    _agents, coordinator = registry()
    send(actor, coordinator, body, timeout=20)
    previous['sent'] = now
    jobs.write(path, previous)


def sweep(*, apply=False, actor=None):
    if apply and (not isinstance(actor, str) or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', actor)):
        raise ValueError('Automatic archival requires an explicit notification actor')
    info = jobs.JOBS.lstat()
    if not stat.S_ISDIR(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
        raise ValueError('Preserve an unowned or shared job index')
    descriptor = os.open(jobs.JOBS.parent/'job-history-sweep.lock',
                         os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_NONBLOCK, 0o600)
    with os.fdopen(descriptor, 'r+') as mutex:
        info = os.fstat(mutex.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
            raise ValueError('Maintenance requires a private owned mutex')
        fcntl.flock(mutex, fcntl.LOCK_EX | fcntl.LOCK_NB)
        with os.scandir(jobs.JOBS) as entries:
            paths = []
            for entry in entries:
                if len(paths) >= 512:
                    raise ValueError('Job inventory exceeds its inspection bound; preserve it')
                paths.append(Path(entry.path))
        now = time.time(); deadline = time.monotonic()+30
        # Oldest terminal records first, so the move bound frees the
        # longest-held slots.
        def oldest_first(path):
            try:
                return (0, (path/'result.json').stat().st_mtime)
            except OSError:
                try:
                    return (1, path.stat().st_mtime)
                except OSError:
                    # A vanished record sorts oldest; the sweep's error
                    # path reports it and the scan continues.
                    return (1, 0.0)
        moved = []; errors = []; heads = {}
        for path in sorted(paths, key=oldest_first):
            if len(moved) >= 32 or time.monotonic() >= deadline:
                break
            try:
                # Staging and foreign names are the admission path's to
                # clean; the sweep only inspects live job records.
                if not re.fullmatch('[0-9a-f]{32}', path.name):
                    continue
                if jobs.directory(path.name) != path or not eligible(path, now, heads):
                    continue
                if apply:
                    archive(path.name)
                moved.append(path.name)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                if len(errors) < 8:
                    errors.append(path.name+': '+str(error)[:256])
        # Count afresh: concurrent submissions also consume the legacy index.
        with os.scandir(jobs.JOBS) as entries:
            count = 0
            for _ in entries:
                count += 1
                if count > 512:
                    raise ValueError('Job inventory grew beyond its inspection bound')
        if apply:
            try:
                pressure(count, actor, now)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                errors.append('Capacity notification: '+str(error)[:256])
        return dict(apply=apply, selected=moved, retained=count, errors=errors)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--apply', action='store_true')
    parser.add_argument('--agent-id')
    args = parser.parse_args()
    print(json.dumps(sweep(apply=args.apply, actor=args.agent_id)), flush=True)


if __name__ == '__main__':
    main()
