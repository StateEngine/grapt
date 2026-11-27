#!/usr/bin/env python3
"""Qualify and stage Linux and Windows packages from one canonical main commit."""
import argparse
from datetime import datetime, timezone
import fcntl
import json
import os
from pathlib import Path
import re
import subprocess
import sys

TARGETS = ('x86_64-unknown-linux-gnu', 'x86_64-pc-windows-msvc')
ASSETS = ('plugin_runtime', 'plugin_runtime_archive', 'plugin_runtime_license', 'stateengine_payload')


# What the run in progress has established so far: the commit it checked out,
# the step it is executing, that step's output log and, on failure, its exit.
# The failure recorder reads this, because the handoff out of main() carries
# only the message.
RUN = {}


def note(**facts):
    RUN.update(facts)


def git(root, *arguments):
    return subprocess.check_output(['git', '-C', str(root), *arguments], text=True, timeout=180).strip()


def source(root, commit):
    if git(root, 'rev-parse', 'HEAD') != commit or git(root, 'status', '--porcelain'):
        raise RuntimeError('Release checkout changed; preserve the unfinished staging directory')


def assets(path):
    if path.stat().st_size > 65536:
        raise ValueError('Release asset configuration exceeds 64 KiB')
    value = json.loads(path.read_text())
    if not isinstance(value, dict) or set(value) != set(TARGETS):
        raise ValueError('Configure assets for exactly the Linux and Windows release targets')
    for target in TARGETS:
        row = value[target]
        required = set(ASSETS) | ({'python_runtime_archive'} if target.endswith('windows-msvc') else set())
        if not isinstance(row, dict) or set(row) != required:
            raise ValueError(f'Incomplete release assets for {target}')
        for name in required:
            candidate = Path(row[name])
            if not candidate.is_absolute() or not candidate.exists():
                raise ValueError(f'Missing absolute {name} path for {target}')
    return value


def stage(root, output, commit, inputs, jobs, run, native=None):
    """Only publish the staged receipt after both packagers verify their inputs."""
    for target in TARGETS:
        source(root, commit)
        built = output / (target + '-artifacts')
        run([sys.executable, 'scripts/release/build_artifacts.py', '--root', str(root),
             '--target', target, '--jobs', str(jobs), '--output', str(built)])
        source(root, commit)
        if target.endswith('windows-msvc'):
            if native is None:
                from native_package import assemble
                native = assemble
            native(root, built, output / target, commit, inputs[target])
        else:
            command = [sys.executable, 'scripts/release/package.py', '--rust-root', str(root),
                       '--target', target, '--artifacts', str(built),
                       '--build-receipt', str(built / 'build-receipt.json'),
                       '--output', str(output / target), '--require-plugin-runtime']
            for key in ASSETS:
                command += ['--' + key.replace('_', '-'), inputs[target][key]]
            run(command)
        manifest = json.loads((output / target / 'release-manifest.json').read_text())
        if manifest.get('engine_version') != commit or manifest.get('target') != target:
            raise RuntimeError('Packaged target or source differs from the pinned release')
    source(root, commit)
    (output / 'release-job.json').write_text(json.dumps(dict(
        schema='grapt.release-job.v1', source_commit=commit, status='staged',
        targets=list(TARGETS), qualification='qualification.log.receipt.json',
        native_acceptance='pending: native Windows exercise and installed startup checks',
    ), indent=2) + '\n')


