#!/usr/bin/env python3
"""Check actual R notification bytes in a PTY with disposable, nonbillable services."""
import argparse
import base64
import fcntl
import json
import os
from pathlib import Path
import sys
sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "native-tui"))
from scripted_profile import ScriptedProfile
import pty
import re
import select
import signal
import struct
import subprocess
import termios
import time
import urllib.request
from urllib.parse import urlencode

LIMIT = 2 * 1024 * 1024
FINISHED = b'Fixturecomplete.'


def require(condition, message):
    if not condition:
        raise RuntimeError(message)


class Terminal:
    def __init__(self, args, name, *, osc=True, disabled=False, approval=False, foreign=False):
        self.root = args.scratch / name
        self.root.mkdir(mode=0o700)
        self.profile = self.root / 'profile'
        self.backend = self.frontend = self.master = self.provider = None
        self.output = bytearray()
        self.env = dict(os.environ)
        for key in ('GRAPT_CONFIG', 'OPENCODE_CONFIG', 'OPENCODE_CONFIG_CONTENT',
                    'GRAPT_PLUGIN_CONFIG', 'SECODE_STATEENGINE_MCP_CONFIG',
                    'TMUX', 'TMUX_PANE', 'GHOSTTY_RESOURCES_DIR', 'WEZTERM_VERSION',
                    'ITERM_SESSION_ID', 'ITERM_PROFILE', 'ITERM_PROFILE_NAME', 'KITTY_WINDOW_ID'):
            self.env.pop(key, None)
        self.env.update(TERM='xterm-256color', TERM_PROGRAM='WezTerm' if osc else 'unknown')
        self.args, self.disabled, self.approval, self.foreign = args, disabled, approval, foreign

    def start(self):
        calls = ([{'tool': 'write', 'arguments': {'path': 'answer.txt', 'content': '42\n'}}]
                 if self.approval else [{'tool': 'glob', 'arguments': {'pattern': '*.txt'}}])
        self.provider = ScriptedProfile(self.args.server, self.profile, self.env, calls)
        self.provider.__enter__()
        (self.root / 'init.json').write_text(json.dumps(self.provider.receipt), encoding='utf-8')
        if self.approval:
            (self.profile / 'answer.txt').write_text('original\n')
        subprocess.run(['git', 'init', '--quiet', str(self.profile)], check=True, timeout=10)
        with (self.root / 'server.stdout').open('xb') as stdout, (self.root / 'server.stderr').open('xb') as stderr:
            self.backend = subprocess.Popen(
                [str(self.args.server), 'serve-terminal', '--service', str(self.profile), '--port', '0'],
                stdin=subprocess.PIPE, stdout=stdout, stderr=stderr, env=self.env, start_new_session=True)
        self.wait(lambda: (self.profile / 'serve-ready.json').exists(), 'server readiness', 30)
        ready = json.loads((self.profile / 'serve-ready.json').read_text())
        self.endpoint = ready['observed_endpoint']
        self.headers = {'Authorization': 'Basic ' + base64.b64encode(
            (self.profile / 'basic.credential').read_bytes().strip()).decode(),
            'Content-Type': 'application/json'}
        agents = self.call('/api/agent?' + urlencode({'location[directory]': str(self.profile)}))['data']
        agent = next((row['id'] for row in agents if not row['hidden']), None)
        require(agent is not None, 'fixture has no offered agent')
        self.session = self.call('/api/session', {'location': {'directory': str(self.profile)},
            'agent': agent, 'model': {'id': 'fixture-model', 'providerID': 'openai'}})['data']['id']
        if self.approval:
            self.call(f'/api/session/{self.session}/preset', {'preset': 'author'})
        displayed = (self.call('/api/session', {'location': {'directory': str(self.profile)},
            'agent': agent, 'model': {'id': 'fixture-model', 'providerID': 'openai'}})['data']['id']
            if self.foreign else self.session)
        self.master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
        command = [str(self.args.tui), '--service', str(self.profile), '--session', displayed, '--dark']
        if self.disabled:
            command.append('--no-notifications')
        try:
            self.frontend = subprocess.Popen(command, stdin=slave, stdout=slave, stderr=slave,
                env=self.env, start_new_session=True, preexec_fn=lambda: fcntl.ioctl(0, termios.TIOCSCTTY, 0))
        finally:
            os.close(slave)
        # The selected session title arrives with the attachment update.
        # Ratatui may paint "Connected" as a diff over "Connecting".
        self.wait(lambda: b'New session' in self.output, 'attached session', 30)

    def call(self, path, body=None, method=None):
        request = urllib.request.Request(self.endpoint + path, headers=self.headers, method=method,
            data=json.dumps(body).encode() if body is not None else None)
        with urllib.request.urlopen(request, timeout=10) as response:
            data = response.read(LIMIT + 1)
        require(len(data) <= LIMIT, 'HTTP output bound exceeded')
        return json.loads(data) if data else None

    def drain(self, seconds=.05):
        if self.master is not None and select.select([self.master], [], [], seconds)[0]:
            try:
                data = os.read(self.master, 65536)
            except OSError:
                data = b''
            self.output.extend(data)
            require(len(self.output) <= LIMIT, 'PTY output bound exceeded')

    def wait(self, predicate, label, seconds=10):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            self.drain()
            if predicate():
                return
            require(self.backend is None or self.backend.poll() is None, 'server exited before ' + label)
            require(self.frontend is None or self.frontend.poll() is None, 'R exited before ' + label)
            if self.master is None:
                time.sleep(.05)
        raise RuntimeError('Did not observe ' + label)

    def settle(self):
        deadline = time.monotonic() + .5
        while time.monotonic() < deadline:
            self.drain()

    def question(self, name):
        form = 'frm_notify_' + name
        self.call(f'/api/session/{self.session}/form', {'id': form, 'title': 'Notify question ' + name,
            'fields': [{'key': 'answer', 'type': 'string', 'required': True}]})
        return form

    def painted_text(self):
        # Cursor moves may encode spaces, and incremental frames split words.
        # Notification assertions still use the untouched output bytes.
        return re.sub(rb'\s+', b'', re.sub(rb'\x1b\[[0-?]*[ -/]*[@-~]', b'', self.output))

    def close(self):
        try:
            self.close_clients()
        finally:
            if self.provider is not None:
                self.provider.close()

    def close_clients(self):
        errors = []
        for process, label in ((self.frontend, 'frontend'), (self.backend, 'backend')):
            if process is None:
                continue
            if process.poll() is None:
                if label == 'frontend':
                    os.write(self.master, b'\x03')
                else:
                    process.stdin.close()
                try:
                    process.wait(timeout=20)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=10)
                    errors.append(label + ' needed SIGTERM')
            if process.returncode != 0:
                errors.append(f'{label} exited {process.returncode}')
        if self.master is not None:
            self.drain(0)
            os.close(self.master)
        (self.root / 'frontend.vt').write_bytes(self.output)
        require(not errors, '; '.join(errors))


