#!/usr/bin/env python3
"""Bounded Linux R event-loop responsiveness journey with a disposable loopback peer.

Real R TUI binary under a real PTY (grapt-tui-driver/tmux), fixture model only.
The native live journey resolves its provider through the named loopback seam, so
the product binary must be the testkit build; the TUI binary under test is ordinary.
Pins, on Linux:
  - idle spin guard: the whole TUI process stays within one percent of one core
    over a 30 s quiet window (the 2026-10-25 busy loop burned ~99 percent);
  - prompt input: each probed character echoes to the composer within the bound
    (a Linux regression guard, not the Windows 16 ms checklist cap);
  - burst smoke: an unpaced 400-line burst ends succeeded with its last line
    visible, and the whole window stays within five percent of one core
    aggregate CPU (a coalescing regression guard, not the rate-specific
    delivery cap, which the unpaced window cannot establish).
  - navigation: Esc opens the sessions picker and a second Esc closes it,
    returning a ready empty composer, each within a bounded wait;
Writes responsive-evidence.json. Raw PTY screens stay in scratch, out of git.
"""
import argparse
import json
import math
import os
import sqlite3
import subprocess
import sys
import time
from pathlib import Path

from acceptance_peer import Model, Server
from headless_acceptance import Journey, SESSION, require, wait
from tui_acceptance import command
from tui_acceptance_peer import TuiModel

ROOT = Path(__file__).resolve().parents[2]

IDLE_SECONDS = 30
IDLE_CPU_MAX = 1.0
ECHO_PROBES = 10
ECHO_P95_MAX_MS = 150.0
STREAM_LINES = 400
STREAM_CPU_MAX = 5.0
STREAM_WAIT_SECONDS = 90
SETTLE_SECONDS = 2
NAV_TIMEOUT_SECONDS = 5
PICKER_TITLE = 'Sessions \u00b7 Enter open \u00b7 Esc close'


def stream_frames(lines):
    """One SSE body carrying a burst of text deltas, the way the fixture does."""
    deltas = "".join(
        'data: ' + json.dumps({"type": "response.output_text.delta",
                               "delta": "line " + str(number) + "\n"}) + "\n\n"
        for number in lines)
    return "text/event-stream", (
        deltas
        + 'data: {"type":"response.output_text.done"}\n\n'
        + 'data: {"type":"response.completed","response":{"usage":'
        + '{"input_tokens":12,"output_tokens":8,"total_tokens":20}}}\n\n'
    ).encode()


class StreamModel(TuiModel):
    """Fixture model with one bounded streaming text action."""

    def __init__(self, lines):
        super().__init__()
        self.lines = lines

    def dispatch(self, path, headers, body):
        # Skip TuiModel.dispatch: it pops its own (action, identifier) entry,
        # while StreamModel queues bare action strings.
        Model.dispatch(self, path, headers, body)
        with self.lock:
            if not self.actions:
                raise ValueError("unexpected extra model execution")
            action = self.actions.popleft()
            self.completed.append((action, None))
        if action == "stream":
            return stream_frames(range(self.lines))
        raise ValueError("unknown StreamModel action: " + repr(action))


def verify_idle(evidence):
    phase = evidence['idle']
    require(phase['wall_seconds'] >= IDLE_SECONDS - 2.0,
            'idle window shorter than the admitted bound')
    require(phase['cpu_percent_one_core'] is not None
            and phase['cpu_percent_one_core'] <= IDLE_CPU_MAX,
            'idle CPU above the spin guard')


def verify_echo(evidence):
    latencies = evidence['echo']['latencies_ms']
    require(len(latencies) == ECHO_PROBES,
            'echo probe count differs from the admitted bound')
    ordered = sorted(latencies)
    rank = max(1, math.ceil(0.95 * len(ordered)))
    require(ordered[rank - 1] <= ECHO_P95_MAX_MS,
            'echo p95 above the Linux regression guard')


def verify_stream(evidence):
    phase = evidence['stream']
    require(phase['wall_seconds'] >= 2.0, 'streaming window is vacuous')
    require(phase['outcome'] == 'succeeded', 'stream execution did not succeed')
    require(phase['last_line_visible'], 'stream tail missing from the terminal')
    require(phase['cpu_percent_one_core'] is not None
            and phase['cpu_percent_one_core'] <= STREAM_CPU_MAX,
            'streaming window CPU above the burst-smoke guard')


