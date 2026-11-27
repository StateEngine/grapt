#!/usr/bin/env python3
"""Reclaim idle or landed build targets, preserving live work and unknown content."""
from operator_paths import data_root
import argparse
import fcntl
import json
import os
from pathlib import Path
import re
import shutil
import subprocess
import sys
import tempfile
import time
import evict_targets as targets
import clean_journal
import hygiene_outcomes
import orphan_cache
import ram_targets
import target_discovery
import duplicate_targets
import disk_stall


IDLE_SECONDS = 3600
RUN_SECONDS = 900
# A measurement is telemetry; it never draws on the time a removal needs.
MEASURE_SECONDS = 5
FINISHED = targets.FINISHED


def discover(cache, home, deadline, qualification=None):
    return target_discovery.discover(cache, home, deadline, qualification)


def disposable(target, deadline):
    return targets.disposable(target, deadline, retain_qualification=False)


def mark_finished(target, owner, commit):
    recorded, _ = targets.marker_owner(target)
    actual = subprocess.check_output(['git', '-C', str(owner), 'rev-parse', 'HEAD'], timeout=10).decode().strip()
    if recorded != owner.resolve() or actual != commit:
        raise RuntimeError('Finished marker differs from the landed source or target owner')
    (target / FINISHED).write_text(commit + '\n')


def active_targets(owners, deadline, proc):
    # A process can exit during observation. Retry complete fresh censuses,
    # never partial evidence, within the existing maintenance deadline.
    #
    # A refusal that cannot say why is not evidence of anything, so each census
    # records its cause and the raise carries them. The decision is unchanged:
    # three fresh attempts, then refuse. Only the explanation improves.
    reasons = []
    for _attempt in range(3):
        targets.remaining(deadline)
        attempt = []
        builds = targets.live_targets(owners, proc=proc, deadline=deadline, refused=attempt)
        references = orphan_cache.active(owners, deadline, proc=proc, refused=attempt)
        if builds is not None and references is not None:
            return builds | references
        reasons = attempt
    cause = '; '.join(reasons) if reasons else 'no cause was recorded by either census'
    raise RuntimeError('Unknown live Cargo process or target reference; stopping before '
                       f'further target removal ({cause})')


def oldest_first(item):
    try:
        return item[0].lstat().st_mtime, str(item[0])
    except FileNotFoundError:
        return float('-inf'), str(item[0])


def measured(target, dry_run=False):
    """The target's size for the journal, or None when telemetry cannot read it.

    This is evidence, not a gate, and it gets its own small budget: drawing on
    the removal deadline meant a slow measurement could spend the time the
    removal needed, so a timeout both lost the number and aborted eligible work.
    A dry run records no rows, so it measures nothing at all.
    """
    if dry_run:
        return None
    try:
        return targets.usage(target, time.monotonic() + MEASURE_SECONDS)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Target measurement unavailable for {target}: {error}', file=sys.stderr)
        return None


