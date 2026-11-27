#!/usr/bin/env python3
"""Install the workstation's nightly full gate and hourly derived-target cleanup."""
from operator_paths import data_root, worktrees
import argparse
import os
from pathlib import Path
import re
import shlex
import shutil
import stat
import subprocess
import tempfile
import cpu_priority


def _alert_sender(alert_agent_id):
    # One dedicated sender for every service alert, never a borrowed seat.
    if not re.fullmatch(r'[A-Za-z0-9][A-Za-z0-9-]{1,80}', alert_agent_id):
        raise ValueError('Use a plain bounded identity for the failure alert sender')
    return alert_agent_id


def _alert_on_failure(alert_agent_id):
    _alert_sender(alert_agent_id)
    return 'OnFailure=secode-unit-failed@%n.service\n'


def _alert_template(home, control, alert_agent_id):
    _alert_sender(alert_agent_id)
    return f'''[Unit]
Description=SE Code failure alert for failed unit %i
[Service]
Type=oneshot
WorkingDirectory={home}
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {control}/unit_failed_alert.py --unit %i --agent-id {alert_agent_id}
TimeoutStartSec=5min
'''


def retention_units(home, worktrees=None, control=None, alert_agent_id=None):
    control = control or home/'.local/lib/secode-release'
    if any(not re.fullmatch(r'/[a-zA-Z0-9_./-]+', str(path))
           for path in (home, control, *([worktrees] if worktrees else []))):
        raise ValueError('Use plain absolute paths for retention configuration')
    on_failure = '' if alert_agent_id is None else _alert_on_failure(alert_agent_id)
    nomination = '' if worktrees is None else f' --worktrees {worktrees}'
    specs = {
        'secode-retention.service': f'''[Unit]
Description=Expire seven-day SE Code scratch and archive nominated worktrees
{on_failure}[Service]
Type=oneshot
WorkingDirectory={home}
Environment=PATH={home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:/usr/bin:/bin
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {control}/retention.py --apply{nomination}
Nice=15
IOSchedulingClass=idle
TimeoutStartSec=20min
''',
        'secode-retention.timer': '''[Unit]
Description=Daily SE Code scratch and worktree retention
[Timer]
OnCalendar=*-*-* 05:00:00
Persistent=true
[Install]
WantedBy=timers.target
''',
    }
    # The template is owned here so both the full and the retention-only
    # install write exactly one of it; instances never need enabling.
    if alert_agent_id is not None:
        specs['secode-unit-failed@.service'] = _alert_template(home, control, alert_agent_id)
    return specs


def units(root, home, agent_id, control=None, alert_agent_id=None):
    control = control or home/'.local/lib/secode-release'
    # These values become systemd arguments, not shell code.
    if any(not re.fullmatch(r'/[a-zA-Z0-9_./-]+',str(path)) for path in [root,home,control]):
        raise ValueError('Use plain absolute paths for the nightly clone and home')
    if not re.fullmatch(r'agt-[a-z0-9-]{3,80}', agent_id):
        raise ValueError('Configure the explicit StateEngine agent that sends nightly notifications')
    on_failure = '' if alert_agent_id is None else _alert_on_failure(alert_agent_id)
    alert_sender = agent_id if alert_agent_id is None else _alert_sender(alert_agent_id)
    # Every caller shares one ticket queue and the legacy heavy flock.
    directory = control
    return {
        **cpu_priority.units(),
        'secode-nightly.service': f'''[Unit]
Description=SE Code main full gate and attributed regression revert
{on_failure}[Service]
Type=oneshot
WorkingDirectory={root}
Environment=PATH={home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:/usr/bin:/bin
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {directory}/nightly.py --repository {root} --agent-id {agent_id}
ExecStopPost=/usr/bin/python3 {directory}/nightly_failure.py
TimeoutStartSec=23h
''',
        'secode-nightly.timer': '''[Unit]
Description=SE Code nightly full gate
[Timer]
OnCalendar=*-*-* 02:00:00
Persistent=true
[Install]
WantedBy=timers.target
''',
        'secode-disk-hygiene.service': f'''[Unit]
Description=Reclaim idle or finished SE Code build targets
{on_failure}[Service]
Type=oneshot
WorkingDirectory={home}
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {directory}/disk_hygiene.py --diagnostics
Nice=15
IOSchedulingClass=idle
# Maintenance uses target leases rather than waiting for build admission.
TimeoutStartSec=20min
''',
        'secode-cleanup-alert.service': f'''[Unit]
Description=Send the SE Code coordinator a warning when cleanup keeps refusing
{on_failure}[Service]
Type=oneshot
WorkingDirectory={home}
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {directory}/disk_alert.py --agent-id {alert_sender}
Nice=15
IOSchedulingClass=idle
# One short delivery; the cleaner never waits for it.
TimeoutStartSec=5min
''',
        'secode-cleanup-alert.timer': '''[Unit]
Description=Deliver the SE Code cleanup warning every five minutes
[Timer]
OnBootSec=12min
OnUnitInactiveSec=5min
[Install]
WantedBy=timers.target
''',
        'secode-disk-hygiene.timer': '''[Unit]
Description=Hourly SE Code derived-target cleanup
[Timer]
OnBootSec=10min
OnUnitInactiveSec=1h
[Install]
WantedBy=timers.target
''',
        'secode-disk-pressure.service': f'''[Unit]
Description=Warn the SE Code coordinator before disk admission stalls
{on_failure}[Service]
Type=oneshot
WorkingDirectory={home}
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {directory}/disk_hygiene.py --alert-only --agent-id {agent_id}
TimeoutStartSec=1min
''',
        'secode-disk-pressure.timer': '''[Unit]
Description=Check SE Code disk capacity every five minutes
[Timer]
OnBootSec=2min
OnUnitInactiveSec=5min
[Install]
WantedBy=timers.target
''',
        'secode-job-history.service': f'''[Unit]
Description=Archive eligible stopped SE Code jobs and warn before history fills
{on_failure}[Service]
Type=oneshot
WorkingDirectory={home}
Environment=PATH=/usr/local/bin:/usr/bin:/bin
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 -B {directory}/durable_archive.py --apply --agent-id {agent_id}
Nice=15
IOSchedulingClass=idle
TimeoutStartSec=2min
''',
        'secode-job-history.timer': '''[Unit]
Description=Maintain SE Code job history every five minutes
[Timer]
OnBootSec=2min
OnUnitInactiveSec=5min
[Install]
WantedBy=timers.target
''',
    }


