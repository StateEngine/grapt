#!/usr/bin/env python3
"""N2 on native Windows: shared ConPTY carrier, private profiles, fixture model only."""
import argparse
import copy
import json
import os
from pathlib import Path
import re
import sys

from headless_acceptance import require, write

COMPOSER = [
    'native ConPTY and isolated fixture Session',
    'background output, Once approvals, edited queue and automatic compaction',
    'hard-kill restart reattaches original R with unsent draft intact',
    'post-crash tested work and exact input, approval and background identities',
]
WORKTREE = [
    'spawn_agent creates detached nested Windows worktree from committed files',
    'background child writes only its worktree and retains a real terminal handle',
    'child output and worktree placement survive a hard-kill service restart',
    'open-file cleanup refuses and preserves the external junction target',
    'closed-handle cleanup succeeds without deleting source or junction target',
]


class Report:
    def __init__(self, root, plan):
        self.root, self.plan, self.rows, self.commands, self.offset = root, plan, [], [], 0

    def record(self, kind, value):
        require(len(self.commands) < 4096, 'N2 command count exceeded')
        row = dict(kind=kind, command=value)
        require(len(json.dumps(row)) <= 131072, 'N2 command frame exceeded')
        self.commands.append(row)
        with (self.root / 'commands.jsonl').open('a', encoding='utf-8') as output:
            output.write(json.dumps(row) + '\n')

    def passed(self, name):
        require(len(self.rows) < len(self.plan) and self.plan[len(self.rows)] == name,
                'N2 omitted or reordered a required step')
        self.rows.append(dict(step=name, state='PASS', commands=self.commands[self.offset:]))
        self.offset = len(self.commands)
        self.finish()

    def finish(self, error=None):
        rows = list(self.rows)
        if error and len(rows) < len(self.plan):
            rows.append(dict(step=self.plan[len(rows)], state='FAIL', error=str(error)[:2048],
                             commands=self.commands[self.offset:]))
        rows.extend(dict(step=name, state='NOT RUN', commands=[]) for name in self.plan[len(rows):])
        write(self.root / 'steps.json', rows)
        lines = ['| Step | Result | Commands |', '| --- | --- | --- |']
        for row in rows:
            commands = [json.dumps(command, ensure_ascii=True).replace('|', '&#124;')
                        for command in row['commands'] if not
                        (command['kind'] == 'ConPTY' and
                         command['command'].get('action') in ('screen', 'expect'))]
            lines.append('| ' + row['step'] + ' | ' + row['state'] + ' | ' +
                         '<br>'.join(commands) + ' |')
        (self.root / 'steps.md').write_text('\n'.join(lines) + '\n', encoding='utf-8')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--node-modules', type=Path, required=True)
    parser.add_argument('--node', default='node')
    parser.add_argument('--phase', choices=('all', 'composer', 'worktree'), default='all')
    parser.add_argument('--smoke', action='store_true')
    args = parser.parse_args()
    require(os.name == 'nt', 'N2 must run natively on Windows')
    args.scratch = args.scratch.resolve()
    require(args.scratch.parent == Path('E:/temp') and args.scratch.name.startswith('theta-'),
            'N2 requires a fresh private E:/temp/theta-* root')
    require(re.fullmatch('[0-9a-f]{40}', args.source), 'N2 requires a full source commit')
    args.binaries = args.binaries.resolve(strict=True)
    args.node_modules = args.node_modules.resolve(strict=True)
    require((args.binaries / 'source.txt').read_text().strip() == args.source,
            'staged binary source differs')
    for field, name in [('cli', 'grapt.exe'), ('product', 'grapt-server.exe'),
                        ('tui', 'grapt-tui.exe')]:
        binary = args.binaries / name
        require(binary.is_file(), 'missing N2 binary ' + name)
        setattr(args, field, binary)
    args.scratch.mkdir()
    args.loopback = args.session_only = True
    plan = COMPOSER[:1] if args.smoke else (
        (COMPOSER if args.phase != 'worktree' else []) +
        (WORKTREE if args.phase != 'composer' else []))
    report = Report(args.scratch, plan)
    args.record = report.record
    report.record('invocation', dict(argv=[sys.executable, *sys.argv]))
    result = dict(passed=False, source=args.source, native_windows=True,
                  transport='Windows ConPTY', fixture_binaries=True, paid_calls=0,
                  smoke=args.smoke, phase=args.phase, real_seat_day='not measured')
    try:
        for phase in (['composer'] if args.smoke else
                      ['composer', 'worktree'] if args.phase == 'all' else [args.phase]):
            options = copy.copy(args)
            options.scratch = args.scratch / phase
            options.scratch.mkdir()
            if phase == 'composer':
                from n2_composer import run
            else:
                from n2_worktree import run
            run(options, report)
        require(len(report.rows) == len(plan), 'N2 did not complete all declared steps')
        result['passed'] = True
    except BaseException as error:
        result['error'] = str(error)[:2048]
        report.finish(error)
        raise
    finally:
        write(args.scratch / 'result.json', result)


if __name__ == '__main__':
    main()
