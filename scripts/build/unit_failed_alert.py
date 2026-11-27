#!/usr/bin/env python3
"""Send the coordinator one alert for a failed SE Code unit.

systemd starts this template (secode-unit-failed@.service) only through the
OnFailure= lines of the SE Code timer-driven units, handing it the failed
invocation as MONITOR_* environment variables. The mail carries only what
systemd already observed: the unit, its result, its exit code or signal,
its invocation ID, and the exact command to read that invocation locally.
It never reads the journal and never queries systemctl, so an unavailable
journal, journal content, and a later healthy run cannot swallow the
alert; the fixed-size body always fits the transport. Every mail begins
with the fixed text 'secode-unit-failed:' so it stays filterable while the
alerts share a sender.

One trigger, one mail. If the unit is actually healthy, nothing is sent.
If the transport fails, the failure is written to this unit's own journal
and the exit code is 1; it never raises into the work it describes, and
it never sends a second mail.
"""
import argparse
import os
import re
import sys
import time

RUN_SECONDS = 120
BODY_MAX_BYTES = 1024


def _field(env, name, pattern):
    """One MONITOR_* variable systemd hands the OnFailure unit, bounded."""
    value = (env.get(name) or '').strip()
    if value and not re.fullmatch(pattern, value):
        raise ValueError(f'{name} is not a plain bounded value')
    return value


def _monitor(env):
    """What systemd observed about the failed invocation, verbatim and bounded."""
    return {
        'result': _field(env, 'MONITOR_SERVICE_RESULT', r'[a-z-]{1,32}'),
        'exit_code': _field(env, 'MONITOR_EXIT_CODE', r'[a-z-]{1,32}'),
        'exit_status': _field(env, 'MONITOR_EXIT_STATUS', r'[A-Za-z0-9_-]{1,16}'),
        'invocation': _field(env, 'MONITOR_INVOCATION_ID', r'[0-9a-f]{32}'),
    }


def compose(unit, monitor):
    """The one mail: unit, result, exit, invocation, and how to read it."""
    outcome = f'result={monitor["result"]}'
    if monitor['exit_code'] and monitor['exit_status']:
        outcome += f', {monitor["exit_code"]} {monitor["exit_status"]}'
    lines = [f'secode-unit-failed: SE Code unit {unit} failed: {outcome}.']
    if monitor['invocation']:
        lines.append(f'Invocation: {monitor["invocation"]}')
        lines.append(
            f'Read locally: journalctl _SYSTEMD_INVOCATION_ID={monitor["invocation"]}')
    body = '\n'.join(lines)
    if len(body.encode('utf-8')) > BODY_MAX_BYTES:
        raise ValueError('Unit failure alert body exceeds its bound')
    return body


def _registry(discover=None):
    """The coordinator, parsed defensively: any shape problem is a failure."""
    if discover is None:
        from nightly_revert import registry as discover
    reply = discover()
    if not isinstance(reply, tuple) or len(reply) != 2:
        raise ValueError('Registry reply has an unexpected shape')
    agents, coordinator = reply
    if (not isinstance(agents, list) or not isinstance(coordinator, str)
            or not re.fullmatch(r'agt-[a-z0-9-]{3,80}', coordinator)):
        raise ValueError('Registry reply has an unexpected shape')
    if not any(isinstance(row, dict) and row.get('agent_id') == coordinator
               for row in agents):
        raise ValueError('Registry reply names no coordinator')
    return coordinator


def deliver(unit, agent_id, env=None, send=None, registry=None):
    """One mail for one failed invocation.

    The monitor state comes only from the environment systemd sets for the
    OnFailure unit; no live state is re-read. Returns the sent body, or
    None when the unit is healthy. Any transport failure raises; main
    reports it to this unit's own journal and exits 1.
    """
    if env is None:
        env = os.environ
    if not re.fullmatch(r'[A-Za-z0-9:._@-]{1,256}', unit):
        raise ValueError('Unit failure alert requires a plain bounded unit name')
    monitor = _monitor(env)
    if monitor['result'] in ('', 'success'):
        return None
    body = compose(unit, monitor)
    if send is None:
        from nightly_revert import send as send
    send(agent_id, _registry(registry), body, timeout=20)
    return body


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--unit', required=True,
                        help='the failed unit name (the template instance)')
    parser.add_argument('--agent-id', required=True,
                        help="the alert's own registered StateEngine sender")
    args = parser.parse_args()
    started = time.monotonic()
    try:
        body = deliver(args.unit, args.agent_id)
        if body is not None:
            print(body, file=sys.stderr, flush=True)
    except Exception as error:  # noqa: BLE001 - telemetry must never fail work
        print(f'Unit failure alert could not be sent: {type(error).__name__}: {error}',
              file=sys.stderr, flush=True)
        return 1
    finally:
        if time.monotonic() - started > RUN_SECONDS:
            print('Unit failure alert exceeded its bound', file=sys.stderr)
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