def verify_evidence(evidence):
    require(evidence['fixture_errors'] == [], 'fixture provider recorded errors')
    require(evidence['model_completed'] == [['stream', None]],
            'model actions differ from the admitted journey')
    require(len(evidence['source']) == 40, 'source pin missing')
    verify_idle(evidence)
    verify_echo(evidence)
    verify_stream(evidence)


def proc_times(pid):
    """Return (total_jiffies, {thread_name: jiffies}) for a process tree."""
    def fields(path):
        raw = path.read_text()
        body = raw[raw.rfind(')') + 2:].split()
        return int(body[11]) + int(body[12])
    try:
        total = fields(Path(f'/proc/{pid}/stat'))
    except (OSError, IndexError, ValueError):
        return None
    task_dir = Path(f'/proc/{pid}/task')
    threads = {}
    for tid_path in sorted(task_dir.iterdir()):
        try:
            jiffies = fields(tid_path / 'stat')
            comm = (tid_path / 'comm').read_text().strip()
        except (OSError, IndexError, ValueError):
            continue
        threads[comm] = threads.get(comm, 0) + jiffies
    return total, threads


def find_tui_pid(tui_path, service_path):
    matches = []
    for entry in Path('/proc').iterdir():
        if not entry.name.isdigit():
            continue
        try:
            cmdline = (entry / 'cmdline').read_bytes().split(b'\0')
        except OSError:
            continue
        if cmdline and cmdline[0] == tui_path.encode() and service_path.encode() in cmdline:
            matches.append(int(entry.name))
    require(len(matches) == 1, 'expected one TUI pid, got ' + repr(matches))
    return matches[0]


def phase(first, started, last, tick):
    wall = time.monotonic() - started
    jiffies = last[0] - first[0]
    cpu = (jiffies / tick / wall * 100) if wall > 1 else None
    return {
        'wall_seconds': round(wall, 1),
        'cpu_percent_one_core': round(cpu, 4) if cpu is not None else None,
        'thread_cpu_seconds': {
            name: round(max(0, last[1].get(name, 0) - first[1].get(name, 0)) / tick, 3)
            for name in sorted(set(first[1]) | set(last[1]))},
    }


def composer_content(screen):
    """Exact trimmed composer text: '' when the composer is empty.

    The composer line is the box interior between the '│>' prompt and the
    right border, so both box characters are dropped before trimming.
    """
    for line in screen.splitlines():
        if line.startswith('\u2502>'):
            text = line[len('\u2502>'):]
            if text.endswith('\u2502'):
                text = text[:-1]
            return text.strip()
    raise RuntimeError('composer line missing from the terminal frame')


def evaluate_echo(before, contents, character):
    """Index of the first frame whose composer is exactly the probed character.

    The composer must be empty before the send, so a stale frame or a dropped
    input can never be accepted as the probe's echo.
    """
    if before != '':
        raise RuntimeError('composer not empty before the echo probe')
    for index, content in enumerate(contents):
        if content == character:
            return index
    raise RuntimeError('echo probe did not reach the composer')


def evaluate_clear(contents):
    """The composer must acknowledge the clearing key, not merely stop changing."""
    if contents and contents[-1] == '':
        return
    raise RuntimeError('echo probe did not clear the composer')


def probe_timing(started, echo_at, after_clear):
    """Echo latency ends at the echoed frame, before the clearing round trip."""
    if not (started <= echo_at <= after_clear):
        raise RuntimeError('probe timestamps are out of order')
    return (echo_at - started) * 1000.0


def echo_probe(driver, session, environment, character):
    contents = [composer_content(
        command([driver, 'capture', '--session', session], environment))]
    started = time.monotonic()
    command([driver, 'send', character, '--session', session], environment)
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        contents.append(composer_content(
            command([driver, 'capture', '--session', session], environment)))
        if contents[-1] == character:
            break
        time.sleep(0.02)
    evaluate_echo(contents[0], contents[1:], character)
    echo_at = time.monotonic()
    command([driver, 'send', 'BSpace', '--session', session], environment)
    clear = []
    deadline = time.monotonic() + 5
    while time.monotonic() < deadline:
        clear.append(composer_content(
            command([driver, 'capture', '--session', session], environment)))
        if clear[-1] == '':
            break
        time.sleep(0.02)
    evaluate_clear(clear)
    return probe_timing(started, echo_at, time.monotonic())


