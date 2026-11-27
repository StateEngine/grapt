"""Fault phases within the existing ordinary R sustained task, using its owners."""
import json
import re
from urllib.parse import urlsplit

from headless_acceptance import SESSION, require, wait, write
from launch_proxy import Proxy
from r_reliability_provider import LOST, RECOVER
from sustained_provider import RESTART
from sustained_walk import committed, idle


def checkpoint(journey, name):
    observer = getattr(journey.args, 'checkpoint', None)
    if observer:
        observer(name)

class RecoveryProxy(Proxy):
    max_requests = 512
    def __init__(self, endpoint):
        self.offline = False
        self.readiness = None
        super().__init__(endpoint)
    def faults(self, method, path):
        return self.offline, False

def point(journey, proxy):
    target = urlsplit(journey.endpoint)
    proxy.target = (target.hostname, target.port)
    receipt = journey.profile/'serve-ready.json'
    proxy.readiness = json.loads(receipt.read_text())
    write(receipt, dict(proxy.readiness, observed_endpoint=proxy.endpoint))
    proxy.offline = False

def retire(journey, proxy):
    if proxy:
        proxy.offline = True
    if proxy and proxy.readiness:
        write(journey.profile/'serve-ready.json', proxy.readiness)
    journey.stop()

def admissions(journey):
    return [data for kind,data in committed(journey) if kind=='session.input.admitted.1']

def prompt_writes(proxy):
    return [path for method,path in proxy.requests if method=='POST' and path.endswith('/prompt')]

def approval_disconnect(terminal, journey, proxy):
    terminal.expect('[y] Once')
    screen = terminal.capture('pending-approval-before-disconnect')
    match = re.search(r'Request: (per_[a-zA-Z0-9]+)', screen)
    require(match and 'Patch (controls escaped):' in screen and '*** Begin Patch' in screen,
        'approval identity or patch context missing')
    identity = match[1]
    route = f'/api/session/{SESSION}/permission/{identity}/reply'
    proxy.offline = True
    terminal.drive('send', 'y')
    terminal.expect('503')
    require(proxy.requests.count(('POST', route)) == 1 and not any(
        kind == 'permission.replied.2' for kind, _ in committed(journey)),
        'disconnected approval was replayed or settled without the server')
    require('return abs(a) + abs(b)' in (journey.profile/'calc.py').read_text(),
        'unconfirmed approval ran the patch')
    proxy.offline = False
    terminal.expect(identity)
    terminal.drive('send', 'y')
    wait(lambda:any(kind == 'permission.replied.2' and data['requestID'] == identity
        for kind, data in committed(journey)), 'explicit retry of the pending approval')
    require(proxy.requests.count(('POST', route)) == 2, 'approval retry used another request')
    terminal.approvals[identity] = 'patch'

def verify_recovery(rows):
    starts = [data for kind, data in rows if kind == 'session.tool.nested.started.1']
    terminals = [data for kind, data in rows if kind == 'session.tool.terminal.1'
        and data.get('outcome', {}).get('reason') == 'effect-unknown']
    require(len(starts) == len(terminals) == 1
        and starts[0]['call'] == terminals[0]['call']
        and starts[0]['attempt'] == terminals[0]['attempt']
        and starts[0]['parent'] == 'call_sustain_lost_parent',
        'lost effect settled under another identity or more than once')
    failed = [data for kind, data in rows if kind.startswith('session.step.failed.')]
    require(len(failed) == 1 and failed[0]['reason']['kind'] == 'process-loss',
        'unexpected failure beyond the injected process loss')

def uncertain_restart(terminal, journey, proxy, model):
    proxy.lose_write, proxy.hide_history = True, True
    terminal.edited(RESTART)
    terminal.expect('Alt+R')
    wait(model.restart_entered.is_set, 'continued task held for uncertain input')
    wait(lambda:len(admissions(journey))==3, 'original uncertain input admission')
    require(RESTART in terminal.capture('uncertain-restart-draft'), 'uncertain response cleared the draft')
    require(len(prompt_writes(proxy))==3, 'an uncertain input was retried automatically')
    terminal.drive('send', '--text', '\x1br')
    wait(lambda:len(prompt_writes(proxy))==4, 'explicit retry of original input')
    proxy.hide_history = False
    model.restart_release.set()
    require(len(admissions(journey))==3, 'explicit retry admitted the input twice')

