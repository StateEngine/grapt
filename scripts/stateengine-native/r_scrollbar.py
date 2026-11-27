#!/usr/bin/env python3
"""Scrollbar and navigation controls preserve R's transcript viewport and draft.

Uses ordinary binaries, a disposable profile and isolated TLS/MCP peers.
"""
import argparse
import json
import re
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import SESSION, require, write
from ordinary_profile import OrdinaryProfile
from r_sustained_terminal import RTerminal
from sustained_provider import text
from sustained_walk import committed, idle
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]


class WheelModel(Model):
    def dispatch(self, path, headers, body):
        super().dispatch(path, headers, body)
        if self.count() == 1:
            return text('\n\n'.join(f'BAR_ROW_{i:03d}' for i in range(240)) + '\n\nBAR_TAIL_DONE')
        return text('\n\n'.join(f'NEW_OUTPUT_{i:03d}' for i in range(60)) + '\n\nNEW_OUTPUT_COMPLETE')


def pane(screen):
    return screen.splitlines()[2:-5]


def thumb(screen):
    return [i for i, row in enumerate(pane(screen)) if row.endswith('█')]


def markers(screen):
    return re.findall(r'BAR_ROW_\d{3}', '\n'.join(pane(screen)))


def exercise(terminal, journey, model):
    require(not thumb(terminal.drive('capture')), 'fitting content showed a scrollbar')
    terminal.capture('no-overflow')
    terminal.send('BAR_PROMPT_FIRST')
    terminal.expect('BAR_TAIL_DONE')
    idle(journey)
    live = terminal.capture('overflow-bottom')
    require(thumb(live) and thumb(live)[-1] == len(pane(live)) - 1, 'live thumb is not at bottom')
    terminal.drive('send', '--text', '\x1b[<64;4;10M' * 8)
    up = terminal.until(lambda s: 'End live' in s and 'BAR_TAIL_DONE' not in s, 'detached viewport')
    require(thumb(up) and thumb(up)[-1] < len(pane(up)) - 1, 'detached thumb did not move')
    terminal.drive('send', '--text', '\x1b[200~KEEP_SCROLL_DRAFT\x1b[201~')
    before = terminal.capture('scrolled-up')
    old = markers(before)
    require(old, 'no old rows in detached viewport')
    journey.api('POST', f'/api/session/{SESSION}/prompt', {
        'id': 'msg_scrollbar_new_output', 'text': 'append new output while I inspect history',
        'delivery': 'queue', 'resume': True})
    idle(journey)
    after = terminal.until(lambda s: 'New output below' in s, 'new output indication')
    require(markers(after) == old and 'NEW_OUTPUT_COMPLETE' not in after, 'new output yanked old rows')
    require('KEEP_SCROLL_DRAFT' in after, 'new output lost draft')
    terminal.capture('new-output-keeps-viewport')
    terminal.drive('send', 'End')
    terminal.expect('NEW_OUTPUT_COMPLETE')
    bottom = terminal.capture('end-live')
    require(thumb(bottom) and thumb(bottom)[-1] == len(pane(bottom)) - 1, 'End thumb is not at bottom')
    require('KEEP_SCROLL_DRAFT' in bottom, 'End lost draft')
    terminal.drive('send', 'C-Home')
    terminal.expect('BAR_PROMPT_FIRST')
    top = terminal.capture('first-history')
    require(thumb(top) and thumb(top)[0] == 0, 'first-history thumb is not at top')
    before_down = terminal.capture('page-down-before')
    first = markers(before_down)
    require(first, 'first history window has no row markers')
    require('KEEP_SCROLL_DRAFT' in before_down, 'Ctrl+Home lost draft')
    terminal.drive('send', 'PageDown')
    terminal.until(lambda s: markers(s) and markers(s)[0] > first[0], 'PageDown moved forward')
    after_down = terminal.capture('page-down-after')
    down = markers(after_down)
    require(down and down[0] > first[0] and down[-1] > first[-1],
            'PageDown did not move the history window forward')
    require('KEEP_SCROLL_DRAFT' in after_down, 'PageDown lost draft')
    before_up = terminal.capture('page-up-before')
    require(markers(before_up) == down, 'history window changed before PageUp')
    terminal.drive('send', 'PageUp')
    terminal.until(lambda s: markers(s) == first, 'PageUp returned to first history window')
    after_up = terminal.capture('page-up-after')
    require(markers(after_up)[0] < down[0] and pane(after_up) == pane(before_down),
            'PageUp did not restore the initial history window')
    require('KEEP_SCROLL_DRAFT' in after_up, 'PageUp lost draft')
    terminal.drive('send', 'C-End')
    terminal.expect('NEW_OUTPUT_COMPLETE')
    require(model.count() == 2, 'navigation submitted an unexpected prompt')
    inputs = [v for k, v in committed(journey) if k == 'session.input.admitted.1']
    require(len(inputs) == 2, 'unexpected input admission')
    terminal.drive('send', 'C-a', 'C-k')
    terminal.send('/help')
    terminal.expect('Ctrl+Home')
    terminal.expect('End/Ctrl+End')
    terminal.capture('navigation-help')


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source,
            'frontend source differs')
    backend = args.binaries / 'backend-source.txt'
    result = dict(passed=False, paid_calls=0, frontend_source=args.source,
                  backend_source=backend.read_text().strip() if backend.exists() else args.source)
    model, mail = WheelModel(), TuiMail()
    provider = Server(0, model.dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    journey, terminal, success = None, None, False
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            journey.start(0)
            require(journey.provision().returncode == 0, 'provision failed')
            terminal = RTerminal(args, journey)
            terminal.start(0)
            exercise(terminal, journey, model)
            require(not provider.errors and not peer.errors, 'fixture peer failed')
            terminal.close()
            journey.stop()
            success = True
        finally:
            errors = []
            if terminal:
                if terminal.session:
                    try:
                        terminal.capture('failure')
                    except (OSError, RuntimeError):
                        pass
                try:
                    terminal.cleanup()
                except (OSError, RuntimeError) as error:
                    errors.append(f'terminal cleanup: {error}')
            if journey:
                try:
                    journey.cleanup()
                except (OSError, RuntimeError) as error:
                    errors.append(f'profile cleanup: {error}')
            result.update(passed=success and not errors, cleanup_errors=errors,
                          scrollbar_overflow=success, viewport_pinned=success, draft_preserved=success,
                          page_keys=success, end_live=success, navigation_help=success)
            write(args.scratch / 'result.json', result)
            if errors:
                raise RuntimeError('; '.join(errors))
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
    args.binaries = args.binaries.resolve(strict=True)
    args.scratch = args.scratch.resolve()
    require(args.scratch != ROOT and ROOT not in args.scratch.parents, 'scratch must be outside source')
    args.scratch.mkdir(mode=0o700)
    subprocess.run([sys.executable, str(ROOT / 'scripts/native-tui/fixture-ca.py'),
                    '--root', str(args.scratch / 'tls')], check=True, timeout=30)
    subprocess.run(['bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev', '--bind', str(args.scratch), str(args.scratch),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries), '--source', args.source,
        '--scratch', str(args.scratch)], check=True, timeout=180)


if __name__ == '__main__':
    main()