def navigation_probe(driver, session, environment):
    """Bounded Esc navigation: open the sessions picker, close it, composer ready."""
    started = time.monotonic()
    command([driver, 'send', 'Escape', '--session', session], environment)
    deadline = time.monotonic() + NAV_TIMEOUT_SECONDS
    screen = ''
    while time.monotonic() < deadline:
        screen = command([driver, 'capture', '--session', session], environment)
        if PICKER_TITLE in screen:
            break
        time.sleep(0.05)
    open_ms = (time.monotonic() - started) * 1000.0
    require(PICKER_TITLE in screen and 'Search:' in screen,
            'sessions picker never opened')
    started = time.monotonic()
    command([driver, 'send', 'Escape', '--session', session], environment)
    deadline = time.monotonic() + NAV_TIMEOUT_SECONDS
    while time.monotonic() < deadline:
        screen = command([driver, 'capture', '--session', session], environment)
        if PICKER_TITLE not in screen and composer_content(screen) == '':
            break
        time.sleep(0.05)
    close_ms = (time.monotonic() - started) * 1000.0
    require(PICKER_TITLE not in screen, 'sessions picker never closed')
    require(composer_content(screen) == '',
            'composer not ready after the picker closed')
    return {'open_ms': round(open_ms, 1), 'close_ms': round(close_ms, 1)}


def committed(journey):
    path = journey.profile / "database.sqlite3"
    with sqlite3.connect(path.as_uri() + "?mode=ro", uri=True, timeout=5) as connection:
        return [(kind, json.loads(data)) for kind, data in connection.execute(
            "SELECT type,data FROM event WHERE aggregate_id=? ORDER BY seq", (SESSION,))]


