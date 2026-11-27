"""Continue the sustained coding task through repeated compaction and process loss."""
import json
import threading

from acceptance_peer import Model
from headless_acceptance import require
from sustained_provider import SustainedModel, calls, text, output, test_command

LOST = 'SUSTAIN_LOST: record one task effect and wait for its result.'
RECOVER = 'SUSTAIN_RECOVER: inspect the existing effect, preserve both operations and test the continued task.'
REPEATED_SUMMARY = """## Objective
- SUSTAIN_TASK fixed signed addition; SUSTAIN_RESTART added subtraction without losing it.
## Important Details
- calc.add returns a + b; calc.subtract returns a - b. Three unit tests pass with OK.
- SUSTAIN_TOOL_DONE: real background tests completed through their owning handles.
## Work State
### Completed
- Subtraction added and tested. Signed addition remains verified.
- SUSTAIN_QUEUED wrote worklog.txt once; that input is complete and must not run again.
### Active
- Check the verified operations after the current automatic compaction.
### Blocked
- none
## Next Move
1. Run the continued task's tests, then wait for explicit further input.
2. Preserve completed patches and tool outcomes; do not repeat them.
## Relevant Files
- calc.py, test_calc.py, worklog.txt and evidence-*.txt belong to this fixture.
"""

class ReliabilityModel(SustainedModel):
    def __init__(self):
        super().__init__()
        self.round = 0
        self.phase = None
        self.restart_entered, self.restart_release = threading.Event(), threading.Event()

    def read_again(self):
        self.phase = 'read'
        return calls([(f'call_repeat_read_{self.round}_{n}', 'read', {'path':f'evidence-{n}.txt'}) for n in range(8)])

    def dispatch_continued(self, body):
        if self.stage == 10:
            return self.read_again()
        return super().dispatch_continued(body)

    def dispatch(self, path, headers, body):
        encoded = json.dumps(body)
        summary = 'Output exactly the Markdown structure shown inside <template>' in encoded
        if summary:
            reply = super().dispatch(path, headers, body)
            if self.stage == 10:
                return text(REPEATED_SUMMARY)
            return reply
        if self.stage == 7:
            self.restart_entered.set()
            require(self.restart_release.wait(timeout=30), 'uncertain input was not explicitly retried')
        if self.stage < 10:
            return super().dispatch(path, headers, body)
        Model.dispatch(self, path, headers, body)
        if self.stage == 10:
            if self.phase == 'read':
                require(self.summaries == self.round + 2 and 'Model-written continuation summary' in encoded,
                    'repeated tool reads did not trigger another automatic compaction')
                require('Subtraction added' in encoded and 'SUSTAIN_TASK' in encoded,
                    'repeat compaction lost the current objective or tested changes')
                self.phase = 'test'
                return calls([(f'call_repeat_test_{self.round}', 'exec_command', {
                    'cmd':test_command(), 'yield_time_ms':10000, 'max_output_tokens':1000})])
            require(self.phase == 'test', 'unexpected repeat stage')
            result = output(body, f'call_repeat_test_{self.round}')
            require(result.get('done') is True and result.get('exit_code') == 0 and 'OK' in result.get('output',''),
                'useful tests did not complete after automatic compaction')
            self.round += 1
            if self.round < 2:
                return self.read_again()
            self.stage = 11
            return text('SUSTAIN_RESTART_DONE')
        if self.stage == 11:
            require(LOST in encoded and 'Subtraction added' in encoded, 'continued crash task lost its objective')
            self.stage = 12
            return calls([('call_sustain_lost_parent', 'execute', {'code':"return await tools['nested_hold']({});"})])
        if self.stage == 12:
            notices = [row.get('content','') for row in body.get('input',[]) if row.get('role')=='user' and isinstance(row.get('content'),str)]
            require(RECOVER in encoded and any(all(part in notice for part in ('Tool nested_hold',
                'run inside call_sustain_lost_parent', 'ended with an unknown effect', 'do not repeat it blindly')) for notice in notices),
                'recovery lost the original nested effect identity or its warning')
            self.stage = 13
            return calls([('call_sustain_recovery_tests', 'exec_command', {
                'cmd':test_command(), 'yield_time_ms':10000, 'max_output_tokens':1000})])
        if self.stage == 13:
            result = output(body, 'call_sustain_recovery_tests')
            require(result.get('done') is True and result.get('exit_code')==0 and 'OK' in result.get('output',''),
                'continued tests failed after crash reconciliation')
            self.stage = 14
            return text('SUSTAIN_RECOVERY_DONE')
        raise ValueError('unexpected replay or extra provider request')