def reclaim_owned_targets(cache, owners, protected, removed, decided, deadline, now, dry_run,
                          proc):
    """Walk the owned targets once, appending each decision and each removal."""
    with targets.admission_lock(cache, deadline):
        for target, owner in sorted(owners.items(), key=oldest_first):
            try:
                targets.remaining(deadline)
                if target in protected:
                    duplicate_targets.prune_owned(target, owner, owners, deadline, now, dry_run,
                                                  proc=proc)
                    decided.append(clean_journal.row(
                        target, 'live', detail='a live Cargo process references it'))
                    if dry_run:
                        print(f'Preserved live target {target}')
                    continue
                if not disposable(target, deadline):
                    decided.append(clean_journal.row(
                        target, 'unknown', detail='unknown target content'))
                    if dry_run:
                        print(f'Preserved unknown target content {target}')
                    continue
                original = target.lstat()
                try:
                    held = targets.lease(target)
                except BlockingIOError:
                    decided.append(clean_journal.row(
                        target, 'leased', detail='a shared lease is held'))
                    if dry_run:
                        print(f'Preserved leased target {target}')
                    continue
                try:
                    # Every skip below records why. A silent `continue` left a
                    # target with no journal entry at all, which read as if the
                    # pass never considered it.
                    if not target.is_dir() or target.is_symlink() or not disposable(target, deadline):
                        decided.append(clean_journal.row(
                            target, 'unknown', detail='changed or unknown content under the lease'))
                        continue
                    active = active_targets(owners, deadline, proc)
                    if target in active:
                        decided.append(clean_journal.row(
                            target, 'live', detail='became live under the lease'))
                        continue
                    observed = target.lstat()
                    if (original.st_dev, original.st_ino) != (observed.st_dev, observed.st_ino):
                        decided.append(clean_journal.row(
                            target, 'unknown', detail='replaced under the lease'))
                        continue
                    if not target_discovery.still_owned(target, owner) or not disposable(target, deadline):
                        decided.append(clean_journal.row(
                            target, 'unknown', detail='ownership or content changed under the lease'))
                        continue
                    latest = targets.latest_modified(target, deadline)
                    landed = targets.finished(target, owner, latest)
                    if not landed and now - latest < IDLE_SECONDS:
                        duplicate_targets.prune(target, deadline, now, dry_run)
                        decided.append(clean_journal.row(
                            target, 'retained', bytes_before=measured(target, dry_run),
                            detail=f'idle {int(now - latest)}s, threshold {IDLE_SECONDS}s'))
                        if dry_run:
                            print(f'Retained recent target {target}')
                        continue
                    # Measured before removal, and never guessed afterwards.
                    before = measured(target, dry_run)
                    if dry_run:
                        print(f'Would remove {target}')
                    else:
                        subprocess.run(['rm', '-rf', '--one-file-system', '--', str(target)],
                                       check=True, timeout=targets.remaining(deadline))
                        if target.exists():
                            raise RuntimeError(f'Incomplete target cleanup: {target}')
                        print(f'Removed {target}', flush=True)
                    decided.append(clean_journal.row(
                        target, 'removed', bytes_before=before,
                        bytes_after=0, detail='landed' if landed else 'idle'))
                    removed.append(target)
                finally:
                    held.close()
            except FileNotFoundError:
                if not targets.target_missing(target):
                    raise  # A missing internal file is still an attribution failure.


def unrecognised_kinds(cache, decided, deadline, dry_run):
    """Name every directory under target/<toolchain>/ that no pass could own.

    A quarantine or any future kind sits inside the cache and counts toward the
    cap, yet nothing enumerated it, so it could grow unseen while every report
    said "protected or unmanaged". This never deletes: a kind with no ownership
    contract is evidence, and a writer of a new kind must bring its own.

    Which kinds are recognised comes from the discovery module itself, not a list
    kept here: a hash-shaped child of worktrees and a Cargo target named after a
    version are both supported layouts that discovery owns, and duplicating that
    knowledge would report them as orphans the moment either grows.
    """
    root = cache/'target'
    if not root.is_dir():
        return 0
    total = 0
    # Every read below is bounded by the pass deadline and by an entry cap, so a
    # large or hostile cache cannot make this inventory outlive its run.
    for toolchain in sorted(targets.bounded_entries(root, 64, deadline), key=lambda e: e.name):
        path = Path(toolchain.path)
        if path.is_symlink() or not path.is_dir():
            continue
        if target_discovery.owns_layout(path):
            # A legacy target named after a version, or the flat worktrees root:
            # either way its children are target contents, not cache kinds.
            continue
        for entry in sorted(targets.bounded_entries(path, 256, deadline), key=lambda e: e.name):
            candidate = Path(entry.path)
            if candidate.is_symlink() or not candidate.is_dir():
                continue
            if target_discovery.owns_layout(candidate):
                continue
            targets.remaining(deadline)
            size = measured(candidate, dry_run)
            total += size or 0
            decided.append(clean_journal.row(
                candidate, 'unrecognised', bytes_before=size,
                detail=f'unrecognised kind under {toolchain.name}; never deleted'))
    return total