def atomic_copyfile(source, destination):
    """A partial controller file must never be visible to a starting timer."""
    temporary = destination.with_name(destination.name+'.tmp')
    try:
        shutil.copyfile(source, temporary)
        if destination.exists():
            # An upgraded controller keeps its final mode across the swap.
            temporary.chmod(destination.stat().st_mode & 0o7777)
        temporary.rename(destination)
    except BaseException:
        temporary.unlink(missing_ok=True)
        raise


def install_control(root, directory):
    # Keep the scheduler outside the checkout it bisects, so a broken main
    # revision cannot disable the next nightly fetch or the cleanup service.
    directory.mkdir(parents=True, exist_ok=True)
    # Deploy dependencies before their consumers; nightly.py must be last.
    # A timer can start during an upgrade and cache whichever modules exist.
    atomic_copyfile(root/'scripts/build-cache.sh', directory/'build-cache.sh')
    atomic_copyfile(root/'scripts/release/windows_version_worker.py', directory/'windows_version_worker.py')
    for name in ['operator_paths.py', 'operator_environment.ps1', 'tmp_scratch.py', 'build_environment.py', 'rustc-cache.sh', 'seat-env.sh', 'seat-shell.sh']:
        atomic_copyfile(root/'scripts/build'/name, directory/name)
    (directory/'rustc-cache.sh').chmod(0o755)
    for name in ('release_job.py','native_package.py','windows_package.py',
                 'package_files.py','hidden_policy.py','hidden_runner.py'):
        atomic_copyfile(root/'scripts/package'/name, directory/name)
    # The release failure recorder lives beside the release modules, and the
    # release installer requires it in a complete controller snapshot.
    atomic_copyfile(root/'scripts/release/release_status.py', directory/'release_status.py')
    for name in ['durable_jobs.py','flaky_quarantine.py','nextest_gate.py', 'gate_parallel.py', 'windows_native.py', 'windows_native_result.py', 'windows_native_worker.py', 'windows_native_environment.py', 'windows_native_survivors.py', 'windows_native.ps1','full_capacity.py','cpu_priority.py','git_environment.py','nightly_failure.py','nightly_revert.py','durable_archive.py','disk_stall.py','external_holders.py','heavy.py','heavy_runs.py','heavy_drain.py','heavy_hardware.py','heavy_sampling.py','heavy_queue.py','landing_guests.py','landing_reservation.py','landing_reservation_history.py','landing_reservation_reconcile.py','landing_priority.py','replace_waiter.py','windows_gate.py','windows_scope.py','gate.py','gate_plan.py','gate_scratch.py','publication_scan.py',
                 'receipt.py','cargo_observation.py','orphan_cache.py','target_discovery.py','duplicate_targets.py','evict_targets.py','test_features.py','ram_targets.py',
                 'reviewed_composition.py','target_inventory.py','gate_policy.py','gate_scope.py','slow_tests.py','gate_timing.py','gate_retry.py','gate_evidence.py','signal_trace.py','cargo_targets.py','disk_hygiene.py','disk_alert.py','clean_journal.py','hygiene_outcomes.py','retention.py','unit_failed_alert.py','nightly.py']:
        atomic_copyfile(root/'scripts/build'/name, directory/name)
    launcher = directory/'heavy'
    atomic_copyfile(root/'scripts/build/heavy.py', launcher)
    launcher.chmod(0o755)


