#!/usr/bin/env python3
"""Install a dedicated nightly/on-demand package staging job on the build box."""
import os
import sys
import argparse
from pathlib import Path
import re
import shutil
import subprocess

import release_job


sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'build'))
from operator_paths import data_root, worktrees


def units(repository, assets, output, home, control=None, actor=None):
    control = control or home/'.local/lib/secode-release'
    for path in (repository, assets, output, home, control):
        if not re.fullmatch(r'/[a-zA-Z0-9_./-]+', str(path)):
            raise ValueError('Use plain absolute paths for systemd configuration')
    # A failure is mailed to the coordinator from this identity; without it the
    # failure record says so instead of pretending a mail was sent.
    if actor is not None and not re.fullmatch(r'agt-[a-z0-9-]{3,80}', actor):
        raise ValueError('Configure the explicit agent that sends release failure mail')
    notification = (f'Environment=GRAPT_RELEASE_ACTOR={actor}\n'
                    f'Environment=GRAPT_RELEASE_STATUS_ROOT={home}/.local/state/secode/release-status\n'
                    if actor else '')
    return {
        'secode-release.service': f'''[Unit]
Description=Stage Grapt Linux and Windows packages from one main commit
[Service]
Type=oneshot
WorkingDirectory={repository}
Environment=PATH={home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:/usr/bin:/bin
EnvironmentFile=-{home}/.config/secode/build.env
{notification}ExecStart=/usr/bin/python3 {control}/release_job.py --repository {repository} --assets {assets} --output-root {output}
TimeoutStartSec=20h
''',
        'secode-release.timer': '''[Unit]
Description=Nightly Grapt package staging
[Timer]
OnCalendar=*-*-* 04:00:00
Persistent=true
[Install]
WantedBy=timers.target
''',
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--repository', type=Path, default=worktrees()/'release-job')
    parser.add_argument('--assets', type=Path, required=True)
    parser.add_argument('--output-root', type=Path, default=data_root()/'releases')
    parser.add_argument('--control-directory', type=Path, help='complete immutable controller installed with install_schedules.py')
    parser.add_argument('--agent-id', help='explicit agent that sends release failure mail')
    args = parser.parse_args()
    root, inputs, output = args.repository.resolve(), args.assets.resolve(), args.output_root.resolve()
    control = args.control_directory or Path.home()/'.local/lib/secode-release'
    specifications = units(root, inputs, output, Path.home(), control, args.agent_id)
    release_job.assets(inputs)
    if root.exists() and not (root / '.git/secode-release-owner').is_file():
        raise ValueError('Existing checkout is unmarked; preserve it and choose a fresh release path')
    if not root.exists():
        subprocess.run(['git', 'clone', '--branch', 'main', '--single-branch', '--no-tags',
                        os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'), str(root)], check=True, timeout=600)
        (root / '.git/secode-release-owner').write_text('Dedicated Grapt release checkout\n')
    if release_job.git(root, 'status', '--porcelain'):
        raise ValueError('Preserve the dirty release checkout before installing')
    if release_job.git(root, 'remote', 'get-url', 'origin') != os.environ.get('SECODE_CANONICAL_REMOTE', 'https://github.com/StateEngine/grapt'):
        raise ValueError('Release origin must be canonical Windows main')
    # Keep the controller outside the source revision it checks out.
    if not (control / 'heavy').is_file():
        raise ValueError('Install the reviewed build launcher with scripts/build/install_schedules.py first')
    control.mkdir(parents=True, exist_ok=True)
    # The failure recorder lives beside the release modules, not the package
    # ones, so it travels through the same snapshot guard as every other module:
    # a supplied controller must already match byte for byte, and a fresh
    # controller publishes it whole rather than writing through a symlink.
    modules = [(name, Path(__file__).with_name(name)) for name in
               ('release_job.py', 'native_package.py', 'windows_package.py',
                'package_files.py', 'hidden_policy.py', 'hidden_runner.py')]
    modules.append(('release_status.py',
                    Path(__file__).resolve().parents[1]/'release/release_status.py'))
    for name, source in modules:
        target = control/name
        if args.control_directory:
            if (target.is_symlink() or not target.is_file() or target.stat().st_size>65536 or
                    target.read_bytes()!=source.read_bytes()):
                raise ValueError('Preserve the existing controller; install a matching complete snapshot first')
        else:
            # Never publish through an existing symlink: replace the link itself.
            if target.is_symlink():
                target.unlink()
            staged = control/f'.{name}.next'
            shutil.copyfile(source, staged)
            staged.replace(target)
    unit_root = Path.home() / '.config/systemd/user'
    unit_root.mkdir(parents=True, exist_ok=True)
    for name, content in specifications.items():
        (unit_root / name).write_text(content)
    subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True, timeout=30)
    subprocess.run(['systemctl', '--user', 'enable', '--now', 'secode-release.timer'], check=True, timeout=30)
    print('Enabled nightly staging at 04:00 local time; on demand: systemctl --user start secode-release.service')


if __name__ == '__main__':
    main()