def run(args):
    args.cli, args.product, args.tui, args.driver = [args.binaries / name for name in
        ('grapt', 'grapt-server', 'grapt-tui', 'grapt-tui-driver')]
    require((args.binaries / 'source.txt').read_text().strip() == args.source, 'binary source differs')
    require('testkit/fixture' in command([args.product, '--version']),
            'native live journey needs the named loopback seam')
    model = StreamModel(STREAM_LINES)
    provider = Server(0, model.dispatch)
    journey, session = None, None
    evidence = dict(passed=False, source=args.source, frontend='R', platform='linux',
                    idle_seconds=IDLE_SECONDS, stream_lines=STREAM_LINES)
    with provider:
        try:
            journey = Journey(args, 'native-tui', provider.endpoint)
            profile_path = args.scratch / 'tui-profile.jsonl'
            journey.environment['GRAPT_TUI_R_PROFILE'] = str(profile_path)
            journey.start(0)
            require(journey.provision().returncode == 0, 'disposable provision failed')
            environment = ['PATH=' + os.environ['PATH'], 'TERM=xterm-256color',
                'GRAPT_TEST_HOME=' + journey.environment['GRAPT_TEST_HOME'],
                'GRAPT_CONFIG_DIR=' + journey.environment['GRAPT_CONFIG_DIR'],
                'GRAPT_TUI_R_PROFILE=' + str(profile_path)]
            session = command([args.driver, 'start', '--seat', 'seat-a', '--journey',
                f'r-responsive-{os.getpid()}', '--width', '140', '--height', '40',
                '--cwd', journey.profile, '--', '/usr/bin/env', '-i', *environment,
                args.tui, '--service', journey.profile, '--session', SESSION,
                '--directory', journey.profile], journey.environment).strip()
            command([args.driver, 'wait', '--text', 'Connected', '--timeout-ms', '20000',
                     '--session', session], journey.environment)
            deadline = time.monotonic() + 30
            screen = ''
            while time.monotonic() < deadline:
                screen = command([args.driver, 'capture', '--session', session],
                                 journey.environment)
                if 'enter send' in screen and 'default effort' in screen:
                    break
                time.sleep(1)
            require('enter send' in screen and 'default effort' in screen, 'composer never became ready')
            time.sleep(10)
            pid = find_tui_pid(str(args.tui), str(journey.profile))
            tick = os.sysconf('SC_CLK_TCK')

            # IDLE: force one repaint, then sample the quiet window.
            command([args.driver, 'send', 'Left', '--session', session], journey.environment)
            time.sleep(2)
            idle_first = proc_times(pid)
            require(idle_first, 'TUI process vanished before idle sampling')
            idle_started = time.monotonic()
            time.sleep(IDLE_SECONDS)
            idle_last = proc_times(pid)
            require(idle_last, 'TUI process vanished during idle sampling')
            evidence['idle'] = phase(idle_first, idle_started, idle_last, tick)

            # ECHO: each probed character must reach the composer, then clear.
            latencies = []
            for index in range(ECHO_PROBES):
                latencies.append(echo_probe(args.driver, session, journey.environment,
                                            'az'[(index + 1) % 2]))
            evidence['echo'] = {'latencies_ms': [round(value, 1) for value in latencies]}

            # NAVIGATION: Esc opens the sessions picker, Esc closes it.
            evidence['navigation'] = navigation_probe(args.driver, session,
                                                      journey.environment)

            # STREAM: one bounded burst; window runs from prompt send to settled success.
            model.expect('stream')
            stream_first = proc_times(pid)
            require(stream_first, 'TUI process vanished before streaming')
            stream_started = time.monotonic()
            prompt = 'Count from one to five, one word per line.'
            command([args.driver, 'send', '--text', '\x1b[200~' + prompt + '\x1b[201~',
                     '--session', session], journey.environment)
            command([args.driver, 'send', 'Enter', '--session', session], journey.environment)
            wait(lambda: any(kind == 'session.execution.succeeded.1' for kind, _ in committed(journey))
                 or any(kind in ('session.execution.failed.1', 'session.execution.interrupted.1')
                        for kind, _ in committed(journey)),
                 'stream execution terminal state', seconds=STREAM_WAIT_SECONDS)
            rows = committed(journey)
            outcome = ('succeeded' if any(kind == 'session.execution.succeeded.1' for kind, _ in rows)
                       else 'failed-or-interrupted')
            time.sleep(SETTLE_SECONDS)
            stream_last = proc_times(pid)
            require(stream_last, 'TUI process vanished during streaming')
            screen = command([args.driver, 'capture', '--session', session], journey.environment)
            (args.scratch / 'stream-end.screen.txt').write_text(screen, encoding='utf-8')
            last_line = 'line ' + str(STREAM_LINES - 1)
            evidence['stream'] = phase(stream_first, stream_started, stream_last, tick)
            evidence['stream']['outcome'] = outcome
            evidence['stream']['last_line_visible'] = last_line in screen
        finally:
            try:
                if session:
                    command([args.driver, 'send', 'C-c', '--session', session], journey.environment)
                    deadline = time.monotonic() + 15
                    while time.monotonic() < deadline:
                        state = command(['tmux', 'display-message', '-p', '-t', session,
                                         '#{pane_dead} #{pane_dead_status}'],
                                        journey.environment).strip()
                        if state == '1 0':
                            break
                        time.sleep(0.2)
                    command([args.driver, 'stop', '--session', session], journey.environment)
            except (OSError, RuntimeError):
                pass
            if journey:
                if journey.child and journey.child.poll() is None:
                    try:
                        journey.stop()
                    except (OSError, RuntimeError, subprocess.TimeoutExpired):
                        pass
                journey.cleanup()
    evidence['fixture_errors'] = provider.errors
    evidence['model_completed'] = [[action, identifier] for action, identifier in model.completed]
    frames = []
    profile_path = args.scratch / 'tui-profile.jsonl'
    if profile_path.exists():
        frames = [json.loads(line) for line in profile_path.read_text().splitlines()]
    evidence['frames_total'] = len(frames)
    evidence['frames'] = frames
    if all(name in evidence for name in ('idle', 'echo', 'navigation', 'stream')):
        verify_evidence(evidence)
        evidence['passed'] = True
    (args.scratch / 'responsive-evidence.json').write_text(json.dumps(evidence, indent=2))
    print(json.dumps({key: value for key, value in evidence.items() if key != 'frames'}, indent=2))


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
    subprocess_args = [
        'bwrap', '--die-with-parent', '--new-session', '--unshare-pid', '--unshare-net',
        '--ro-bind', '/', '/', '--tmpfs', '/home', '--tmpfs', '/tmp', '--tmpfs', '/var/tmp',
        '--proc', '/proc', '--dev', '/dev',
        '--bind', str(args.scratch), str(args.scratch),
        '--ro-bind', str(ROOT), str(ROOT), '--ro-bind', str(args.binaries), str(args.binaries),
        '--chdir', str(args.scratch), '/usr/bin/env', '-i', 'PATH=/usr/bin:/bin',
        'PYTHONDONTWRITEBYTECODE=1', f'HOME={args.scratch}/empty-home', sys.executable,
        str(Path(__file__)), '--inside', '--binaries', str(args.binaries),
        '--source', args.source, '--scratch', str(args.scratch)]
    subprocess.run(subprocess_args, check=True, timeout=540)


if __name__ == '__main__':
    main()