def shell_profile(path):
    def identity(info):
        return (info.st_dev, info.st_ino, info.st_size, info.st_mtime_ns,
                info.st_ctime_ns, info.st_mode)
    try:
        descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    except FileNotFoundError:
        if path.is_symlink():
            raise ValueError('Shell profile is an unreadable symlink') from None
        return b'', None
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > 1024*1024:
            raise ValueError('Shell profile must be a bounded regular file')
        content = source.read(1024*1024+1)
        if len(content) > 1024*1024 or identity(info) != identity(os.fstat(source.fileno())):
            raise ValueError('Shell profile changed while reading; preserve it')
        return content, identity(info)


def install_seat_shells(home, control=None):
    """Opt in only SE Code seats; preserve existing shell startup content."""
    helper = shlex.quote(str((control or home/'.local/lib/secode-release')/'seat-shell.sh'))
    hook = ('# SE Code build environment\n'
            f'[ -z "${{BASH_VERSION:-}}" ] || [ ! -f {helper} ] || . {helper}\n'
            '# End SE Code build environment\n').encode()
    for name in ['.bashrc', '.profile']:
        path = home/name
        content, identity = shell_profile(path)
        mode = 0o644 if identity is None else stat.S_IMODE(identity[-1])
        if b'# SE Code build environment' in content:
            if content.count(hook) == 1:
                continue
            start = b'# SE Code build environment\n'; end = b'# End SE Code build environment\n'
            if content.count(start) != 1 or content.count(end) != 1:
                raise ValueError('Preserve a customized SE Code shell hook')
            left = content.index(start); right = content.index(end,left)+len(end)
            old = content[left:right]
            match = re.fullmatch(rb'# SE Code build environment\n\[ -z "\$\{BASH_VERSION:-\}" \] \|\| \[ ! -f (.+) \] \|\| \. \1\n# End SE Code build environment\n',old)
            words = shlex.split(match[1].decode()) if match else []
            old_helper = Path(words[0]) if len(words)==1 else None
            legacy = home/'.local/lib/secode-release/seat-shell.sh'
            if old_helper != legacy and (old_helper is None or old_helper.name!='seat-shell.sh' or
                    old_helper.parent.parent!=home/'.local/lib/secode-release-snapshots' or
                    not re.fullmatch('[0-9a-f]{9,40}',old_helper.parent.name)):
                raise ValueError('Preserve a customized SE Code shell hook')
            updated = content[:left]+hook+content[right:]
        else:
            # .bashrc commonly returns early for noninteractive shells.
            updated = hook+content if name == '.bashrc' else content+b'\n'+hook
        descriptor, temporary = tempfile.mkstemp(prefix='.secode-shell-', dir=home)
        try:
            with os.fdopen(descriptor, 'wb') as output:
                output.write(updated)
                os.fchmod(output.fileno(), mode)
            if shell_profile(path) != (content, identity):
                raise ValueError('Shell profile changed before publication; preserve it')
            if identity is None:
                try:
                    os.link(temporary, path)  # Never replace a concurrently created profile.
                except FileExistsError:
                    raise ValueError('Shell profile changed before publication; preserve it') from None
            else:
                os.replace(temporary, path)
        finally:
            Path(temporary).unlink(missing_ok=True)