def reclaim(cache, home, dry_run=False, now=None, qualification=None, *, proc):
    deadline = time.monotonic() + RUN_SECONDS
    now = time.time() if now is None else now
    # Every decision this pass makes, so a timer run leaves evidence rather than
    # a silent no-op. A dry run measured nothing and records nothing.
    decided = []
    removed = []
    # RAM leaks must be recoverable even if unrelated disk attribution refuses.
    ram_error = None
    try:
        ram_removed = ram_targets.reap(dry_run, deadline)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        ram_removed = []
        ram_error = str(error)
        print(f'RAM cleanup incomplete; continuing disk cleanup: {error}', file=sys.stderr)
    try:
        owners = discover(cache, home, deadline, qualification)
        protected = active_targets(owners, deadline, proc)
        removed.extend(ram_removed)
        reclaim_owned_targets(cache, owners, protected, removed, decided, deadline, now, dry_run,
                              proc)
        unowned = unrecognised_kinds(cache, decided, deadline, dry_run)
        if unowned:
            # Naming the bytes makes the disk alert able to point at them.
            print(f'Unrecognised cache kinds hold {unowned} bytes; never deleted',
                  file=sys.stderr)
        removed.extend(orphan_cache.reclaim(cache, home, owners, dry_run, now, deadline,
                                            proc=proc))
    finally:
        # Written even when a later pass aborts: the decisions already reached are
        # the evidence, and losing them makes a refusal look like a no-op.
        if not dry_run:
            try:
                clean_journal.append(decided)
            except (OSError, ValueError, RuntimeError) as error:
                print(f'Cleanup journal unavailable: {error}', file=sys.stderr)
    if ram_error:
        raise RuntimeError(f'Disk cleanup completed, but RAM cleanup remains incomplete: {ram_error}')
    return removed


