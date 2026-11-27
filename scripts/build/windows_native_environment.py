"""Prepare the existing MSVC tools, pinned fixtures and E: scratch for a gate."""
from pathlib import Path
import os
import shutil
import subprocess
import sys
import tarfile


def prepare_python(worker):
    # Fixtures use python3 on every platform. Supply a run-local alias of
    # the existing interpreter, without changing the PC's installation.
    python = worker.directory/'python'
    worker.run([sys.executable, '-m', 'venv', '--without-pip', '--system-site-packages', str(python)])
    scripts = python/'Scripts'
    shutil.copyfile(scripts/'python.exe', scripts/'python3.exe')
    worker.environment['PATH'] = str(scripts)+os.pathsep+worker.environment.get('PATH', '')
    worker.run([str(scripts/'python3.exe'), '-c', 'import json, ssl, sqlite3'])


def prepare(worker, root, checkout):
    worker.environment['VSCMD_SKIP_SENDTELEMETRY'] = '1'
    vswhere = Path('C:/Program Files (x86)/Microsoft Visual Studio/Installer/vswhere.exe')
    installation = subprocess.check_output([str(vswhere), '-latest', '-products', '*',
        '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64', '-property', 'installationPath'],
        env=worker.environment, creationflags=subprocess.CREATE_NO_WINDOW, timeout=30).decode().strip()
    vcvars = Path(installation)/'VC/Auxiliary/Build/vcvars64.bat'
    if not installation or not vcvars.is_file() or any(c in str(vcvars) for c in '"&|<>%^!\r\n'):
        raise ValueError('A bounded local MSVC installation is required')
    # cmd parses its /c tail itself; list2cmdline would escape these quotes
    # for the C argv parser and break a Visual Studio path containing spaces.
    raw = subprocess.check_output('cmd.exe /d /s /c ""'+str(vcvars)+'" >nul && set"',
        env=worker.environment, creationflags=subprocess.CREATE_NO_WINDOW, timeout=60).decode(errors='replace')
    if len(raw) > 256*1024:
        raise ValueError('MSVC environment exceeds its bound')
    worker.environment.update((key.upper(), value) for key, value in
                              (line.split('=', 1) for line in raw.splitlines()
                               if '=' in line and not line.startswith('=')))
    environment = worker.environment
    for key in tuple(environment):
        if key.startswith(('SCCACHE_', 'SECODE_HEAVY_', 'SECODE_CACHE_', 'SECODE_RAM_')):
            environment.pop(key)
    for key in ('RUSTC_WRAPPER', 'RUSTC_WORKSPACE_WRAPPER', 'CARGO_ENCODED_RUSTFLAGS', 'RUSTFLAGS'):
        environment.pop(key, None)
    temporary = worker.directory/'tmp'
    temporary.mkdir()
    environment.update(TEMP=str(temporary), TMP=str(temporary), TMPDIR=str(temporary),
        CARGO_TARGET_DIR=str(root/'target'), CARGO_BUILD_JOBS=str(worker.request['jobs']),
        CARGO_BUILD_RUSTC_WRAPPER='', CARGO_BUILD_RUSTC_WORKSPACE_WRAPPER='',
        RUSTUP_TOOLCHAIN='1.97.1', CARGO_INCREMENTAL='1')
    prepare_python(worker)
    worker.run(['cargo', '--version'], cwd=checkout)
    pin = worker.request['fixture_commit']
    fixtures = root/'fixtures'/pin
    if not fixtures.exists():
        archive = worker.directory/'fixtures.tar'
        with archive.open('xb') as output:
            worker.run(['git', '-C', os.environ['SECODE_WINDOWS_ORACLE_ROOT'], 'archive', pin, 'fixtures/rust-reconstruction'], stdout=output)
        if archive.stat().st_size > 256*1024**2:
            raise ValueError('Pinned fixture archive exceeds its bound')
        staging = worker.directory/'fixtures-source'
        with tarfile.open(archive) as source:
            members = source.getmembers()
            if len(members) > 20000 or sum(m.size for m in members) > 256*1024**2:
                raise ValueError('Pinned fixture inventory exceeds its bound')
            source.extractall(staging, members=members, filter='data')
        fixtures.parent.mkdir(exist_ok=True)
        (staging/'fixtures/rust-reconstruction').rename(fixtures)
        (fixtures/'.source-commit').write_text(pin+'\n')
    if (fixtures/'.source-commit').read_text().strip() != pin:
        raise ValueError('Native fixtures differ from the selected pin')
    environment.update(SECODE_FIXTURES=str(fixtures), SECODE_FIXTURE_COMMIT=pin)
