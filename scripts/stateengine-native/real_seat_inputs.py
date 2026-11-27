"""Build and verify the existing fixture carrier against its actual build record."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

TARGET = 'x86_64-pc-windows-msvc'
FEATURE = 'loopback-provider-testkit'
COMMAND = ['cargo', '+1.97.1', 'xwin', 'build', '--locked', '--release', '-p',
           'grapt-product', '--features', FEATURE, '--bin', 'grapt-server',
           '--target', TARGET, '-j', '4']


def digest(path):
    with path.open('rb') as stream:
        return hashlib.file_digest(stream, 'sha256').hexdigest()


def verify_fixture(directory, source):
    path = directory / 'build-receipt.json'
    if path.stat().st_size > 65536:
        raise ValueError('fixture build record exceeded 64 KiB')
    receipt = json.loads(path.read_text(encoding='utf-8'))
    if (receipt.get('schema') != 'grapt.seat.fixture-build.v1'
            or receipt.get('source_commit') != source or receipt.get('tree_dirty') is not False
            or receipt.get('target') != TARGET or receipt.get('features') != [FEATURE]
            or receipt.get('command') != COMMAND):
        raise ValueError('fixture build record does not describe the same-source testkit build')
    product = directory / 'grapt-server.exe'
    artifact = receipt['outputs']['grapt-server.exe']
    if (not product.is_file() or product.is_symlink()
            or not 0 < product.stat().st_size <= 160 * 1024 * 1024
            or product.stat().st_size != artifact['bytes'] or digest(product) != artifact['sha256']):
        raise ValueError('fixture backend differs from its build record')
    return product


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    # Reuse the release builder's source observations and pinned-toolchain check.
    sys.path.insert(0, str(Path(__file__).resolve().parents[1] / 'release'))
    import build_artifacts as build
    source, dirt = build.custody()
    if dirt or subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no']):
        raise ValueError('fixture build requires a clean source checkout')
    target = Path(os.environ['CARGO_TARGET_DIR'])
    if not target.is_absolute():
        raise ValueError('source scripts/build-cache.sh before building the fixture')
    args.output.mkdir()
    build.require_toolchain()
    build.run(COMMAND, 7200)
    product = args.output / 'grapt-server.exe'
    shutil.copyfile(target / TARGET / 'release/grapt-server.exe', product)
    after, dirt = build.custody()
    if after != source or dirt or subprocess.check_output(['git', 'status', '--porcelain', '--untracked-files=no']):
        raise ValueError('fixture source changed during its build')
    receipt = dict(schema='grapt.seat.fixture-build.v1', source_commit=source,
        tree_dirty=False, target=TARGET, features=[FEATURE], command=COMMAND,
        outputs={'grapt-server.exe': dict(bytes=product.stat().st_size, sha256=digest(product))})
    (args.output / 'build-receipt.json').write_text(json.dumps(receipt, indent=2) + '\n', encoding='utf-8')
    verify_fixture(args.output, source)


if __name__ == '__main__':
    main()
