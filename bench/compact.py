"""Run a native manual-compaction witness separately from frozen task scores."""
import argparse
import hashlib
import json
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

import run
from accounts import aligned, document, private_write


def fingerprint(path):
    if not path.is_file() or path.stat().st_size > 2 * 1024**3:
        raise ValueError('Native executable exceeds its fingerprint bound')
    digest = hashlib.sha256()
    with path.open('rb') as executable:
        for chunk in iter(lambda: executable.read(1024 * 1024), b''):
            digest.update(chunk)
    return {'path': str(path), 'sha256': digest.hexdigest()}


def execute(harness, binary, output, lines, authorization):
    if harness in {'codex', 'opencode'}:
        aligned(Path.home()/'.codex/auth.json', Path.home()/'.local/share/opencode/auth.json')
    native = shutil.which(binary)
    if not native:
        raise ValueError('Native executable is unavailable')
    binary = Path(native).resolve()
    script = Path(__file__).with_name(harness + '_compaction.py')
    output = output.resolve()
    output.mkdir(parents=True, exist_ok=False, mode=0o700)
    source = output/'runtime-source'
    source.mkdir()
    hashes = {}
    for path in [Path(__file__), script, Path(run.__file__), Path(__file__).with_name('accounts.py'),
                 run.ROOT/'scripts/build/gate.py', run.ROOT/'scripts/build/evict_targets.py']:
        shutil.copyfile(path, source/path.name)
        hashes[path.name] = hashlib.sha256(path.read_bytes()).hexdigest()
    result = {'schema': 'benchmark.native-compaction-run.v1', 'harness': harness,
              'authorization_ref': authorization, 'trigger': 'user-requested',
              'requested_model': {'codex': 'gpt-6-luna', 'opencode': 'openai/gpt-6-luna',
                                  'claude': 'claude-haiku-5-5'}[harness],
              'billing_route': 'claude-max' if harness == 'claude' else 'chatgpt-oauth', 'paid_calls': 0, 'held_out_runs': 0,
              'binary': fingerprint(binary), 'source': hashes,
              'limits': ['manual trigger, not automatic threshold',
                         'recent user messages may survive compaction',
                         'model selection is not independently verified provider identity',
                         'native token usage may omit compaction billing',
                         'setup witness separate from corpus scores']}
    snapshot = output/'repo'
    result['baseline'] = run.create_snapshot(
        {'files': {'README.md': 'Disposable native manual-compaction probe.\n'}}, snapshot)
    environment = run.runtime_environment()
    version_status, _, _ = run.bounded_process([str(binary), '--version'], output, '',
                                              output/'version.log', 10, environment)
    result['native_version'] = (output/'version.log').read_text().strip() if version_status == 'completed' else None
    extra = [script]
    if harness == 'codex':
        helper = binary.with_name('codex-code-mode-host')
        if helper.is_file():
            result['runtime_helper'] = fingerprint(helper)
            extra.append(helper)
    with tempfile.TemporaryDirectory(prefix='grapt-compact-account-') as private:
        account = None
        if harness == 'opencode':
            account = Path(private)/'data/opencode/auth.json'
            account.parent.mkdir(parents=True)
            selected = document(Path.home()/'.local/share/opencode/auth.json')['openai']
            private_write(account, json.dumps({'openai': selected}))
            environment['XDG_DATA_HOME'] = str(Path(private)/'data')
        command = run.agent_sandbox([sys.executable, str(script), '--binary', str(binary),
                                    '--lines', str(lines), '--timeout', '550'],
                                   harness, snapshot, output, environment, product_binary=str(binary),
                                   account_override=account, extra_paths=extra)
        status, code, seconds = run.bounded_process(command, snapshot, '', output/'probe.log',
                                                   600, environment)
    result.update(status=status, exit_code=code, wall_seconds=seconds)
    evidence = snapshot/'probe-result.json'
    if evidence.is_file() and not evidence.is_symlink() and evidence.stat().st_size <= 65536:
        result['native_probe'] = json.loads(evidence.read_text())
    result['success'] = code == 0 and result.get('native_probe', {}).get('success') is True
    (output/'result.json').write_text(json.dumps(result, indent=2)+'\n')
    print(json.dumps(result), flush=True)
    return 0 if result['success'] else 1


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--harness', choices=['codex', 'opencode', 'claude'], required=True)
    parser.add_argument('--binary', required=True)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--lines', type=int, default=1600)
    parser.add_argument('--execute', action='store_true')
    parser.add_argument('--authorization-ref')
    args = parser.parse_args()
    if not 0 <= args.lines <= 2000:
        parser.error('Archive rows must be between 0 and 2000')
    if not args.execute:
        print(json.dumps({'harness': args.harness, 'archive_rows': args.lines,
                          'trigger': 'user-requested', 'provider_calls': 0}))
        return 0
    if not args.authorization_ref:
        parser.error('Supply the actual received execution authorization reference')
    try:
        return execute(args.harness, args.binary, args.output, args.lines, args.authorization_ref)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError) as error:
        print(f'Native compaction setup failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