def cache_root():
    """Rename default: prefer the grapt cache root, fall back to the legacy
    secode root, else the renamed path."""
    override = os.environ.get('SECODE_CACHE_DIR')
    if override is not None:
        return Path(override)
    base = Path.home()/'.cache'
    if (base/'grapt').is_dir():
        return base/'grapt'
    if (base/'secode').is_dir():
        return base/'secode'
    return base/'grapt'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', required=True, type=Path)
    parser.add_argument('--assets', required=True, type=Path)
    parser.add_argument('--output-root', required=True, type=Path)
    parser.add_argument('--commit', default='main', help='main or a full commit reachable from canonical main')
    parser.add_argument('--jobs', type=int, default=3, choices=range(1, 4))
    parser.add_argument('--build-only', action='store_true', help=argparse.SUPPRESS)
    parser.add_argument('--staging', type=Path, help=argparse.SUPPRESS)
    args = parser.parse_args()
    args.assets = args.assets.resolve()
    args.output_root = args.output_root.resolve()
    root = args.repository.resolve()
    if not (root / '.git/secode-release-owner').is_file():
        raise ValueError('Use a dedicated standalone clone marked .git/secode-release-owner')
    inputs = assets(args.assets)
    os.chdir(root)
    helpers = Path(__file__).resolve().parent
    if not (helpers/'heavy.py').is_file():
        helpers = root/'scripts/build'
    sys.path.insert(0,str(helpers))
    import heavy
    if args.build_only:
        if heavy.inherited() is None or args.staging is None or not re.fullmatch('[0-9a-f]{40}', args.commit):
            raise ValueError('Internal build requires its admitted lease and pinned source')
        heavy.run([sys.executable,str(root/'scripts/build/receipt.py'),
                   str(args.staging/'qualification.log'),args.commit,'--base',args.commit,'--require-full'],
                  cwd=root,check=True,timeout=120,pass_fds=heavy.inherited_fds())
        def run(command):
            with heavy.owned_process(command, pass_fds=heavy.inherited_fds()) as child:
                child.wait(timeout=15000)
                if child.returncode:
                    note(step=command[0], exit=child.returncode,
                         log=args.staging/'qualification.log', commit=args.commit)
                    raise subprocess.CalledProcessError(child.returncode, command)
        stage(root, args.staging, args.commit, inputs, args.jobs, run)
        return
    if heavy.inherited() is not None:
        raise ValueError('Run the release supervisor outside a heavy lease; phases queue separately')
    with (root / '.git/grapt-release.lock').open('a') as lock:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        if git(root, 'status', '--porcelain'):
            raise ValueError('Preserve the dirty release checkout before fetching')
        if git(root, 'remote', 'get-url', 'origin') != os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'):
            raise ValueError('Release origin must be canonical Windows main')
        git(root, 'fetch', '--quiet', '--no-tags', 'origin', 'main')
        canonical_tip = git(root, 'rev-parse', 'FETCH_HEAD')
        commit = canonical_tip if args.commit == 'main' else args.commit
        if not re.fullmatch('[0-9a-f]{40}', commit):
            raise ValueError('Use main or a full canonical commit')
        git(root, 'merge-base', '--is-ancestor', commit, canonical_tip)
        git(root, 'checkout', '--quiet', '--detach', commit)
        note(commit=commit)
        source(root, commit)
        stamp = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
        final = args.output_root.resolve() / f'{stamp}-{commit[:12]}'
        unfinished = final.with_name(final.name + '.unfinished')
        unfinished.mkdir(parents=True)
        launcher = [sys.executable,str(Path(heavy.__file__).resolve())]
        environment = dict(os.environ, CARGO_BUILD_JOBS=str(args.jobs),
                           SECODE_CACHE_DIR=str(cache_root()))
        with (unfinished / 'job.log').open('w') as log:
            # Build-cache defaults and disk admission remain owned by the existing entrypoints.
            for phase, command in [
                ('full gate', [*launcher, '--nvme', '--owner', 'release-full', '--', 'bash', 'scripts/gate.sh',
                 'full', '--row-a', '--base', commit, '--wait', '--log', str(unfinished / 'qualification.log')]),
                ('package stage', [*launcher, '--nvme', '--owner', 'release-packages', '--', 'bash', '-eu', '-c',
                 'source scripts/build-cache.sh; exec "$@"', 'release-job', sys.executable, str(Path(__file__).resolve()),
                 '--build-only', '--repository', str(root), '--assets', str(args.assets.resolve()),
                 '--output-root', str(args.output_root.resolve()), '--commit', commit,
                 '--jobs', str(args.jobs), '--staging', str(unfinished)]),
            ]:
                # The phase, not the launcher both phases share, is what names
                # the failure for an operator.
                note(step=phase, log=unfinished/'job.log')
                with heavy.owned_process(command, env=environment, stdout=log, stderr=subprocess.STDOUT) as child:
                    code = heavy.wait_owned(child, 32000)
                    if code:
                        note(exit=code)
                        raise subprocess.CalledProcessError(code, command)
        source(root, commit)
        # A success supersedes the last failure in the record, so the newest row
        # always describes the most recent night. Recorded before the rename,
        # while the run's own log is still at the staged path.
        record_outcome('staged', 0, unfinished/'job.log', commit, failed=False)
        unfinished.rename(final)
        print(f'Staged {commit}: {final}', flush=True)