def exercise(args, name, *, osc=True, disabled=False, approval=False, focused=False, foreign=False):
    terminal = Terminal(args, name, osc=osc, disabled=disabled, approval=approval, foreign=foreign)
    try:
        terminal.start()
        require(b'\x1b[?1004h' in terminal.output, 'focus reporting was not enabled')
        os.write(terminal.master, b'\x1b[I' if focused else b'\x1b[O')
        if foreign:
            terminal.call(f'/api/session/{terminal.session}/prompt', {'text': 'notify check'})
        else:
            os.write(terminal.master, b'notify check\r')
        if approval:
            terminal.wait(lambda: b'\x1b]9;Grapt: approval needed\x07' in terminal.output,
                          'approval notification')
            terminal.settle()
            require(terminal.output.count(b'\x1b]9;Grapt: approval needed\x07') == 1,
                    'approval notification repeated')
            return
        terminal.wait(lambda: FINISHED in terminal.painted_text(), 'local turn completion', 20)
        if not disabled and not focused:
            expected = b'\x1b]9;Grapt: turn complete\x07' if osc else b'\x07'
            terminal.wait(lambda: expected in terminal.output, 'completion notification')
        terminal.settle()
        expected_count = 0 if disabled or focused else 1
        require(terminal.output.count(b'\x07') == expected_count, 'unexpected completion bell count')
        if osc and not disabled and not focused:
            require(terminal.output.count(b'\x1b]9;Grapt: turn complete\x07') == 1,
                    'wrong OSC 9 completion bytes')
        if osc and not focused:
            os.write(terminal.master, b'\x1b[I')
            terminal.question(name)
            if disabled:
                terminal.wait(lambda: b'Notifyquestion' in terminal.painted_text(), 'disabled question display')
            else:
                terminal.wait(lambda: b'\x1b]9;Grapt: question waiting\x07' in terminal.output,
                              'question notification while focused')
            terminal.settle()
            require(terminal.output.count(b'\x07') == (0 if disabled else 2),
                    'question notification missing, repeated, or emitted while disabled')
    finally:
        terminal.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--server', type=Path, required=True)
    parser.add_argument('--tui', type=Path, required=True)
    parser.add_argument('--scratch', type=Path, required=True)
    args = parser.parse_args()
    args.server, args.tui = args.server.resolve(strict=True), args.tui.resolve(strict=True)
    args.scratch = args.scratch.resolve()
    root = Path(__file__).resolve().parents[2]
    require(args.scratch != root and root not in args.scratch.parents, 'scratch must be outside source')
    os.umask(0o077)
    args.scratch.mkdir(mode=0o700)
    exercise(args, 'osc')
    exercise(args, 'bel', osc=False)
    exercise(args, 'disabled', disabled=True)
    exercise(args, 'focused', focused=True)
    exercise(args, 'approval', focused=True, approval=True)
    exercise(args, 'foreign-approval', focused=True, approval=True, foreign=True)
    print(json.dumps({'passed': True, 'cases': 6, 'external_provider_calls': 0}))


if __name__ == '__main__':
    main()
