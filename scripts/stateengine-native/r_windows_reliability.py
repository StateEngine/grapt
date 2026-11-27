#!/usr/bin/env python3
"""Run row 4's existing fault journey natively, in a private Windows ConPTY.

Uses labeled loopback fixtures; does not touch system trust, port 4877, native
seat binding or a producer profile. Those remain the real producer's witness.
Supply the retained observer's node_modules via --node-modules.
"""
import argparse
import json
import os
from pathlib import Path
import sys

from headless_acceptance import require, write
from r_sustained_walk import run
from r_windows_terminal import WindowsTerminal

STEPS = [
    'native ConPTY startup and private Session',
    'pending approval disconnect and explicit retry',
    'background tools, automatic compaction and queued edited input',
    'same R reconnect with rotated endpoint, credential and retained draft',
    'uncertain input explicit retry and repeated compaction continuation',
    'pending question crash, original R reconnect and explicit answer',
    'lost nested tool reconciliation is idempotent without replay',
    'tested coding continuation preserves the single pre-crash effect',
    'three committed automatic compactions',
    'clean offline exit and exact input, approval and effect settlement',
]


class Steps:
    def __init__(self, root):
        self.root, self.rows, self.commands, self.offset = root, [], [], 0
        self.failure=None

    def record(self, kind, command):
        require(len(self.commands)<4096, 'journey command count bound')
        row=dict(kind=kind, command=command)
        require(len(json.dumps(row))<=131072, 'journey command frame bound')
        self.commands.append(row)
        with (self.root/'commands.jsonl').open('a', encoding='utf-8') as output:
            output.write(json.dumps(row)+'\n')

    def checkpoint(self, name):
        require(len(self.rows)<len(STEPS) and STEPS[len(self.rows)]==name, 'unexpected journey phase')
        self.rows.append(dict(step=name, state='PASS', commands=self.commands[self.offset:]))
        self.offset=len(self.commands)
        self.finish()

    def finish(self, error=None):
        if error:
            self.failure=str(error)[:2048]
        rows=list(self.rows)
        if self.failure and len(rows)<len(STEPS):
            rows.append(dict(step=STEPS[len(rows)], state='FAIL', error=self.failure,
                             commands=self.commands[self.offset:]))
        rows.extend(dict(step=name, state='NOT RUN', commands=[]) for name in STEPS[len(rows):])
        write(self.root/'steps.json', rows)
        lines=['| Step | Result | Exact commands |', '| --- | --- | --- |']
        for row in rows:
            commands=[]
            for command in row['commands']:
                kind, value=command['kind'], command['command']
                if kind=='ConPTY' and value.get('action') in ('screen','expect'):
                    continue
                commands.append(json.dumps(command, ensure_ascii=True).replace('|','&#124;'))
            lines.append('| '+row['step']+' | '+row['state']+' | '+ '<br>'.join(commands)+' |')
        (self.root/'steps.md').write_text('\n'.join(lines)+'\n', encoding='utf-8')


def observe(args, journey):
    original_api, original_start, original_stop = journey.api, journey.start, journey.stop

    def api(method, route, body=None):
        args.record('HTTP', dict(method=method, route=route, body=body))
        return original_api(method, route, body)

    def start(index):
        args.record('process start', dict(argv=[str(args.product), 'serve-terminal', '--service',
            str(journey.profile), '--port', '0'], index=index))
        return original_start(index)

    def stop():
        args.record('process stop', dict(action='close owned control stdin',
            pid=journey.child.pid if journey.child else None))
        return original_stop()

    journey.api, journey.start, journey.stop = api, start, stop


def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--node-modules', type=Path, required=True)
    parser.add_argument('--node', default='node')
    parser.add_argument('--smoke', action='store_true', help='startup and clean exit only; no reliability pass')
    args=parser.parse_args()
    require(os.name=='nt', 'this carrier must execute natively on Windows')
    args.binaries=args.binaries.resolve(strict=True)
    args.node_modules=args.node_modules.resolve(strict=True)
    args.scratch=args.scratch.resolve()
    require(args.scratch.as_posix().lower().startswith('e:/temp/gamma-'), 'private E:/temp/gamma-* root required')
    args.scratch.mkdir()
    args.loopback=args.session_only=True
    args.terminal_class=WindowsTerminal
    steps=Steps(args.scratch)
    args.record, args.checkpoint=steps.record, steps.checkpoint
    args.observe_profile=lambda journey:observe(args, journey)
    steps.record('invocation', dict(argv=[sys.executable, *sys.argv]))
    try:
        run(args)
        require(len(steps.rows)==(1 if args.smoke else len(STEPS)), 'journey omitted required phases')
    except BaseException as error:
        steps.finish(error)
        raise
    finally:
        steps.finish()


if __name__=='__main__':
    main()