def record_failure(message):
    """Record and announce the run that just failed, from what it established.

    The message is only the summary. The commit, the failing step, its exit and
    its log come from RUN, which main() filled in as it went; nothing is guessed
    and nothing is read back out of the environment.

    Best effort throughout: a status or mail failure is printed and kept in the
    record, and never changes the release run's own exit.
    """
    step = RUN.get('step') or message
    log_path = RUN.get('log')
    exit_code = RUN.get('exit', 1)
    commit = RUN.get('commit', os.environ.get('GRAPT_RELEASE_COMMIT', ''))
    record_outcome(step, exit_code, log_path, commit, failed=True)


def record_outcome(step, exit_code, log_path, commit, *, failed):
    """Record and announce one finished run without changing its exit.

    A successful run records too: it supersedes the last failure, so the newest
    record is always the truth about the most recent night.
    """
    status_root = Path(os.environ.get('GRAPT_RELEASE_STATUS_ROOT')
                       or Path.home()/'.local/state/secode/release-status')
    actor = os.environ.get('GRAPT_RELEASE_ACTOR')
    run_id = datetime.now(timezone.utc).strftime('%Y%m%dT%H%M%SZ')
    try:
        sys.path.insert(0, str(Path(__file__).resolve().parent.parent / 'release'))
        import release_status
    except ImportError as error:
        print(f'Release status helper is unavailable: {error}', file=sys.stderr)
        return
    output = [] if log_path is None else release_status.tail_lines(log_path)
    outcome = 'failed' if failed else 'staged'
    row = {'run': run_id, 'commit': commit, 'outcome': outcome, 'mail_outcome': None}
    if exit_code == 0:
        try:
            release_status.write_status(status_root, run_id, row)
        except (OSError, ValueError) as error:
            print(f'Release success could not be recorded: {error}', file=sys.stderr)
        return
    failure = release_status.failed(step, output, exit_code)
    row['failure'] = failure
    if actor:
        row['mail_outcome'] = release_status.mail(
            actor, release_status.announcement(run_id, commit, failure, status_root))
    else:
        row['mail_outcome'] = 'no GRAPT_RELEASE_ACTOR configured'
    if row['mail_outcome'] is not None:
        # Never let an unsent alert read as a sent one: say so where an operator
        # and the journal both look, without failing the run over it.
        print(f'release failure alert NOT sent: {row["mail_outcome"]}', file=sys.stderr, flush=True)
    try:
        release_status.write_status(status_root, run_id, row)
    except (OSError, ValueError, MemoryError) as error:
        print(f'Release failure could not be recorded: {error}', file=sys.stderr)


if __name__ == '__main__':
    try:
        main()
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Release job failed: {error}', file=sys.stderr)
        # A failed run must announce itself: the journal alone is not enough,
        # because after a reboot the unit reads as successful and nobody was
        # told. Recording is best effort and must never raise past this point,
        # or the record and the alert are lost with it.
        try:
            record_failure(str(error))
        except Exception as record:
            print(f'Release failure could not be recorded: {record}', file=sys.stderr)
        raise SystemExit(1)