def crash_phase(terminal, journey, proxy, model):
    (journey.profile/'test_effect.py').write_text(
        "import unittest\nfrom pathlib import Path\nclass Effect(unittest.TestCase):\n"
        "    def test_effect_once(self):\n"
        "        self.assertEqual(Path('effect.txt').read_text(), 'effect recorded once\\n')\n"
        "        Path('continued.txt').write_text('verified existing effect\\n')\n")
    terminal.send(LOST)
    wait(lambda:(journey.profile/'entered').exists(), 'real nested effect before process loss',seconds=30)
    wait(lambda:any(line.strip('│ ').strip()=='>' for line in terminal.drive('capture').splitlines()), 'lost task admission clears composer')
    terminal.draft(RECOVER)
    form='frm_sustain_recovery'
    base=f'/api/session/{SESSION}/form'
    answer='keep tested signed addition and subtraction'
    journey.api('POST',base,dict(id=form,title='Keep the verified operations',fields=[
        dict(key='objective',type='string',required=True)]))
    terminal.expect('Keep the verified operations')
    terminal.drive('send','--text',answer)
    terminal.expect(answer)
    terminal.capture('pending-question-before-crash')
    calls_before=model.count()
    pane=terminal.session
    proxy.offline=True
    write(journey.profile/'serve-ready.json', proxy.readiness)
    journey.child.kill(); journey.child.wait(timeout=5); journey.child=None
    terminal.expect('Service reconnecting')
    journey.start(2)
    journey.rebind()
    point(journey,proxy)
    terminal.until(lambda screen:'Connected' in screen and answer in screen,'original R and question draft after crash')
    require(terminal.session==pane, 'crash replaced the R process')
    require(model.count()==calls_before and len(admissions(journey))==4, 'restart replayed input or work')
    terminal.drive('send','--text','\x13')
    terminal.expect('Answer confirmed')
    require(journey.api('GET',base+'/'+form+'/state')['data']==dict(status='answered',answer=dict(objective=answer)),
        'restart lost or changed the explicit question answer')
    checkpoint(journey, 'pending question crash, original R reconnect and explicit answer')
    terminal.drive('send','Escape')
    wait(lambda:'Ctrl+S answer' not in terminal.drive('capture'), 'answered question overlay closes')
    terminal.expect(RECOVER+'X')
    terminal.capture('crash-retained-composer-draft')
    terminal.drive('send',*['BSpace']*(len(RECOVER)+1))
    wait(lambda:any(line.strip('│ ').strip()=='>' for line in terminal.drive('capture').splitlines()), 'explicitly cleared recovery draft')
    terminal.send('/reconcile')
    terminal.expect('recorded 1 lost tool run as unknown')
    terminal.capture('lost-tool-recovery')
    terminal.drive('send','Escape')
    wait(lambda:'Command result' not in terminal.drive('capture'), 'recovery result closes')
    terminal.send('/reconcile')
    terminal.expect('no lost tool runs to reconcile')
    terminal.drive('send','Escape')
    wait(lambda:'Command result' not in terminal.drive('capture'), 'idempotent recovery result closes')
    require(model.count()==calls_before, 'recovery command became model input')
    checkpoint(journey, 'lost nested tool reconciliation is idempotent without replay')
    terminal.send(RECOVER)
    terminal.until(lambda screen:'SUSTAIN_RECOVERY_DONE' in screen,'continued tested task after crash')
    idle(journey)
    require((journey.profile/'effect.txt').read_text()=='effect recorded once\n' and
        (journey.profile/'continued.txt').read_text()=='verified existing effect\n','effect replayed or continued tests missing')
    verify_recovery(committed(journey))
    replies = [data for kind, data in committed(journey)
        if kind == 'form.replied.1' and data['id'] == form]
    require(replies == [dict(id=form, sessionID=SESSION, answer=dict(objective=answer))],
        'question answer was lost, changed or settled more than once')
    checkpoint(journey, 'tested coding continuation preserves the single pre-crash effect')
    write(journey.root/'proxy-requests.json',proxy.requests)
