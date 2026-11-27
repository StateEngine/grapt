"""Bounded nightly termination records and systemd's final exit observation."""
from operator_paths import data_root
import argparse
import json
import os
from pathlib import Path
import signal


def tail(path):
    try:
        with path.open('rb') as stream:
            stream.seek(0, 2)
            stream.seek(max(0, stream.tell()-4096))
            return '\n'.join(stream.read(4096).decode('utf-8', errors='replace').splitlines()[-20:])
    except OSError as error:
        return f'{type(error).__name__}: {error}'


def record(state, error, *, commit=None, returncode=None, requested_signal=None, logs=()):
    detail = str(error).strip()[:2048] or type(error).__name__
    signum = -returncode if isinstance(returncode, int) and returncode < 0 else None
    value = dict(exception_type=type(error).__name__, message=detail,
                 commit=commit, exit_status=returncode if returncode is not None and returncode >= 0 else None,
                 signal=signal.Signals(signum).name if signum is not None else None,
                 requested_signal=signal.Signals(requested_signal).name if requested_signal else None,
                 stderr_tail=[dict(path=str(path), text=tail(path)) for path in logs])
    state.mkdir(parents=True, exist_ok=True)
    temporary = state/'last-failure.json.new'
    temporary.write_text(json.dumps(value, indent=2)+'\n')
    temporary.replace(state/'last-failure.json')
    return describe(value)


def describe(value):
    return (f"{value['exception_type']}: {value['message']}; exit_status={value.get('exit_status')}; "
            f"signal={value.get('signal')}; requested_signal={value.get('requested_signal')}")


def service_exit(state, environment):
    """ExecStopPost survives a killed main process and its process group."""
    result = environment.get('SERVICE_RESULT', 'unknown')
    if result == 'success':
        return
    code = environment.get('EXIT_CODE', 'unavailable')
    status = environment.get('EXIT_STATUS', 'unavailable')
    reason = f'systemd result={result}; exit_code={code}; exit_status={status}'
    try:
        path = state/'current-attempt.json'
        if path.stat().st_size > 4096:
            raise ValueError('Current nightly attempt exceeds 4 KiB')
        attempt = json.loads(path.read_text())
        logs = [Path(item) for item in attempt['logs'][:2]]
        commit = attempt['commit']
    except (OSError, ValueError, KeyError, TypeError) as error:
        logs = []
        commit = None
        reason += f'; attempt unavailable: {type(error).__name__}: {error}'
    previous = state/'last-failure.json'
    try:
        if previous.stat().st_size > 65536:
            raise ValueError('Failure record exceeds 64 KiB')
        value = json.loads(previous.read_text())
        if not isinstance(value, dict):
            raise ValueError('Failure record is not an object')
        if code != 'exited' or value.get('commit') != commit:
            raise ValueError('Failure record belongs to another attempt')
    except (OSError, ValueError, TypeError):
        returncode = int(status) if code == 'exited' and status.isdigit() else None
        if code in ('killed', 'dumped'):
            name = status if status.startswith('SIG') else 'SIG'+status
            try:
                returncode = -signal.Signals[name].value
            except KeyError:
                pass
        record(state, RuntimeError(reason), commit=commit, returncode=returncode, logs=logs)
        value = json.loads(previous.read_text())
    value.update(service_result=result, service_exit_code=code, service_exit_status=status)
    temporary = state/'last-failure.json.new'
    temporary.write_text(json.dumps(value, indent=2)+'\n')
    temporary.replace(previous)
    detail = describe(value)+'; '+reason
    # Preserve a specific worker diagnosis on an ordinary handled failure.
    if code != 'exited' or not (state/'last-result').is_file() or 'RUNNING' in (state/'last-result').read_text()[:4096]:
        temporary = state/'last-result.new'
        temporary.write_text(f'INCONCLUSIVE main {commit}: {detail}\n')
        temporary.replace(state/'last-result')
    return detail


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--state', type=Path, default=data_root()/'logs/nightly')
    arguments = parser.parse_args()
    outcome = service_exit(arguments.state, os.environ)
    if outcome:
        print(outcome, flush=True)
