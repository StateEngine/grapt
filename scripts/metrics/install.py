#!/usr/bin/env python3
"""Install the read-only fleet exporter as a five-minute user timer."""
import argparse
from pathlib import Path
import re
import shutil
import subprocess


def units(home):
    if not re.fullmatch(r'/[a-zA-Z0-9_./-]+', str(home)):
        raise ValueError('Use a plain absolute home path for systemd arguments')
    return {
        'secode-fleet-metrics.service': f'''[Unit]
Description=Grapt fleet operational metrics
[Service]
Type=oneshot
Environment=PATH={home}/.cargo/bin:{home}/.local/bin:/usr/local/bin:/usr/bin:/bin
EnvironmentFile=-{home}/.config/secode/build.env
ExecStart=/usr/bin/python3 {home}/.local/lib/grapt-metrics/fleet_metrics.py
Nice=10
TimeoutStartSec=90
''',
        'secode-fleet-metrics.timer': '''[Unit]
Description=Sample Grapt fleet metrics every five minutes
[Timer]
OnBootSec=30s
OnCalendar=*-*-* *:0/5:00
AccuracySec=1s
Persistent=true
[Install]
WantedBy=timers.target
''',
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.parse_args()
    home = Path.home()
    specifications = units(home)
    directory = home / '.local/lib/grapt-metrics'
    directory.mkdir(parents=True, exist_ok=True)
    for name in ('operator_paths.py', 'operator_environment.ps1'):
        shutil.copyfile(Path(__file__).resolve().parents[1]/'build'/name, directory/name)
    source = Path(__file__).with_name('fleet_metrics.py')
    staged = directory / 'fleet_metrics.py.next'
    shutil.copyfile(source, staged)
    staged.replace(directory / 'fleet_metrics.py')
    unit_directory = home / '.config/systemd/user'
    unit_directory.mkdir(parents=True, exist_ok=True)
    for name, content in specifications.items():
        (unit_directory / name).write_text(content)
    subprocess.run(['systemctl', '--user', 'daemon-reload'], check=True, timeout=15)
    subprocess.run(['systemctl', '--user', 'enable', '--now', 'secode-fleet-metrics.timer'],
                   check=True, timeout=15)
    subprocess.run(['systemctl', '--user', 'start', 'secode-fleet-metrics.service'],
                   check=True, timeout=95)
    print('Enabled five-minute fleet snapshots at the configured metrics output.')


if __name__ == '__main__':
    main()