def seed_green(root, commit):
    if not re.fullmatch('[0-9a-f]{40}',commit):
        raise ValueError('The initial full-green baseline needs a full commit')
    subprocess.run(['git','-C',str(root),'merge-base','--is-ancestor',commit,'HEAD'],check=True,timeout=10)
    subprocess.run(['git','-C',str(root),'update-ref','refs/secode/nightly-green',commit],check=True,timeout=10)


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository',type=Path,default=worktrees()/'nightly')
    parser.add_argument('--last-green',help='known full-green ancestor for the initial nightly diagnosis')
    parser.add_argument('--launcher-only',action='store_true',help='install the box launcher without fetching or changing timers')
    parser.add_argument('--seat-shells', action='store_true', help='also enable the cache in SE Code Bash seat shells')
    parser.add_argument('--retention-only', action='store_true', help='install only retention and its timer; no nightly fetch')
    parser.add_argument('--retention-worktrees', type=Path, help='explicit SE Code-only worktree nomination JSON')
    parser.add_argument('--agent-id', help='explicit StateEngine agent for nightly author/coordinator notifications')
    parser.add_argument('--alert-agent-id',
                        help='registered StateEngine sender that speaks for the service failure alerts; adds OnFailure=secode-unit-failed@%%n.service to every timer-driven service and re-points the cleaner alert to it')
    parser.add_argument('--control-directory', type=Path,
                        help='fresh immutable snapshot; existing controllers retain their original files')
    args = parser.parse_args()
    root = args.repository.resolve()
    home = Path.home()
    control = args.control_directory or home/'.local/lib/secode-release'
    if args.control_directory and (control.parent!=home/'.local/lib/secode-release-snapshots' or
            not re.fullmatch('[0-9a-f]{9,40}',control.name) or control.exists() or control.is_symlink()):
        raise ValueError('Choose a fresh version under ~/.local/lib/secode-release-snapshots')
    def deploy(source):
        install_control(source,control)
        if args.control_directory:
            for path in control.iterdir():
                if not path.is_file() or path.is_symlink():
                    raise ValueError('Unexpected immutable controller entry; preserve it')
                path.chmod(0o555 if path.stat().st_mode & 0o111 else 0o444)
            control.chmod(0o555)
    if args.seat_shells and not args.launcher_only:
        raise ValueError('Install seat shell hooks with --launcher-only')
    retention_specs = retention_units(home, args.retention_worktrees, control, args.alert_agent_id)
    if args.retention_worktrees is not None:
        from retention import worktree_paths
        worktree_paths(args.retention_worktrees)
    if args.retention_only:
        if args.launcher_only:
            raise ValueError('Choose launcher-only or retention-only')
        deploy(Path(__file__).resolve().parents[2])
        directory = home/'.config/systemd/user'
        directory.mkdir(parents=True, exist_ok=True)
        for name, content in retention_specs.items():
            (directory/name).write_text(content)
        subprocess.run(['systemctl','--user','daemon-reload'],check=True)
        subprocess.run(['systemctl','--user','enable','--now','secode-retention.timer'],check=True)
        print('Enabled daily seven-day retention at 05:00 local time.')
        return
    if args.launcher_only:
        deploy(Path(__file__).resolve().parents[2])
        if args.seat_shells:
            install_seat_shells(home, control)
        return
    if root.exists() and not (root/'.git/secode-nightly-owner').is_file():
        raise RuntimeError('Existing clone is not marked for nightly; preserve it and choose a fresh path')
    if args.agent_id is None:
        raise ValueError('Nightly installation requires --agent-id for notifications')
    specifications={**units(root,home,args.agent_id,control,args.alert_agent_id), **retention_specs}
    if not root.exists():
        subprocess.run(['git','clone','--branch','main','--single-branch','--no-tags',
                        os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'),str(root)],check=True,timeout=600)
    if not (root/'.git').is_dir() or subprocess.check_output(['git','-C',str(root),'status','--porcelain'],timeout=60):
        raise RuntimeError('Use a dedicated clean standalone clone for nightly')
    origin=subprocess.check_output(['git','-C',str(root),'remote','get-url','origin'],text=True,timeout=10).strip()
    if origin!=os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'):
        raise RuntimeError('Nightly origin must be canonical Windows main')
    subprocess.run(['git','-C',str(root),'fetch','--quiet','--no-tags','origin','main'],check=True,timeout=180)
    subprocess.run(['git','-C',str(root),'checkout','--quiet','--detach','FETCH_HEAD'],check=True,timeout=60)
    for filename in ['git_environment.py','nightly.py','disk_hygiene.py']:
        if not (root/'scripts/build'/filename).is_file():
            raise RuntimeError('Land the scheduler scripts before enabling timers')
    if args.last_green:
        seed_green(root,args.last_green)
    deploy(root)
    (root/'.git/secode-nightly-owner').write_text('Dedicated SE Code nightly clone\n')
    directory=home/'.config/systemd/user'; directory.mkdir(parents=True,exist_ok=True)
    for name,content in specifications.items():
        (directory/name).write_text(content)
    subprocess.run(['systemctl','--user','daemon-reload'],check=True)
    subprocess.run(['systemctl','--user','start',cpu_priority.SLICE],check=True,timeout=30)
    subprocess.run(['systemctl','--user','enable','--now','secode-nightly.timer','secode-disk-hygiene.timer',
                    'secode-retention.timer','secode-disk-pressure.timer','secode-job-history.timer',
                    'secode-cleanup-alert.timer'],check=True)
    print('Enabled nightly main full gate at 02:00 local machine time and hourly disk hygiene.')


if __name__=='__main__':
    main()
