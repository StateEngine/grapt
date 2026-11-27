"""Drive the real R composer and approval controls; retain bounded PTY snapshots."""
import os
import re
import time

from headless_acceptance import SESSION, require, wait
from tui_acceptance import Terminal, command


class RTerminal(Terminal):
    def __init__(self, args, journey):
        super().__init__(args, journey)
        self.approvals = {}

    def start(self, index, prompt=None, directory=None):
        self.session = command([self.args.driver, 'start', '--seat', 'seat-a', '--journey',
            f'r-sustained-{os.getpid()}-{index}', '--width', '160', '--height', '45',
            '--cwd', self.journey.profile, '--', '/usr/bin/env', '-i',
            *[f'{key}={value}' for key, value in self.journey.environment.items()],
            self.args.tui, '--service', self.journey.profile, '--session', SESSION,
            '--directory', directory or self.journey.profile,
            *(['--prompt', prompt] if prompt is not None else [])], self.journey.environment).strip()
        self.expect('enter send')
        self.expect('gpt-live')
        self.expect('Connected')
        self.capture('connected-' + str(index))

    def until(self, predicate, label, seconds=45):
        deadline = time.monotonic() + seconds
        while time.monotonic() < deadline:
            screen = self.drive('capture')
            if '[y] Once' in screen:
                request = re.search(r'Request: (per_[a-zA-Z0-9]+)', screen)
                action = re.search(r'Action: (patch|shell):', screen)
                require(request and action, 'unidentified approval')
                identity, kind = request[1], action[1]
                if identity not in self.approvals:
                    context = ('Patch (controls escaped):', '*** Begin Patch') if kind == 'patch' else (
                        'Working directory:', 'Shell:', 'Command (controls escaped):', 'unittest')
                    require(all(value in screen for value in context), 'approval context missing')
                    self.approvals[identity] = kind
                    self.capture('approval-' + str(len(self.approvals)))
                    self.drive('send', 'y')
            elif predicate(screen):
                return screen
            time.sleep(.1)
        raise RuntimeError('did not observe ' + label)

    def draft(self, text):
        self.drive('send', '--text', '\x1b[200~' + text + 'X\x1b[201~')
        self.expect(text + 'X')

    def edited(self, text):
        require(text + 'X' in self.capture('retained-draft'), 'background updates lost unsent draft')
        self.drive('send', 'BSpace')
        def edited():
            screen = self.capture('edited-draft')
            return text in screen and text + 'X' not in screen
        wait(edited, 'edited composer redraw')
        self.drive('send', 'Enter')

    def close(self):
        self.drive('send', 'C-c')
        def exited():
            state = command(['tmux', 'display-message', '-p', '-t', self.session,
                             '#{pane_dead} #{pane_dead_status}']).strip()
            require(not state.startswith('1 ') or state == '1 0', 'R TUI exited unsuccessfully')
            return state == '1 0'
        wait(exited, 'R TUI clean exit', seconds=15)
        self.drive('stop')
        self.session = None
