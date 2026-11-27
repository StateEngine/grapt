#!/usr/bin/env python3
"""The visible inherited effort matches the ordinary provider request."""
import argparse
import json
import os
from pathlib import Path
import ssl
import subprocess
import sys

from acceptance_peer import Model, Server
from headless_acceptance import SESSION, require, write
from ordinary_profile import OrdinaryProfile
from r_launch_walk import Terminal
from sustained_walk import idle
from sustained_provider import text
from tui_acceptance import command
from tui_acceptance_peer import TuiMail

ROOT = Path(__file__).resolve().parents[2]


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' not in command([args.product, '--version']), 'ordinary product required')
    model, mail, journey, terminal = Model(), TuiMail(), None, None
    requests = []
    def dispatch(path, headers, body):
        expected = ['high', 'low', 'high', 'high']
        require(len(requests) < len(expected), 'unexpected provider request')
        require(body.get('reasoning', {}).get('effort') == expected[len(requests)], 'provider effort differs from selection')
        requests.append(body)
        model.dispatch(path, headers, body)
        return text('EFFORT_DONE_' + str(len(requests)))
    provider = Server(0, dispatch)
    tls = ssl.SSLContext(ssl.PROTOCOL_TLS_SERVER)
    tls.load_cert_chain(args.scratch / 'tls/server.crt', args.scratch / 'tls/server.key')
    provider.socket = tls.wrap_socket(provider.socket, server_side=True)
    result = dict(passed=False, source=args.source, ordinary_binaries=True, paid_calls=0)
    with provider, Server(4877, mail.dispatch, with_connection=True) as peer:
        try:
            journey = OrdinaryProfile(args, f'https://127.0.0.1:{provider.server_port}')
            config_path = journey.profile / 'grapt.jsonc'
            config = json.loads(config_path.read_text())
            # Issuance admits offered discovery levels, not an invented config row.
            listing_path = journey.profile / 'account-models.json'
            listing = json.loads(listing_path.read_text())
            listing['data'][0].update(supported_reasoning_levels=[{'effort': 'low'}, {'effort': 'high'}],
                                      default_reasoning_level='low')
            write(listing_path, listing)
            config['provider']['selected_variant'] = 'high'
            write(config_path, config)
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            before = journey.api('GET', f'/api/session/{SESSION}')['data']['model']
            require(before.get('variant') in (None, 'default'), 'fixture has an explicit effort')
            terminal = Terminal(args, journey)
            terminal.start(0, text=None)
            terminal.expect('high (default)')
            def prompt(value, count):
                terminal.send(value)
                terminal.expect('EFFORT_DONE_' + str(count))
                idle(journey)

            def local(value, expected, capture):
                before = len(requests)
                terminal.send(value)
                terminal.expect(expected)
                terminal.capture(capture)
                require(len(requests) == before, value + ' invoked the provider')

            def clear():
                terminal.drive('send', 'C-a', 'C-k')

            def close(marker):
                terminal.drive('send', 'Escape')
                terminal.until(lambda screen: marker not in screen, 'closed ' + marker)

            prompt('Show the inherited reasoning effort.', 1)
            terminal.capture('configured-inherited-effort')
            require(journey.api('GET', f'/api/session/{SESSION}')['data']['model'] == before,
                    'inherited default changed saved selection')
            local('/model', 'Models and effort', 'model-picker')
            close('Models and effort')
            local('/effort', 'Models and effort', 'effort-picker')
            close('Models and effort')
            local('/effort low', '· low', 'effort-low')
            low = journey.api('GET', f'/api/session/{SESSION}')['data']['model']
            require(low.get('variant') == 'low', 'effort selection was not saved')
            prompt('Use the lower effort for this answer.', 2)
            local('/effort status', 'Effort: low', 'effort-status')
            close('Command result')
            for value, message in [
                    ('/effort imaginary', 'Effort not offered'),
                    ('/effort high unexpected', 'Use /effort'),
                    ('/model nowhere/missing', 'Model or effort not offered'),
                    ('/model openai/gpt-live high unexpected', 'Use /model')]:
                local(value, message, 'refusal-' + value.split()[0].lstrip('/'))
                require(journey.api('GET', f'/api/session/{SESSION}')['data']['model'] == low,
                        'invalid command changed saved model')
                clear()
            local('/model openai/gpt-live high', '· high', 'inline-model-high')
            require(journey.api('GET', f'/api/session/{SESSION}')['data']['model'].get('variant') == 'high',
                    'inline model effort was not saved')
            prompt('Use the explicitly selected higher effort.', 3)
            local('/effort default', 'high (default)', 'effort-default')
            prompt('Use the inherited reasoning effort again.', 4)
            after = journey.api('GET', f'/api/session/{SESSION}')['data']['model']
            require(after == before and len(requests) == 4, 'default restoration or request count differs')
            terminal.close()
            require(not provider.errors and not peer.errors, 'fixture provider or MCP error')
            result.update(passed=True, header_matches_provider=True, saved_choice_restored=True, inline_selection_durable=True,
                          model_and_effort_argument_refusals=True, provider_requests=len(requests))
        finally:
            if terminal:
                if terminal.session:
                    try: terminal.capture('failure')
                    except (OSError, RuntimeError): pass
                terminal.cleanup()
            if journey:
                if journey.child and journey.child.poll() is None:
                    try: journey.stop()
                    except (OSError, RuntimeError, subprocess.TimeoutExpired): pass
                journey.cleanup()
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
        '--scratch', str(args.scratch)], check=True, timeout=180)


if __name__ == '__main__':
    main()