def scheduled_reclaim(cache, home, dry_run, diagnostics, proc):
    error = None
    try:
        reclaim(cache, home, dry_run,
                qualification=data_root()/'qualification', proc=proc)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as failure:
        error = str(failure)
        raise
    finally:
        # One row per pass, so a cleaner that has stopped succeeding is a fact
        # rather than something only journald saw. This is the only thing the
        # pass does about its own outcome: appending a local row is fast and
        # bounded, and every failure here is caught, so a witness can never
        # change what the pass itself did or how long it took.
        #
        # Alerting is deliberately not here. Reaching the coordinator means an
        # SSH round trip that can stall for minutes, and a pass must return its
        # own result promptly; a separate unit delivers (see install_schedules
        # and disk_alert.py), reading this record on its own schedule.
        try:
            path = hygiene_outcomes.append(hygiene_outcomes.row(
                'succeeded' if error is None else 'failed', error))
            if path is None:
                print('Cleanup outcome record unavailable: this pass was not recorded',
                      file=sys.stderr)
        # An unwritable record means this pass left no witness. That is reported,
        # but it never changes the pass's own result.
        except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as failure:
            print(f'Cleanup outcome record unavailable: {failure}', file=sys.stderr)
        # Fresh whole-root accounting includes hardlinks and unknown content.
        # A failed cleanup keeps its failure in the retained diagnostic too.
        if diagnostics:
            deadline = time.monotonic() + 30
            row = {'measured_unix': time.time(), 'cap_bytes': targets.CAP_BYTES,
                   'whole_cache_bytes': None, 'over_budget_bytes': None, 'available_bytes': None, 'error': error}
            try:
                row['whole_cache_bytes'] = targets.usage(cache, deadline)
                row['available_bytes'] = shutil.disk_usage(cache.resolve()).free
                row['over_budget_bytes'] = max(0, row['whole_cache_bytes']-targets.CAP_BYTES)
            except (subprocess.TimeoutExpired,
                    OSError, ValueError, RuntimeError, subprocess.SubprocessError) as failure:
                if (isinstance(failure, subprocess.TimeoutExpired)
                        or (isinstance(failure, RuntimeError)
                            and failure.args == ('Target cache maintenance exceeded its time limit',))):
                    # Only a spent bound is an unknown number, not a failed
                    # pass: the age and idle removals above already ran, and a
                    # measurement timeout must not fail the work it cannot
                    # undo. Every other census failure fails the pass below.
                    # The RuntimeError message stays in sync with
                    # evict_targets.remaining.
                    row['whole_cache_bytes'] = 'unknown'
                else:
                    row['error'] = '; '.join(filter(None, [error, str(failure)]))
            diagnostics.mkdir(parents=True, exist_ok=True)
            descriptor, path = tempfile.mkstemp(prefix='disk-hygiene-', suffix='.json', dir=diagnostics)
            with os.fdopen(descriptor, 'w') as output:
                json.dump(row, output)
                output.write('\n')
            excess = ('unknown' if row['over_budget_bytes'] is None
                      else f'{row["over_budget_bytes"]} bytes')
            print(f'Cache budget diagnostic: {path}; excess={excess}', flush=True)
            if row['error'] and error is None:
                raise RuntimeError(row['error'])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--dry-run', action='store_true')
    parser.add_argument('--diagnostics', type=Path, nargs='?', const=data_root()/'diagnostics',
                        help='write cleanup measurements, defaulting to the configured data root')
    parser.add_argument('--alert-only', action='store_true', help='check capacity and request cleanup when over budget')
    parser.add_argument('--agent-id', help='explicit StateEngine identity for coordinator disk warnings')
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument('--finish', metavar='COMMIT', help='mark the current owner target after landing')
    mode.add_argument('--inventory', action='store_true',
                      help='report managed and local targets at idle I/O priority; never reclaim')
    args = parser.parse_args()
    if args.alert_only and (not args.agent_id or args.inventory or args.finish or args.dry_run):
        parser.error('--alert-only requires --agent-id and cannot run cleanup modes')
    if args.agent_id and not args.alert_only:
        parser.error('--agent-id is used with --alert-only')
    if args.diagnostics and (args.alert_only or args.inventory or args.finish or args.dry_run):
        parser.error('--diagnostics is used with scheduled cleanup')
    cache = Path(os.environ.get('SECODE_CACHE_DIR', Path.home() / '.cache/secode')).resolve()
    if not args.inventory:
        cache.mkdir(parents=True, exist_ok=True)
    try:
        if args.alert_only:
            failures = []
            try:
                # Pressure is a free-space question, and statvfs answers it in
                # microseconds: a whole-cache du drew thirty seconds on a 100+
                # GiB cache and failed this unit many times a day. Each watched
                # filesystem is judged against its existing threshold, the
                # cache against the cleanup threshold and the root against the
                # alert threshold the mailer already uses.
                cache_free = shutil.disk_usage(cache).free
                root_free = shutil.disk_usage(Path('/')).free
                if (cache_free < disk_stall.CAPACITY_WARNING_BYTES
                        or root_free < disk_stall.CAPACITY_ALERT_BYTES):
                    subprocess.run(['systemctl', '--user', 'start', '--no-block',
                                    'secode-disk-hygiene.service'], check=True, timeout=10)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                failures.append('Cleanup request failed: '+str(error))
            try:
                disk_stall.capacity_warning(cache, args.agent_id)
            except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
                failures.append('Disk warning failed: '+str(error))
            if failures:
                raise RuntimeError('; '.join(failures))
            return 0
        if args.inventory:
            subprocess.run(['ionice', '-c', '3', sys.executable,
                            str(Path(__file__).with_name('target_inventory.py')),
                            '--cache', str(cache), '--home', str(Path.home())],
                           check=True, timeout=RUN_SECONDS)
        elif args.finish:
            if not re.fullmatch('[0-9a-f]{40}', args.finish):
                raise ValueError('Expected a full landed commit')
            mark_finished(Path(os.environ['CARGO_TARGET_DIR']).resolve(), Path.cwd().resolve(), args.finish)
            subprocess.run(['systemctl', '--user', 'start', '--no-block',
                            'secode-disk-hygiene.service'], check=True, timeout=10)
        else:
            with (cache / '.hygiene.lock').open('a') as maintenance:
                try:
                    fcntl.flock(maintenance, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except BlockingIOError:
                    print('Disk hygiene is already running; this request was coalesced')
                    return 0
                scheduled_reclaim(cache, Path.home(), args.dry_run, args.diagnostics,
                                  proc=Path('/proc'))
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Disk hygiene failed: {error}', file=sys.stderr)
        return 1
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
