#!/usr/bin/env python3
"""Packaged Windows tools through system or modern offscreen ConPTY; no paid model."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import struct
import sys
import zlib

from acceptance_peer import Server
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from r_windows_n2 import Report
from r_windows_terminal import WindowsTerminal
from sustained_walk import committed, idle
from windows_tools_provider import INITIAL, SOURCE, UPDATED, WorkflowModel, plan

STEPS = [
    'packaged R startup in the declared offscreen ConPTY backend',
    'background PTY build/test, output polling and Ctrl-C with owned-child cleanup',
    'Unicode add/update/move/delete patches preserve exact expected bytes',
    'native undo restores the original bytes without changing conversation history',
    'unfinished plan survives compaction and is completed through update_plan',
    'ordinary service and R reopen retain the completed plan without another model call',
    'project instructions, skills, image, public fetch and both MCP transports reach the model',
]
COMMAND = '''import json,os,py_compile,runpy,subprocess,sys,time
from pathlib import Path
import psutil
if sys.argv[1]=='build':
 print('BUILD_READY',flush=True)
 time.sleep(2)
 py_compile.compile(sys.argv[2],doraise=True)
 code=runpy.run_path(sys.argv[2])
 assert code['add'](-2,5)==3
 assert code['add'](0,0)==0
 print('BUILD_DONE: compilation and two signed-addition checks passed',flush=True)
else:
 child=subprocess.Popen([sys.executable,'-c','import time;time.sleep(600)'])
 rows=[dict(pid=p.pid,created=p.create_time()) for p in [psutil.Process(),psutil.Process(child.pid)]]
 Path('command-pids.json').write_text(json.dumps(rows),encoding='utf-8')
 print('INTERRUPT_READY',flush=True)
 time.sleep(600)
'''


def verify_package(args):
    manifest = json.loads((args.package / 'release-manifest.json').read_text(encoding='utf-8'))
    require(manifest['engine_version'] == args.source and
            manifest['target'] == 'x86_64-pc-windows-msvc', 'package source/target differs')
    for name in ('grapt.exe', 'grapt-tui.exe'):
        data = (args.package / name).read_bytes()
        row = manifest['binaries'][name]
        require(len(data) == row['bytes'] and hashlib.sha256(data).hexdigest() == row['sha256'],
                'packaged binary differs: ' + name)
    require((args.fixture / 'source.txt').read_text().strip() == args.source,
            'fixture Product source differs')
    args.cli, args.tui = args.package / 'grapt.exe', args.package / 'grapt-tui.exe'
    args.product = args.fixture / 'grapt-server.exe'


def file_state(workspace):
    names = [SOURCE, 'répertoire/ancien.txt', 'répertoire/déplacé.txt',
             'répertoire/新規.txt', 'répertoire/削除.txt']
    return {name: (workspace / name).read_bytes() if (workspace / name).exists() else None
            for name in names}


def parity_inputs(journey, workspace):
    (workspace / 'AGENTS.md').write_text('Follow AGENTS_PARITY_REQUIRED.\n', encoding='utf-8')
    skill = workspace / '.agents/skills/parity-check'
    skill.mkdir(parents=True)
    (skill / 'SKILL.md').write_text(
        '---\nname: Parity check\ndescription: Check the Windows tool smoke.\n---\nSKILL_PARITY_BODY\n',
        encoding='utf-8')
    def chunk(kind, data):
        return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
    (workspace / 'parity.png').write_bytes(b'\x89PNG\r\n\x1a\n'
        + chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0))
        + chunk(b'IDAT', zlib.compress(b'\x00\x20\x60\xa0\xff')) + chunk(b'IEND', b''))
    user_config = Path(journey.environment['GRAPT_CONFIG_DIR']) / 'grapt.json'
    config = json.loads(user_config.read_text(encoding='utf-8'))
    config['web_search'] = 'live'
    write(user_config, config)
    config_path = journey.profile / 'grapt.jsonc'
    config = json.loads(config_path.read_text(encoding='utf-8'))
    config['permission'].update(webfetch='allow', skill='allow', **{'parity_*': 'allow'})
    write(config_path, config)
    catalog_path = journey.profile / 'model-catalog.json'
    catalog = json.loads(catalog_path.read_text(encoding='utf-8'))
    catalog['openai']['models']['gpt-live']['modalities']['input'] = ['text', 'image']
    write(catalog_path, catalog)


def plans(history):
    return [part['state']['input'] for message in history['data']
            for part in message.get('content', [])
            if part.get('type') == 'tool' and part.get('name') == 'update_plan'
            and part['state']['status'] == 'completed']


def same_process(row):
    import psutil
    try:
        process = psutil.Process(row['pid'])
        return process if process.create_time() == row['created'] else None
    except psutil.NoSuchProcess:
        return None


def show_completed_plan(terminal):
    # Disclose the latest group, then its plan through the public keyboard path.
    terminal.drive('send', '--text', '\x1b[Z')
    terminal.drive('send', 'Enter')
    terminal.expect('▾ Updated plan')
    terminal.drive('send', 'Escape')
    terminal.drive('send', '--text', '\x1b[Z')
    terminal.drive('send', 'Enter')
    terminal.expect('Plan updated · 3/3 complete')
    for step in plan(True)['plan']:
        terminal.expect('[x] ' + step['step'])
    terminal.drive('send', '--text', '\x1b[1;1F')


def run(args, report):
    model, journey, terminal = WorkflowModel(), None, None
    pids = []
    with Server(0, model.dispatch) as provider, Server(0, model.mcp) as remote:
        try:
            journey = OrdinaryProfile(args, provider.endpoint, mcp_servers={
                'parity_stdio': dict(type='local', codemode=False, command=[args.node,
                    str(Path(__file__).parents[1] / 'daily-use/journey_mcp_server.js'),
                    str(args.scratch / 'stdio-mcp.jsonl')]),
                'parity_remote': dict(type='remote', codemode=False, url=remote.endpoint + '/mcp'),
            })
            journey.environment['GRAPT_CONPTY_BACKEND'] = args.backend
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text(encoding='utf-8'))
            config.pop('mcp', None)
            write(config_path, config)
            workspace = args.scratch / 'workspace'
            (workspace / 'répertoire').mkdir(parents=True)
            (workspace / SOURCE).write_bytes(INITIAL)
            (workspace / 'répertoire/ancien.txt').write_bytes(b'before\r\n')
            (workspace / 'répertoire/削除.txt').write_bytes(b'deleted\r\n')
            (workspace / 'workflow_command.py').write_text(COMMAND, encoding='utf-8')
            parity_inputs(journey, workspace)
            original = file_state(workspace)
            journey.start(0)
            journey.api('POST', '/api/session', dict(id=SESSION, agent='agt_live_headless1',
                model=dict(providerID='openai', id='gpt-live'), location=dict(directory=str(workspace))))
            terminal = WindowsTerminal(args, journey)
            terminal.start(0, directory=workspace)
            import psutil
            modules = [row.path for row in psutil.Process(terminal.observer.pid).memory_maps()
                       if Path(row.path).name.lower() == 'conpty.dll']
            require(bool(modules) == (args.backend == 'modern'),
                    'loaded ConPTY DLL does not match the declared backend')
            report.record('loaded modern ConPTY modules', modules)
            terminal.capture('startup-' + args.backend)
            report.passed(STEPS[0])
            if args.smoke:
                terminal.close()
                journey.stop()
                return
            terminal.send('WINDOWS_TOOLS: patch, build, poll, interrupt and preserve the checklist')
            terminal.expect('WINDOWS_WORKFLOW_DONE')
            idle(journey)
            pid_path = workspace / 'command-pids.json'
            require(pid_path.stat().st_size <= 1024, 'command PID evidence exceeds bound')
            pids = json.loads(pid_path.read_text(encoding='utf-8'))
            require(len(pids) == 2 and len({row['pid'] for row in pids}) == 2,
                    'command parent and child identities missing')
            wait(lambda: all(same_process(row) is None for row in pids),
                 'owned command and child exit before fixture cleanup', seconds=5)
            report.record('command results', model.outputs)
            report.record('owned process exits', pids)
            report.passed(STEPS[1])
            expected = {SOURCE: UPDATED, 'répertoire/ancien.txt': None,
                        'répertoire/déplacé.txt': b'after\r\n',
                        'répertoire/新規.txt': b'created\n', 'répertoire/削除.txt': None}
            require(file_state(workspace) == expected, 'patched bytes or Unicode paths differ')
            require(plans(journey.history()) == [plan()], 'initial mixed-status plan missing')
            report.record('patched bytes', {name: data.hex() if data is not None else None
                                           for name, data in expected.items()})
            report.passed(STEPS[2])
            before_undo = journey.history()
            terminal.send('/undo')
            terminal.expect('File undo: 3 writes, 2 deletes.')
            terminal.expect('/undo apply to restore these files.')
            terminal.capture('undo-preview')
            terminal.drive('send', 'Escape')
            terminal.send('/undo apply')
            wait(lambda: file_state(workspace) == original, 'exact original bytes after native undo')
            terminal.expect('Patch files restored.')
            terminal.drive('send', 'Escape')
            wait(lambda: 'Command result' not in terminal.drive('capture'), 'undo result pane closes')
            require(journey.history() == before_undo, 'file undo changed conversation history')
            report.passed(STEPS[3])
            journey.api('POST', f'/api/session/{SESSION}/compact', {})
            terminal.send('WINDOWS_COMPACT: finish the existing Windows workflow checklist')
            terminal.expect('WINDOWS_PLAN_DONE')
            idle(journey)
            require(model.summaries == 1 and plans(journey.history())[-1] == plan(True),
                    'completed plan or compaction missing')
            require(any(kind == 'session.compaction.ended.2' for kind, _ in committed(journey)),
                    'no committed compaction')
            show_completed_plan(terminal)
            terminal.capture('completed-plan')
            report.passed(STEPS[4])
            before_reopen, calls = journey.history(), model.count()
            terminal.close()
            journey.stop()
            journey.start(1)
            terminal = WindowsTerminal(args, journey)
            terminal.start(1, directory=workspace)
            terminal.expect('WINDOWS_PLAN_DONE')
            show_completed_plan(terminal)
            require(journey.history() == before_reopen and model.count() == calls,
                    'reopen changed history or made another model call')
            terminal.capture('reopened-plan')
            report.passed(STEPS[5])
            terminal.send('WINDOWS_ALL_TOOLS: check instructions, skill, pixels, web and both MCP transports.')
            wait(lambda: 'WINDOWS_ALL_TOOLS_DONE' in terminal.drive('capture'),
                 'all Windows tool families complete', seconds=100)
            idle(journey)
            require(model.parity_done and not remote.errors,
                    'tool family execution or remote MCP failed: ' + str(remote.errors))
            require('journey_echo' in (args.scratch / 'stdio-mcp.jsonl').read_text(encoding='utf-8'),
                    'stdio MCP peer never observed its tool call')
            report.record('hosted search scope', 'advertised search and synthetic provider framing only; retained replay and external search not measured')
            report.passed(STEPS[6])
            terminal.close()
            journey.stop()
            require(not provider.errors, 'fixture provider errors: ' + str(provider.errors))
        finally:
            write(args.scratch / 'model.json', dict(stage=model.stage, summaries=model.summaries,
                  bodies=model.bodies, outputs=model.outputs, provider_errors=provider.errors))
            if terminal:
                terminal.cleanup()
            if journey:
                journey.cleanup()
            # A failed cleanup witness stays failed; this is only its exact-child backstop.
            for row in pids:
                process = same_process(row)
                if process:
                    process.kill()
                    process.wait(timeout=5)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('package', 'fixture', 'scratch', 'node-modules'):
        parser.add_argument('--' + name, type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--node', required=True)
    parser.add_argument('--backend', choices=('system', 'modern'), required=True)
    parser.add_argument('--smoke', action='store_true')
    args = parser.parse_args()
    require(os.name == 'nt', 'run natively on Windows')
    args.scratch = args.scratch.resolve()
    parent = args.scratch.parent
    require(parent.parent == Path('E:/temp') and
            re.fullmatch(r'win-tools-[0-9a-f]{8,40}', parent.name) and
            re.fullmatch(r'[0-9a-f]{40}', args.source) and
            args.source.startswith(parent.name.removeprefix('win-tools-')) and
            re.fullmatch(r'run-[a-zA-Z0-9-]{1,80}', args.scratch.name),
            'fresh source-pinned private run directory required')
    args.scratch.mkdir()
    args.loopback = args.session_only = True
    verify_package(args)
    report = Report(args.scratch, STEPS[:1] if args.smoke else STEPS)
    args.record = report.record
    report.record('invocation', dict(argv=sys.argv, transport=args.backend, offscreen=True))
    result = dict(passed=False, source=args.source, offscreen_conpty=args.backend,
                  paid_calls=0, smoke=args.smoke, visible_host_acceptance='not measured')
    try:
        run(args, report)
        require(len(report.rows) == len(report.plan), 'workflow omitted a required step')
        result['passed'] = True
    except BaseException as error:
        report.finish(error)
        result['error'] = str(error)[:2048]
        raise
    finally:
        write(args.scratch / 'result.json', result)


if __name__ == '__main__':
    main()
