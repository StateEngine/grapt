"""N2 composer work through the shared native Windows ConPTY carrier."""
import json

from acceptance_peer import Server
from headless_acceptance import SESSION, require, wait, write
from ordinary_profile import OrdinaryProfile
from r_windows_terminal import WindowsTerminal
from r_windows_reliability import observe
from sustained_provider import SustainedModel, TASK, QUEUED, RESTART
from sustained_walk import committed, idle


def verify_inputs(rows):
    admissions = [value for kind, value in rows if kind == 'session.input.admitted.1']
    require(len(admissions) == 3 and len({row['inputID'] for row in admissions}) == 3,
            'N2 lost or replayed an input')
    require([row['input']['data']['text'] for row in admissions] == [TASK, QUEUED, RESTART],
            'N2 admitted different text from the edited composer')
    require(all(row['sessionID'] == SESSION and row['input']['delivery'] == 'queue'
                for row in admissions), 'N2 used a different Session or delivery route')


def hard_kill(journey, report):
    child = journey.child
    require(child is not None and child.poll() is None, 'owned service is not running')
    report.record('process hard kill', {'pid': child.pid, 'operation': 'Popen.kill/TerminateProcess'})
    child.kill()
    child.wait(timeout=10)
    child.stdin.close()
    journey.child = None


def run(args, report):
    model = SustainedModel()
    journey = terminal = None
    with Server(0, model.dispatch) as provider:
        try:
            journey = OrdinaryProfile(args, provider.endpoint)
            observe(args, journey)
            path = journey.profile / 'grapt.jsonc'
            config = json.loads(path.read_text())
            config.pop('mcp', None)
            config['permission'].update(shell='ask', edit='ask', patch='ask')
            write(path, config)
            (journey.profile / 'calc.py').write_text('def add(a, b):\n    return abs(a) + abs(b)\n')
            (journey.profile / 'test_calc.py').write_text(
                'import unittest\nimport calc\nclass Calc(unittest.TestCase):\n'
                '    def test_signed(self): self.assertEqual(calc.add(-2, 5), 3)\n'
                '    def test_zero(self): self.assertEqual(calc.add(0, 0), 0)\n')
            for n in range(8):
                (journey.profile / f'evidence-{n}.txt').write_text(
                    f'evidence-{n}: signed addition task\n' + ('x' * 79 + '\n') * 450)
            journey.start(0)
            require(journey.provision().returncode == 0, 'private Session creation failed')
            terminal = WindowsTerminal(args, journey)
            terminal.start(0)
            report.passed('native ConPTY and isolated fixture Session')
            if args.smoke:
                terminal.close()
                journey.stop()
                return
            terminal.send(TASK)
            terminal.until(lambda screen: 'SUSTAIN_TOOL_BEGIN' in screen, 'background output')
            terminal.capture('background-tool-output')
            terminal.draft(QUEUED)
            terminal.until(lambda _: model.summary_entered.is_set(), 'automatic compaction')
            require(QUEUED not in json.dumps(model.bodies), 'unsent draft reached provider')
            terminal.capture('compaction-and-unsent-draft')
            terminal.edited(QUEUED)
            wait(lambda: sum(kind == 'session.input.admitted.1' for kind, _ in committed(journey)) == 2,
                 'edited input queued during compaction')
            model.summary_release.set()
            terminal.until(lambda screen: 'SUSTAIN_QUEUED_DONE' in screen, 'queued work completion')
            idle(journey)
            require((journey.profile / 'worklog.txt').read_text() ==
                    'addition verified; queued input completed once\n', 'queued effect missing')
            compactions = [value for kind, value in committed(journey)
                           if kind == 'session.compaction.ended.2']
            require(compactions and all(row.get('trigger') == 'policyThreshold' for row in compactions),
                    'no committed automatic policy compaction')
            report.passed('background output, Once approvals, edited queue and automatic compaction')
            history, original_pid, endpoint = journey.history(), terminal.session, journey.endpoint
            terminal.draft(RESTART)
            hard_kill(journey, report)
            terminal.until(lambda screen: 'Service' in screen and 'Connected' not in screen
                           and RESTART + 'X' in screen, 'offline notice and retained draft')
            journey.start(1)
            require(journey.endpoint != endpoint, 'service endpoint did not change after hard kill')
            require(journey.history() == history, 'hard-kill restart changed committed history')
            terminal.until(lambda screen: 'Connected' in screen and RESTART + 'X' in screen,
                           'original R reconnect and retained draft')
            require(terminal.session == original_pid, 'restart replaced the R process')
            require(sum(kind == 'session.input.admitted.1' for kind, _ in committed(journey)) == 2,
                    'reattach replayed input or submitted the unsent draft')
            terminal.capture('same-r-after-hard-kill')
            report.passed('hard-kill restart reattaches original R with unsent draft intact')
            terminal.edited(RESTART)
            terminal.until(lambda screen: 'SUSTAIN_RESTART_DONE' in screen, 'post-crash coding work')
            idle(journey)
            rows = committed(journey)
            verify_inputs(rows)
            asks = [value for kind, value in rows if kind == 'permission.asked.2']
            replies = [value for kind, value in rows if kind == 'permission.replied.2']
            require([row['action'] for row in asks] == ['patch', 'shell', 'patch', 'patch', 'shell'],
                    'unexpected or missing N2 tool approval')
            require([row['id'] for row in asks] == list(terminal.approvals) ==
                    [row['requestID'] for row in replies] and
                    all(row['reply'] == 'once' for row in replies), 'approval identity or reply lost')
            require(model.stage == 11 and len(set(model.handles)) == 2 and not provider.errors,
                    'background continuation or fixture transport failed')
            require(not any(kind in ('session.execution.failed.1', 'session.execution.interrupted.1')
                            for kind, _ in rows), 'N2 producer recorded a failure')
            terminal.capture('post-crash-tested-work')
            terminal.close()
            journey.stop()
            report.passed('post-crash tested work and exact input, approval and background identities')
        finally:
            model.summary_release.set()
            if terminal:
                terminal.cleanup()
            if journey:
                journey.cleanup()
