#!/usr/bin/env python3
"""Serial exact-target Tier1 witnesses; restore bytes before every green check.

Run only in an idle integration root with the existing register-only cargo
lease helper. No target eviction, git reset, credential or provider operation.
"""
import argparse
import json
import os
from pathlib import Path
import subprocess
import sys

parser = argparse.ArgumentParser()
parser.add_argument('--evidence', type=Path, required=True)
parser.add_argument('--cargo-helper', type=Path, required=True)
parser.add_argument('--only')
parser.add_argument('--manifest', type=Path)
parser.add_argument('--inherited-rm31', action='store_true')
parser.add_argument('--inherited-rm12', action='store_true')
args = parser.parse_args()
if 'CARGO_TARGET_DIR' not in os.environ or os.environ.get('TRYBUILD'):
    raise SystemExit('shared target required; probe overwrite must be disabled')
manifest = args.manifest or Path('fixtures/stateengine-native/mail-authority-mutations.json')
if not manifest.resolve().is_relative_to(Path.cwd().resolve()): raise SystemExit('manifest escapes its owned root')
rows = json.loads(manifest.read_text())['rows']
if not 1 <= len(rows) <= 32 or len({row['id'] for row in rows}) != len(rows) or (not args.manifest and len(rows) != 7):
    raise SystemExit('mutation census')
if args.inherited_rm12 or args.inherited_rm31:
    if args.only or args.manifest or args.inherited_rm12 and args.inherited_rm31: raise SystemExit('inherited and new selections are distinct')
    selected = 'RM31' if args.inherited_rm31 else 'RM12'
    owner = 'crates/grapt-types/src/remote_mail/ack.rs' if args.inherited_rm31 else 'crates/grapt-types/src/remote_mail.rs'
    inherited = json.loads(Path('crates/grapt-mail-transport/mutations/manifest.json').read_text())['mutations']
    original = [row for row in inherited if row['id'] == selected]
    if len(original) != 1: raise SystemExit('inherited census')
    row = original[0]
    if row['file'] != owner: raise SystemExit('unexpected inherited owner')
    rows = [dict(id=selected, path=row['file'], before=row['old'], after=row['new'], witness=row['test'], law=row['law'], target='lib')]
elif args.only:
    rows = [row for row in rows if row['id'] == args.only]
    if len(rows) != 1: raise SystemExit('unknown mutation')
args.evidence.mkdir(parents=True, exist_ok=True)
results = []

def check(row, phase):
    log = str(args.evidence / f"{row['id']}-{phase}.log")
    if row['target'] == 'lib':
        target = ['--lib']
    elif row['target'].startswith('bin:'):
        target = ['--bin', row['target'][4:]]
    elif row['target'].startswith('example:'):
        target = ['--example', row['target'][8:]]
    else:
        target = ['--test', row['target']]
    command = [sys.executable, str(args.cargo_helper), log, 'test', '-p', row.get('crate', 'grapt-mail-transport'), '--no-fail-fast', *target, row['witness'], '--', '--exact']
    result = subprocess.run(command, check=False)
    body = (args.evidence / f"{row['id']}-{phase}.log").read_text()
    return result.returncode, body

for row in rows:
    path = Path(row['path'])
    if not path.resolve().is_relative_to(Path.cwd().resolve()):
        raise SystemExit('mutation path escapes its owned root')
    original = path.read_bytes()
    before, after = row['before'].encode(), row['after'].encode()
    if original.count(before) != 1:
        raise SystemExit(f"{row['id']}: target must occur exactly once")
    try:
        path.write_bytes(original.replace(before, after, 1))
        code, body = check(row, 'red')
        named = f"test {row['witness']} ... FAILED" in body
        if code == 0 or not named or 'could not compile' in body:
            raise RuntimeError(f"{row['id']}: mutation did not redden its named executable witness")
        if row['target'] == 'compile_fail' and 'Expected test case to fail to compile, but it succeeded.' not in body:
            raise RuntimeError('seal probe failed for another reason')
    finally:
        path.write_bytes(original)
    if path.read_bytes() != original:
        raise SystemExit('source restoration failed')
    code, body = check(row, 'green')
    if code != 0 or f"test {row['witness']} ... ok" not in body:
        raise SystemExit(f"{row['id']}: restored witness not green")
    results.append({'id': row['id'], 'law': row['law'], 'red': 'named executable failure', 'restored_green': True})
    (args.evidence / 'result.json').write_text(json.dumps({'schema':1,'rows':results,'complete':len(results)==len(rows)},indent=2)+'\n')
print(f'{len(results)} authority mutants red then restored green')
