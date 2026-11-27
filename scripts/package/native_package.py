"""Transfer opaque cross-built inputs to a fresh native Windows assembly attempt."""
import os
import base64
import json
from pathlib import Path
import subprocess
import sys
import zipfile

from package_files import extract, fingerprint, verify_package

sys.path.insert(0, str(Path(__file__).resolve().parents[1]/'build'))
import operator_paths

PYTHON_SHA256 = '90905e64a840557d8c1bc4f5b74db3d7ee85e3ffb217611cac1c7813acea4159'


def remote(script):
    encoded = base64.b64encode(operator_paths.powershell_environment(script).encode('utf-16le')).decode('ascii')
    return subprocess.check_output(['ssh', os.environ['SECODE_WINDOWS_HOST'], 'powershell.exe', '-NoProfile',
                                    '-NonInteractive', '-EncodedCommand', encoded], timeout=1200, text=True)


def assemble(source, built, output, commit, inputs):
    work = output.parent / 'windows-native-inputs'
    work.mkdir()
    sys.path.insert(0, str(source / 'scripts/release'))
    import stateengine_payload
    pins = json.loads((source / 'packages/stateengine/payload-pins.json').read_text())['targets']
    payload = Path(inputs['stateengine_payload'])
    document, _ = stateengine_payload.verify(payload, 'x86_64-pc-windows-msvc', pins['x86_64-pc-windows-msvc'])
    receipt = json.loads((built / 'build-receipt.json').read_text())
    if receipt['source_commit'] != commit or receipt['tree_dirty'] is not False:
        raise ValueError('Windows artifacts differ from the selected clean source')
    bundle = work / 'source.bundle'
    subprocess.run(['git', '-C', str(source), 'bundle', 'create', str(bundle), 'HEAD'], check=True, timeout=180)
    rows = [(bundle, 'source.bundle')]
    for name, value in receipt['outputs'].items():
        stateengine_payload.relative(name)
        if fingerprint(built / name) != {key: value[key] for key in ('sha256', 'bytes')}:
            raise ValueError('Windows artifact differs from its build receipt')
        rows.append((built / name, 'artifacts/' + name))
    rows.append((built / 'build-receipt.json', 'artifacts/build-receipt.json'))
    rows += [(payload / name, 'payload/' + name)
             for name in (stateengine_payload.manifest_name(payload), *document['files'])]
    rows += [(Path(inputs[key]), 'plugin-runtime/' + name) for key, name in (
        ('plugin_runtime', 'node.exe'), ('plugin_runtime_archive', 'node.zip'), ('plugin_runtime_license', 'LICENSE'))]
    if len(rows) > 20000 or sum(path.stat().st_size for path, _ in rows) > 2 * 1024**3:
        raise ValueError('Windows input archive exceeds its bound')
    archive = work / 'inputs.zip'
    with zipfile.ZipFile(archive, 'x', compression=zipfile.ZIP_DEFLATED) as output_zip:
        for path, name in rows:
            if path.is_symlink() or not path.is_file() or path.stat().st_size > 160 * 1024**2:
                raise ValueError('Windows input is linked, absent, or oversized')
            output_zip.write(path, name)
    (work / 'inputs.json').write_text(json.dumps(dict(commit=commit, inputs=fingerprint(archive))) + '\n')
    python = Path(inputs['python_runtime_archive'])
    if fingerprint(python)['sha256'] != PYTHON_SHA256:
        raise ValueError('Pinned Windows embedded Python 3.14.10 archive differs')
    # Numeric time + commit labels contain no PowerShell metacharacters. Never reuse an earlier attempt.
    import time
    label = f'native-release-{time.time_ns()}-{commit[:12]}'
    remote_root = 'E:/temp/' + label
    controls = Path(__file__).resolve().parent
    files = {name: controls / name for name in ('windows_package.py', 'package_files.py', 'hidden_policy.py', 'hidden_runner.py')}
    files.update({'inputs.zip': archive, 'inputs.json': work / 'inputs.json', 'python.zip': python})
    files['windows_version_worker.py'] = source / 'scripts/release/windows_version_worker.py'
    expected = {name: fingerprint(path) for name, path in files.items()}
    remote(f"$ErrorActionPreference='Stop'; if(Test-Path -LiteralPath '{remote_root}'){{throw 'Preserve native attempt'}}; New-Item -ItemType Directory -Path '{remote_root}'|Out-Null")
    (work / 'native-location.txt').write_text(remote_root + '\n')
    for name, path in files.items():
        subprocess.run(['scp', '-q', str(path), os.environ['SECODE_WINDOWS_HOST']+':/' + remote_root + '/' + name], check=True, timeout=600)
    check = ';'.join(f"if((Get-FileHash -Algorithm SHA256 -LiteralPath '{remote_root}/{name}').Hash.ToLowerInvariant() -ne '{value['sha256']}'){{throw 'Transferred input differs'}}" for name, value in expected.items())
    remote(f"$ErrorActionPreference='Stop'; {check}; Expand-Archive -LiteralPath '{remote_root}/python.zip' -DestinationPath '{remote_root}/python'; "
           f"$p=Start-Process -FilePath '{remote_root}/python/python.exe' -WindowStyle Hidden -PassThru "
           f"-ArgumentList @('-I','-B','{remote_root}/windows_package.py') "
           f"-RedirectStandardOutput '{remote_root}/native.log' -RedirectStandardError '{remote_root}/native.stderr'; "
           f"$null=$p.Handle; $p.Id|Set-Content -LiteralPath '{remote_root}/native.pid'; "
           f"try{{if(!$p.WaitForExit(900000)){{throw 'Native assembly timed out'}}; "
           f"if($p.ExitCode -ne 0 -or !(Test-Path -LiteralPath '{remote_root}/windows-package.json')){{throw 'Native assembly failed; preserve logs at {remote_root}'}}}} "
           f"finally{{if(!$p.HasExited){{$p.Kill();$null=$p.WaitForExit(5000)}};$p.Dispose()}}")
    for name in ('windows-package.zip', 'windows-package.json', 'native.log', 'native.stderr'):
        subprocess.run(['scp', '-q', os.environ['SECODE_WINDOWS_HOST']+':/' + remote_root + '/' + name, str(work / name)], check=True, timeout=600)
    result = json.loads((work / 'windows-package.json').read_text())
    if (result.get('source_commit') != commit or result.get('native_assembly') is not True
            or fingerprint(work / 'windows-package.zip') != result['package']):
        raise ValueError('Native Windows package result differs')
    extract(work / 'windows-package.zip', output)
    verify_package(output, commit)
