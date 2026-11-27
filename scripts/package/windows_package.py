"""Assemble the pinned Windows package on Windows, preserving its native smoke."""
import json
import os
from pathlib import Path
import subprocess
import sys
import zipfile

sys.path.insert(0, str(Path(__file__).resolve().parent))
from package_files import extract, fingerprint, verify_package


def main():
    if os.name != 'nt':
        raise RuntimeError('Windows package assembly requires a native Windows host')
    import hidden_policy
    hidden_policy.install()
    # Reuse the release worker's existing kill-on-close, eight-process job.
    import windows_version_worker
    windows_version_worker.contain()
    root = Path(__file__).resolve().parent
    metadata = json.loads((root / 'inputs.json').read_text())
    if fingerprint(root / 'inputs.zip') != metadata['inputs']:
        raise ValueError('Transferred Windows inputs differ')
    extract(root / 'inputs.zip', root / 'inputs')
    source = root / 'source'
    git = Path(os.environ.get('ProgramFiles', r'C:\Program Files')) / 'Git/cmd/git.exe'
    environment = dict(os.environ, PATH=str(git.parent) + os.pathsep + os.environ.get('PATH', ''))
    def run(command):
        subprocess.run([os.fspath(value) for value in command], check=True, timeout=300, env=environment)
    run([git, 'init', source])
    run([git, '-C', source, 'config', 'core.autocrlf', 'false'])
    run([git, '-C', source, 'config', 'core.longpaths', 'true'])
    run([git, '-C', source, 'fetch', root / 'inputs/source.bundle', 'HEAD'])
    run([git, '-C', source, 'checkout', '--detach', metadata['commit']])
    artifacts = root / 'inputs/artifacts'
    runtime = root / 'inputs/plugin-runtime'
    package = root / 'package'
    run([sys.executable, source / 'scripts/release/package.py', '--rust-root', source,
         '--target', 'x86_64-pc-windows-msvc', '--artifacts', artifacts,
         '--build-receipt', artifacts / 'build-receipt.json', '--output', package,
         '--stateengine-payload', root / 'inputs/payload', '--require-plugin-runtime',
         '--plugin-runtime', runtime / 'node.exe', '--plugin-runtime-archive', runtime / 'node.zip',
         '--plugin-runtime-license', runtime / 'LICENSE'])
    verify_package(package, metadata['commit'])
    sys.path.insert(0, str(source / 'scripts/release'))
    import stateengine_payload
    names = stateengine_payload.inventory(package)
    archive = root / 'windows-package.zip'
    with zipfile.ZipFile(archive, 'x', compression=zipfile.ZIP_DEFLATED) as output:
        for name in sorted(names):
            output.write(package / name, name)
    (root / 'windows-package.json').write_text(json.dumps(dict(
        source_commit=metadata['commit'], package=fingerprint(archive),
        native_assembly=True,
    )) + '\n')


if __name__ == '__main__':
    main()
