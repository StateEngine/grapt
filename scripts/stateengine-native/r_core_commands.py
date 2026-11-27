#!/usr/bin/env python3
"""R core-command PTY journey with ordinary binaries and disposable TLS/MCP peers.

Run under the normal heavy scheduler. Uses exclusively created profiles in a
private network namespace; no real accounts or paid calls.
"""
import argparse
import json
import shutil
from pathlib import Path
import ssl
import subprocess
import sys
import threading

from acceptance_peer import Model, Server, PROJECT, AGENT, CAPABILITY
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import text
from sustained_walk import committed, idle
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]
SUMMARY = """## Objective
- Verify R core commands.
## Important Details
- CORE_BEFORE_COMPACT completed.
## Work State
### Completed
- The initial prompt completed.
### Active
- Prepare repository guidance without replacing existing AGENTS.md.
### Blocked
- none
## Next Move
1. Continue the requested guidance task.
## Relevant Files
- AGENTS.md is an existing fixture file and must be preserved.
"""


class CoreModel(Model):
    def __init__(self):
        super().__init__()
        self.interrupt_entered, self.interrupt_release = threading.Event(), threading.Event()

    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        encoded = json.dumps(body)
        if self.count() == 5:
            require('CORE_INTERRUPT' in encoded, 'unexpected interrupt fixture prompt')
            self.interrupt_entered.set()
            require(self.interrupt_release.wait(20), 'interrupt fixture was not released')
            return text('CORE_INTERRUPT_RELEASED')
        if self.count() == 1:
            require('/compact' in encoded, 'literal startup prompt changed')
            return text('Fixture context for manual compaction.\n' * 200 + 'CORE_LITERAL_STARTUP_DONE')
        if "Output exactly the Markdown structure shown inside <template>" in encoded:
            return text(SUMMARY)
        if "Generate a file named AGENTS.md" in encoded:
            require("do not overwrite or modify it" in encoded, "init lost preservation instruction")
            return text("CORE_INIT_PRESERVED")
        require("CORE_BEFORE_COMPACT" in encoded, "a slash command reached the model")
        return text("CORE_BEFORE_COMPACT_DONE")


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail = CoreModel(), TuiMail()
    provider = Server(0, model.dispatch)
    context = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    context.minimum_version = ssl.TLSVersion.TLSv1_2
    context.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = context.wrap_socket(provider.socket, server_side=True)
    journey, terminal = None, None
    result = dict(passed=False, source=args.source, frontend='R', paid_calls=0,
                  seat_day='not measured', windows='not measured')
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            # Service configuration is user-owned; the repository is separate.
            shutil.rmtree(journey.profile / '.git')
            journey.workspace = journey.root / 'workspace'
            journey.workspace.mkdir()
            subprocess.run(['git', 'init', '--quiet', str(journey.workspace)], check=True, timeout=15)
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text())
            config['compaction']['auto'] = False
            write(config_path, config)
            sentinel = 'Existing fixture guidance: preserve this file.\n'
            (journey.workspace / 'AGENTS.md').write_text(sentinel)
            (journey.workspace / 'core-diff.txt').write_text('CORE_WORKSPACE_CHANGE\n')
            journey.start(0)
            identity = {'scope': 'project', 'project_id': PROJECT, 'agent_id': AGENT,
                        'mail_transport_protocol': 'remote-seat-mail-transport/v2'}
            provision = journey.cli(['session', 'provision', '--service', str(journey.profile),
                '--session', SESSION, '--agent', 'agt_live_headless1', '--model', 'openai/gpt-live',
                '--location', str(journey.workspace), '--identity-entry', json.dumps(identity),
                '--packet-entry', json.dumps(journey.pointer)], 'provision', (CAPABILITY+'\n').encode())
            require(provision.returncode == 0, 'disposable provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0, prompt='/compact', directory=journey.workspace)
            terminal.expect('CORE_LITERAL_STARTUP_DONE')
            idle(journey)

            def local(value, expected, capture):
                before = model.count()
                terminal.send(value)
                terminal.expect(expected)
                terminal.capture(capture)
                require(model.count() == before, value + ' invoked the provider')

            def close_pane(marker):
                terminal.drive('send', 'Escape')
                terminal.until(lambda screen: marker not in screen, 'closed ' + marker, seconds=10)

            terminal.drive('send', '--text', '\x1b[200~KEEP_IDLE_DRAFT\x1b[201~')
            terminal.expect('KEEP_IDLE_DRAFT')
            terminal.drive('send', 'Escape')
            terminal.expect('Sessions · Enter open')
            terminal.capture('idle-escape-sessions')
            close_pane('Sessions · Enter open')
            require('KEEP_IDLE_DRAFT' in terminal.drive('capture'), 'idle Escape changed the draft')
            terminal.drive('send', 'C-a', 'C-k')
            for invalid in ['/help extra', '/status extra', '/compact extra', '/clear extra']:
                local(invalid, 'Use ' + invalid.split()[0], 'invalid-core-argument')
                terminal.drive('send', 'C-a', 'C-k')
            local('/help', 'CORE COMMANDS', 'help')
            close_pane('Command result')
            local('/status', 'Usage · live session', 'status')
            close_pane('Usage · live session')
            local('/model', 'Models and effort', 'models')
            close_pane('Models and effort')
            local('/diff', 'WORKSPACE DIFF', 'diff')
            def find_diff_marker(screen):
                if 'CORE_WORKSPACE_CHANGE' in screen:
                    return True
                terminal.drive('send', 'PageDown')
                return False
            terminal.until(find_diff_marker, 'paged workspace diff marker')
            terminal.capture('diff-marker')
            command(['tmux', 'resize-window', '-t', terminal.session, '-x', '60', '-y', '20'])
            terminal.drive('send', 'End')
            terminal.capture('diff-narrow-end')
            command(['tmux', 'resize-window', '-t', terminal.session, '-x', '160', '-y', '45'])
            close_pane('Command result')
            local('/mcp', 'MCP SERVERS', 'mcp-summary')
            close_pane('Command result')
            local('/mcp verbose', 'MCP DETAILS', 'mcp-details')
            terminal.expect('stateengine')
            terminal.drive('send', 'End')
            terminal.expect('Session permissions and policy')
            terminal.capture('mcp-details-end')
            close_pane('Command result')
            command(['tmux', 'resize-window', '-t', terminal.session, '-x', '80', '-y', '24'])
            local('/new extra', 'Use /new to start', 'new-argument-refusal')
            command(['tmux', 'resize-window', '-t', terminal.session, '-x', '160', '-y', '45'])
            terminal.drive('send', 'C-a', 'C-k')
            local('/new', 'New session started', 'new-session')
            local('/clear', 'New session started', 'clear-session')
            require(journey.api('GET', '/api/session/' + SESSION)['data']['id'] == SESSION,
                    'clear removed the saved session')
            local('/resume ' + SESSION, 'Session resumed with its saved', 'resume-id')
            local('/resume', 'Sessions', 'resume-picker')
            close_pane('Sessions · Enter open')
            terminal.send('CORE_BEFORE_COMPACT')
            terminal.expect('CORE_BEFORE_COMPACT_DONE')
            idle(journey)
            local('/compact', 'Compaction will run before your next message.', 'compact-pending')
            require(not any(kind == 'session.compaction.ended.2' for kind, _ in committed(journey)),
                    'compact ran before the next message')
            terminal.send('/init')
            terminal.expect('CORE_INIT_PRESERVED')
            idle(journey)
            rows = committed(journey)
            inputs = [value['input']['data']['text'] for kind, value in rows
                      if kind == 'session.input.admitted.1']
            require(len(inputs) == 3 and inputs[:2] == ['/compact', 'CORE_BEFORE_COMPACT'] and
                    inputs[2].startswith('Generate a file named AGENTS.md'), 'wrong prompt admissions')
            require(any(kind == 'session.compaction.ended.2' for kind, _ in rows), 'requested compaction missing')
            require(not any(kind == 'session.compaction.failed.1' for kind, _ in rows), 'requested compaction did not reduce context')
            require((journey.workspace / 'AGENTS.md').read_text() == sentinel, 'init modified existing guidance')
            require(model.count() == 4 and not provider.errors and not peer.errors, 'extra or failed model request')
            terminal.capture('init-after-compact')
            terminal.send('CORE_INTERRUPT')
            wait(model.interrupt_entered.is_set, 'interrupt fixture provider entered')
            terminal.expect('Esc interrupt')
            terminal.drive('send', '--text', '\x1b[200~KEEP_INTERRUPT_DRAFT\x1b[201~')
            terminal.expect('KEEP_INTERRUPT_DRAFT')
            terminal.drive('send', 'Escape')
            wait(lambda: any(kind == 'session.execution.interrupted.1' for kind, _ in committed(journey)),
                 'running Escape interrupts execution')
            model.interrupt_release.set()
            idle(journey)
            terminal.until(lambda screen: 'Esc interrupt' not in screen, 'idle after interrupt')
            screen = terminal.capture('running-escape-interrupt')
            require('KEEP_INTERRUPT_DRAFT' in screen and 'Sessions · Enter open' not in screen,
                    'running Escape lost the draft or opened sessions')
            require(model.count() == 5 and not provider.errors and not peer.errors,
                    'extra or failed interrupt model request')
            inputs = [value['input']['data']['text'] for kind, value in committed(journey)
                      if kind == 'session.input.admitted.1']
            require(len(inputs) == 4 and inputs[-1] == 'CORE_INTERRUPT', 'interrupt submitted the draft')
            terminal.close()
            journey.stop()
            result.update(passed=True, commands=8, provider_requests=model.count(), prompt_admissions=len(inputs),
                          shared_init_prompt=True, next_input_compaction=True, read_only_mcp=True,
                          literal_startup_prompt=True, idle_escape_sessions=True, running_escape_interrupt=True)
        finally:
            model.interrupt_release.set()
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                journey.cleanup()
                write(journey.root / 'model-requests.json', model.bodies)
                write(journey.root / 'events.json', committed(journey))
            write(args.scratch / 'result.json', result)
    print(json.dumps(result), flush=True)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binaries', type=Path, required=True)
    parser.add_argument('--source', required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    parser.add_argument('--inside', action='store_true', help=argparse.SUPPRESS)
    args = parser.parse_args()
    if args.inside:
        return run(args)
    args.binaries, args.scratch = args.binaries.resolve(strict=True), args.scratch.resolve()
    require(args.scratch != ROOT and ROOT not in args.scratch.parents, 'scratch must be outside source')
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable, str(ROOT / 'scripts/native-tui/fixture-ca.py'), '--root', str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--ro-bind', str(ROOT), str(ROOT), '--ro-bind', str(args.binaries), str(args.binaries),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch)], check=True, timeout=240)


if __name__ == '__main__':
    main()
