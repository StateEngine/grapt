"""No-model witnesses for C04 coverage, privacy and paid stop boundaries."""
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import threading
import time
import unittest
from unittest.mock import patch

import automatic
import automatic_session
import automatic_settings
import compaction_evidence as evidence
import run
from budget import BudgetStop

INITIAL_REFERENCE = '''def migrate(rows):
    result = []
    for row in rows:
        if not isinstance(row, dict) or not isinstance(row.get('tenant'), str) or not row['tenant']:
            raise ValueError('invalid tenant')
        result.append({'tenant_key': row['tenant'], 'value': row.get('value')})
    return result
'''
REFERENCE = INITIAL_REFERENCE + '''
def migrate_many(batches):
    return [row for batch in batches for row in migrate(batch)]
'''


class AutomaticTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.task = json.loads((run.ROOT/'fixtures/benchmarks/automatic-compaction.json').read_text())['tasks'][0]

    def test_plan_does_not_create_output_or_use_a_model(self):
        output = self.root/'unused'
        row = subprocess.run([sys.executable, automatic.__file__, '--output', str(output)],
                             capture_output=True, text=True, timeout=10, check=True)
        self.assertEqual(json.loads(row.stdout)['provider_calls'], 0)
        self.assertEqual(json.loads(row.stdout)['trigger_profile'], 'c04-32k')
        self.assertEqual(json.loads(row.stdout)['threshold_tokens'], 32000)
        self.assertFalse(json.loads(row.stdout)['server_side'])
        self.assertFalse(output.exists())

    def test_deferred_native_compaction_row_is_not_fingerprinted_or_executed(self):
        output = self.root/'held-native'
        argv = ['automatic', '--execute', '--authorization-ref', 'fixture', '--output', str(output),
                '--account-source', str(self.root), '--harness', 'codex', '--harness', 'claude',
                '--defer-harness', 'claude=Subscription launch held', '--claude-binary', '/never/claude']
        row = {'harness': 'codex', 'task': 'C04', 'status': 'completed', 'success': True}
        with patch.object(sys, 'argv', argv), patch.object(automatic, 'aligned'), \
                patch.object(automatic.shutil, 'which', return_value=sys.executable) as which, \
                patch.object(automatic, 'fingerprint', return_value={'path': sys.executable}), \
                patch.object(run, 'attempt', return_value=row) as attempt:
            self.assertEqual(automatic.main(), 1)
        self.assertEqual(which.call_count, 1)
        self.assertNotIn('/never/claude', [call.args[0] for call in which.call_args_list])
        self.assertEqual(attempt.call_count, 1)
        self.assertEqual(attempt.call_args.args[1], 'codex')
        saved = json.loads((output/'results.json').read_text())
        self.assertEqual(saved['planned_harnesses'], ['codex', 'claude'])
        self.assertEqual(saved['deferred_harnesses'], {'claude': 'Subscription launch held'})
        self.assertFalse(saved['measurement_complete'])
        self.assertEqual(len(saved['attempts']), 1)

    def test_long_profile_plan_is_explicit_and_unknown_or_foreign_profiles_refuse(self):
        output = self.root/'unused'
        command = [sys.executable, automatic.__file__, '--output', str(output), '--harness', 'grapt']
        for switch in ['off', 'on']:
            row = subprocess.run(command+['--trigger-profile', 'c04-long-128k', '--server-side', switch],
                                 capture_output=True, text=True, timeout=10, check=True)
            plan = json.loads(row.stdout)
            self.assertEqual(plan['trigger_profile'], 'c04-long-128k')
            self.assertEqual(plan['threshold_tokens'], 128000)
            self.assertEqual(plan['server_side'], switch == 'on')
            self.assertEqual(plan['provider_calls'], 0)
        for options, diagnostic in [(['--trigger-profile', 'unbounded'], 'unbounded'),
                                    (['--trigger-profile', 'c04-long-128k', '--harness', 'codex'], 'requires grapt'),
                                    (['--server-side', 'on', '--harness', 'opencode'], 'requires grapt')]:
            row = subprocess.run(command+options, capture_output=True, text=True, timeout=10)
            self.assertEqual(row.returncode, 2)
            self.assertIn(diagnostic, row.stderr)
        self.assertFalse(output.exists())

    def test_long_launcher_passes_the_selected_policy_and_arm_without_changing_the_fixture(self):
        fixture = run.ROOT/'fixtures/benchmarks/automatic-compaction.json'
        original = fixture.read_bytes()
        output = self.root/'long-plan'
        argv = ['automatic', '--execute', '--authorization-ref', 'synthetic-release',
                '--harness', 'grapt', '--output', str(output), '--account-source', str(self.root),
                '--grapt-compaction-ref', 'synthetic-source',
                '--trigger-profile', 'c04-long-128k', '--server-side', 'on']
        with patch.object(sys, 'argv', argv), patch.object(automatic, 'aligned'), \
                patch.object(automatic.shutil, 'which', return_value=sys.executable), \
                patch.object(automatic, 'fingerprint', return_value={'path': sys.executable}), \
                patch.object(run, 'attempt', return_value={'status': 'completed', 'success': True}) as attempt:
            self.assertEqual(automatic.main(), 0)
        task, _, _, _, timeout, replacements = attempt.call_args.args
        self.assertEqual(task['automatic_compaction']['threshold_tokens'], 128000)
        self.assertEqual(timeout, 600)
        self.assertEqual(replacements['compaction_profile'], 'c04-long-128k')
        self.assertTrue(replacements['compaction_server_side'])
        self.assertEqual(fixture.read_bytes(), original)
        self.assertEqual(self.task['automatic_compaction']['threshold_tokens'], 32000)

    def test_session_profile_refusal_precedes_native_start(self):
        command = [sys.executable, automatic_session.__file__, '--harness', 'grapt',
                   '--binary', '/missing/native', '--model', 'openai/gpt-6-luna']
        visible = {key: self.task[key] for key in ['prompt', 'automatic_compaction']}
        for options, diagnostic in [(['--trigger-profile', 'unknown'], 'Unknown C04 trigger profile'),
                                    (['--trigger-profile', 'c04-long-128k'], 'fixed pressure policy')]:
            row = subprocess.run(command+options, input=json.dumps(visible),
                                 capture_output=True, text=True, timeout=10)
            self.assertNotEqual(row.returncode, 0)
            self.assertIn(diagnostic, row.stderr)
            self.assertNotIn('No such file', row.stderr)

    def test_profile_validation_retains_fixed_workload_and_boolean_arm(self):
        for invalid in ['true', 1, None]:
            with self.assertRaisesRegex(ValueError, 'server_side must be a boolean'):
                automatic_settings.trigger_profile('c04-long-128k', invalid)
        profile = automatic_settings.trigger_profile('c04-long-128k')
        policy = dict(self.task['automatic_compaction'], threshold_tokens=128000)
        automatic_settings.validate_policy(policy, profile)
        for key, value in [('threshold_tokens', 128001), ('archive_blocks', 13),
                           ('archive_block_bytes', 24577), ('final_stage_symbol', 'other')]:
            with self.assertRaisesRegex(ValueError, 'c04-long-128k'):
                automatic_settings.validate_policy(dict(policy, **{key: value}), profile)

    def test_long_profile_effective_configs_differ_only_in_server_side_and_refuse_no_room(self):
        source = self.root/'accounts/luna'
        source.mkdir(parents=True)
        catalog = {'openai': {'models': {'gpt-6-luna': {'limit': {'context': 272000, 'output': 32768}}}}}
        (source/'model-catalog.json').write_text(json.dumps(catalog))
        (source/'opencode.jsonc').write_text(json.dumps({'provider': {'id': 'openai'}}))
        (source/'account-models.json').write_text(json.dumps({'models': [
            {'slug': 'gpt-6-luna', 'context_window': 272000}]}))
        adapter = {'model': 'openai/gpt-6-luna', 'cohort': 'luna'}
        replacements = {'account_source': source.parent, 'grapt_compaction_ref': 'released-source',
                        'compaction_profile': 'c04-long-128k'}
        settings = []
        for server_side in [False, True]:
            replacements['compaction_server_side'] = server_side
            result = automatic_settings.configure({}, 'grapt', adapter, replacements)
            self.assertEqual(result['model_context_window'], 272000)
            self.assertEqual(result['threshold_tokens'], 128000)
            self.assertEqual(result['trigger_profile'], 'c04-long-128k')
            config = result['native_setting']
            self.assertEqual(config['buffer'], 114016)
            self.assertEqual(result['context_budget_tokens']-config['buffer']-16384, 128000)
            self.assertEqual(config['server_side'], server_side)
            service = self.root/str(server_side)
            service.mkdir()
            (service/'opencode.jsonc').write_text('{"provider":{"id":"openai"}}')
            automatic_settings.grapt_profile(service, config)
            applied = json.loads((service/'opencode.jsonc').read_text())
            self.assertEqual(applied['compaction'], config)
            self.assertEqual(applied['provider'], {'id': 'openai'})
            settings.append({key: value for key, value in config.items() if key != 'server_side'})
        self.assertEqual(settings[0], settings[1])
        with self.assertRaisesRegex(ValueError, 'no room'):
            automatic_settings.grapt_config(144383, replacements)

    def test_staged_material_is_fixed_bounded_and_does_not_repeat_constraints(self):
        blocks = [automatic_session.archive(index, 24576) for index in range(12)]
        self.assertEqual(sum(len(block.encode()) for block in blocks), 12*24576)
        self.assertEqual(blocks[0], automatic_session.archive(0, 24576))
        self.assertNotIn('tenant_key', ''.join(blocks))
        self.assertNotIn('tenant_key', self.task['automatic_compaction']['final_prompt'])
        self.assertNotIn('migrate_many', self.task['prompt'])
        self.assertNotIn('migrate_many', ''.join(blocks))
        self.assertFalse(self.task['held_out'])

    def test_hidden_fixture_is_red_on_seed_and_green_on_reference(self):
        for correct in [False, True]:
            repo = self.root/str(correct)
            run.create_snapshot(self.task, repo)
            if correct:
                (repo/'app.py').write_text(REFERENCE)
            status, _ = run.hidden_checks(self.task, repo, self.root/(str(correct)+'.log'))
            self.assertEqual(status, 'completed' if correct else 'failed')

    def test_complete_initial_stage_cannot_pass_the_final_evaluator(self):
        repo = self.root/'initial-only'
        run.create_snapshot(self.task, repo)
        (repo/'app.py').write_text(INITIAL_REFERENCE)
        status, _ = run.hidden_checks(self.task, repo, self.root/'initial-only.log')
        self.assertEqual(status, 'failed')
        self.assertIn('migrate_many', (self.root/'initial-only.log').read_text())

    def test_opencode_ack_or_manual_or_failed_summary_never_counts(self):
        document = {'info': {'id': 's'}, 'messages': [
            {'info': {'id': 'p', 'role': 'user'}, 'parts': [{'type': 'compaction', 'auto': False}]},
            {'info': {'id': 'summary', 'role': 'assistant', 'parentID': 'p', 'summary': True, 'finish': 'stop'}}]}
        self.assertEqual(evidence.opencode(document, 's'), [])
        document['messages'][0]['parts'][0]['auto'] = True
        document['messages'][1]['info']['error'] = {'name': 'failure'}
        self.assertEqual(evidence.opencode(document, 's'), [])
        document['messages'][1]['info'].pop('error')
        document['messages'].append(document['messages'][1])
        self.assertEqual(len(evidence.opencode(document, 's')), 1)
        with self.assertRaises(ValueError): evidence.opencode(document, 'foreign')

    def test_se_history_requires_complete_census_and_consistent_native_ids(self):
        row = {'type': 'compaction', 'status': 'completed', 'reason': 'auto',
               'metadata': {'compactionID': 'c', 'contextWindowID': 'w', 'epoch': 1,
                            'trigger': 'policy-threshold'}}
        page = {'session': 's', 'body': {'data': [row, row], 'cursor': {}}}
        self.assertEqual(len(evidence.grapt([page], 's')), 1)
        with self.assertRaises(ValueError): evidence.grapt([page], 'foreign')
        page['body']['cursor']['next'] = 'more'
        with self.assertRaises(ValueError): evidence.grapt([page], 's')
        page['body']['cursor'] = {}
        row['reason'] = 'manual'
        self.assertEqual(evidence.grapt([page], 's'), [])

    def test_claude_manual_and_foreign_boundaries_are_not_auto(self):
        event = {'type': 'system', 'subtype': 'compact_boundary', 'session_id': 's',
                 'uuid': 'c', 'compact_metadata': {'trigger': 'manual'}}
        self.assertEqual(evidence.claude([event], 's'), [])
        event['compact_metadata']['trigger'] = 'auto'
        self.assertEqual(evidence.claude([event], 'other'), [])
        self.assertEqual(len(evidence.claude([event, event], 's')), 1)

    def test_zero_unknown_and_absent_continuation_fail_coverage(self):
        self.assertEqual(evidence.coverage(None, True)['compaction_coverage'], 'unknown')
        self.assertEqual(evidence.coverage([], True)['compaction_coverage'], 'failed')
        self.assertEqual(evidence.coverage([{'id': 'c'}], False)['compaction_coverage'], 'failed')

    def test_native_config_preserves_the_actual_window_and_refuses_unknown_se_control(self):
        source = self.root/'accounts/luna'
        source.mkdir(parents=True)
        catalog = {'openai': {'models': {'gpt-6-luna': {'id': 'gpt-6-luna',
                                                     'limit': {'context': 272000, 'output': 32768}}}}}
        (source/'model-catalog.json').write_text(json.dumps(catalog))
        (source/'opencode.jsonc').write_text(json.dumps({'provider': {'id': 'openai'}}))
        inventory = {'models': [{'slug': 'gpt-6-luna', 'context_window': 272000}]}
        (source/'account-models.json').write_text(json.dumps(inventory))
        adapter = {'model': 'openai/gpt-6-luna', 'cohort': 'luna'}
        binary = self.root/'opencode'; binary.write_text('synthetic native binary'); binary.chmod(0o700)
        witness = {'binary_sha256': hashlib.sha256(binary.read_bytes()).hexdigest(),
                   'network_disabled': True, 'credentials_mounted': False,
                   'models': {'openai/gpt-6-luna': {
                       'configured_model_sha256': hashlib.sha256(json.dumps(catalog['openai']['models']['gpt-6-luna'],
                           sort_keys=True, separators=(',', ':')).encode()).hexdigest(),
                       'resolved_limit': {'context': 272000, 'input': 272000, 'output': 32768}}}}
        witness_path = self.root/'limits.json'; witness_path.write_text(json.dumps(witness))
        replacements = {'account_source': source.parent, 'opencode_binary': str(binary),
                        'opencode_model_witness': witness_path}
        env = {}
        automatic_settings.configure(env, 'opencode', adapter, replacements)
        config = json.loads(env['OPENCODE_CONFIG_CONTENT'])
        self.assertEqual(config['provider']['openai']['models']['gpt-6-luna']['limit']['context'], 272000)
        self.assertEqual(config['compaction']['buffer'], 240000)
        witness['models']['openai/gpt-6-luna']['resolved_limit'].pop('input')
        witness_path.write_text(json.dumps(witness))
        gap_env = {}
        gap = automatic_settings.configure(gap_env, 'opencode', adapter, replacements)
        self.assertFalse(gap['threshold_controlled'])
        self.assertIsNone(gap['threshold_tokens'])
        self.assertNotIn('OPENCODE_CONFIG_CONTENT', gap_env)
        binary.write_text('different native binary')
        with self.assertRaises(ValueError): automatic_settings.configure({}, 'opencode', adapter, replacements)
        with self.assertRaises(ValueError): automatic_settings.configure({}, 'grapt', adapter, replacements)
        replacements['grapt_compaction_ref'] = 'released-supported-source'
        se = automatic_settings.configure({}, 'grapt', adapter, replacements)
        self.assertEqual(se['native_setting']['buffer'],210016)
        self.assertEqual(se['context_budget_tokens']-se['native_setting']['buffer']-16384,32000)
        self.assertEqual(se['effective_percent_source'], 'SE loader default')
        inventory['models'][0]['effective_context_window_percent'] = 90
        (source/'account-models.json').write_text(json.dumps(inventory))
        explicit = automatic_settings.configure({}, 'grapt', adapter, replacements)
        self.assertEqual(explicit['context_budget_tokens'], 244800)
        self.assertEqual(explicit['inventory_effective_budget_tokens'], 244800)
        self.assertEqual(explicit['native_setting']['buffer'], 196416)
        self.assertEqual(explicit['effective_percent_source'], 'inventory')
        for invalid in [None, True, 0, 101, '95']:
            inventory['models'][0]['effective_context_window_percent'] = invalid
            (source/'account-models.json').write_text(json.dumps(inventory))
            with self.assertRaises(ValueError): automatic_settings.configure({}, 'grapt', adapter, replacements)
        inventory['models'][0]['effective_context_window_percent'] = 95
        inventory['models'].append(dict(inventory['models'][0]))
        (source/'account-models.json').write_text(json.dumps(inventory))
        with self.assertRaises(ValueError): automatic_settings.configure({}, 'grapt', adapter, replacements)

    def test_grapt_target_pressure_stays_constant_when_advertised_window_changes(self):
        source = self.root/'accounts/luna'
        source.mkdir(parents=True)
        (source/'opencode.jsonc').write_text(json.dumps({'provider': {'id': 'openai'}}))
        adapter = {'model': 'openai/gpt-6-luna', 'cohort': 'luna'}
        replacements = {'account_source': source.parent, 'grapt_compaction_ref': 'released-source'}
        buffers = []
        for window in [272000, 1000000]:
            catalog = {'openai': {'models': {'gpt-6-luna': {'limit': {'context': window, 'output': 32768}}}}}
            (source/'model-catalog.json').write_text(json.dumps(catalog))
            (source/'account-models.json').write_text(json.dumps({'models': [
                {'slug': 'gpt-6-luna', 'context_window': window}]}))
            result = automatic_settings.configure({}, 'grapt', adapter, replacements)
            buffer = result['native_setting']['buffer']
            self.assertEqual(window*95//100-buffer-16384, 32000)
            self.assertEqual(result['model_context_window'], window)
            self.assertEqual(result['threshold_tokens'], 32000)
            self.assertEqual(result['context_budget_tokens'], window*95//100)
            buffers.append(buffer)
        self.assertEqual(buffers, [210016, 901616])

    def test_paid_row_and_total_stops_keep_the_weekly_baseline(self):
        key = self.root/'key'; key.write_text('synthetic'); key.chmod(0o600)
        ledger = {'start_usage_usd': 100, 'key_digest': hashlib.sha256(b'synthetic').hexdigest()}
        current = {'usage': 120, 'usage_weekly': 20}
        allocation = {}
        budget = automatic.RowBudget(key, ledger, allocation, observer=lambda _: current)
        self.assertEqual(budget.check()['run_shared_key_usage_usd'], 20)
        current['usage'] = 122.5
        with self.assertRaises(BudgetStop): budget.check()
        budget = automatic.RowBudget(key, ledger, allocation, observer=lambda _: current)
        budget.check()
        current['usage'] = 125
        with self.assertRaises(BudgetStop): budget.check()

    def fake_codex(self, compacted, early=False):
        binary = self.root/'native'
        binary.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
if '--ignore-user-config' in sys.argv: raise RuntimeError('Unsupported app-server option')
COMPACTED = '''+repr(compacted)+'''
EARLY = '''+repr(early)+'''
INITIAL_REFERENCE = '''+repr(INITIAL_REFERENCE)+'''
REFERENCE = '''+repr(REFERENCE)+'''
def emit(row): print(json.dumps(row),flush=True)
count=0
for line in sys.stdin:
 row=json.loads(line); method=row.get('method'); request=row.get('id'); params=row.get('params',{})
 if method == 'thread/compact/start': raise RuntimeError('Manual compaction is forbidden')
 if request is None: continue
 if method == 'initialize': emit({'id':request,'result':{}})
 elif method == 'thread/start': emit({'id':request,'result':{'thread':{'id':'s'},'model':'stub'}})
 elif method == 'turn/start':
  count+=1; identity=str(count); text=params['input'][0]['text']
  if 'Hidden(unittest.TestCase)' in text: raise RuntimeError('Hidden evaluator leaked')
  if count == 1: Path('app.py').write_text(REFERENCE if EARLY else INITIAL_REFERENCE)
  if count < 14 and 'migrate_many' in text: raise RuntimeError('Final-stage reveal leaked early')
  if text.startswith('Add a NEW migrate_many'): Path('app.py').write_text(REFERENCE)
  emit({'id':request,'result':{'turn':{'id':identity}}})
  if COMPACTED and count == 5:
   emit({'method':'item/completed','params':{'threadId':'s','turnId':identity,
        'item':{'type':'contextCompaction','id':'c'}}})
  emit({'method':'turn/completed','params':{'threadId':'s','turn':{'id':identity,'status':'completed'}}})
''')
        binary.chmod(0o755)
        return binary

    def test_hidden_green_without_auto_compaction_is_a_failed_row(self):
        for compacted in [False, True]:
            binary = self.fake_codex(compacted)
            adapter = {'model': 'gpt-6-luna', 'auth': 'none', 'harness': 'codex', 'cohort': 'luna',
                       'argv': [str(binary)]}
            row = run.attempt(self.task, 'codex', adapter, self.root/str(compacted), 15,
                              {'codex_binary': str(binary)})
            self.assertEqual(row['test_status'], 'completed', row)
            self.assertEqual(row['success'], compacted, row)
            self.assertEqual(row['observed_compactions'], int(compacted), row)
            self.assertEqual(row['compaction_coverage'], 'passed' if compacted else 'failed')
            self.assertIsNone(row['usage'])
            self.assertEqual(row['new_work_after_compaction'], compacted)
            self.assertFalse(row['stage_before_final']['declared'])
            self.assertTrue(row['stage_after_final']['declared'])

    def test_final_stage_finished_early_refuses_compaction_coverage(self):
        binary = self.fake_codex(True, early=True)
        adapter = {'model': 'gpt-6-luna', 'auth': 'none', 'harness': 'codex', 'cohort': 'luna',
                   'argv': [str(binary)]}
        row = run.attempt(self.task, 'codex', adapter, self.root/'early', 15, {'codex_binary': str(binary)})
        self.assertEqual(row['test_status'], 'completed', row)
        self.assertFalse(row['success'])
        self.assertTrue(row['stage_before_final']['declared'])
        self.assertEqual(row['compaction_coverage'], 'unknown')

    def test_native_symlink_launch_resolves_inside_sandbox(self):
        binary = self.fake_codex(True)
        link = self.root/'launch-alias'; link.symlink_to(binary)
        adapter = {'model':'gpt-6-luna','auth':'none','harness':'codex','cohort':'luna','argv':[str(link)]}
        row = run.attempt(self.task,'codex',adapter,self.root/'alias',15,{'codex_binary':str(link)})
        self.assertTrue(row['success'],row)

    def test_nonblocking_evidence_pipe_preserves_large_native_export(self):
        reader, writer = os.pipe()
        output = os.fdopen(writer,'w')
        received = bytearray()
        def drain():
            time.sleep(0.05)
            with os.fdopen(reader,'rb') as source:
                while chunk := source.read(4096): received.extend(chunk)
        worker=threading.Thread(target=drain);worker.start()
        try:
            with patch.object(automatic_session.sys,'stdout',output):
                log=automatic_session.Log()
                body=json.dumps({'type':'benchmark.native-export','document':{'payload':'x'*128000}})+'\n'
                log.emit(body)
            output.close();worker.join(timeout=2)
            self.assertFalse(worker.is_alive())
            self.assertEqual(bytes(received),body.encode())
        finally:
            if not output.closed: output.close()
            worker.join(timeout=2)

    def test_unread_evidence_pipe_has_a_deadline(self):
        reader,writer=os.pipe();output=os.fdopen(writer,'w')
        try:
            with patch.object(automatic_session.sys,'stdout',output):
                log=automatic_session.Log();log.deadline=time.monotonic()+0.05
                with self.assertRaises(TimeoutError): log.emit('x'*128000)
        finally:
            output.close();os.close(reader)

    def test_se_public_reader_observes_owned_auto_boundary_and_continued_acp(self):
        service = self.root/'service'; service.mkdir()
        product = self.root/'product'
        product.write_text('''#!/usr/bin/python3
import json,os,sys
from pathlib import Path
from http.server import HTTPServer,BaseHTTPRequestHandler
root=Path(sys.argv[sys.argv.index('--service')+1])
(root/'basic.credential').write_text('test:synthetic')
class Reader(BaseHTTPRequestHandler):
 def log_message(self,*args): pass
 def do_GET(self):
  if self.headers.get('Authorization') != 'Basic dGVzdDpzeW50aGV0aWM=':
   self.send_error(401); return
  count=int((root/'stage').read_text()) if (root/'stage').exists() else 0
  rows=[]
  if count>=5:
   rows=[{'type':'compaction','status':'completed','reason':'auto','metadata':
     {'compactionID':'c','contextWindowID':'w','epoch':1,'trigger':'policy-threshold'}}]
  body=json.dumps({'data':rows,'cursor':{}}).encode()
  self.send_response(200);self.end_headers();self.wfile.write(body)
server=HTTPServer(('127.0.0.1',0),Reader)
(root/'serve-ready.json').write_text(json.dumps({'pid':os.getpid(),
    'observed_endpoint':'http://127.0.0.1:'+str(server.server_port)}))
server.serve_forever()
'''); product.chmod(0o755)
        binary = self.root/'grapt'
        binary.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
root=Path(sys.argv[sys.argv.index('--service')+1]);count=0
REFERENCE = '''+repr(REFERENCE)+'''
for line in sys.stdin:
 row=json.loads(line); method=row['method'];identity=row['id'];result={}
 if method=='session/new':result={'sessionId':'s'}
 elif method=='session/prompt':
  count+=1;(root/'stage').write_text(str(count));result={'stopReason':'end_turn'}
  if count == 14: Path('app.py').write_text(REFERENCE)
  print(json.dumps({'method':'session/update','params':{'sessionId':'s','update':
    {'sessionUpdate':'agent_message_chunk','content':{'type':'text','text':'ACK'}}}}),flush=True)
 print(json.dumps({'id':identity,'result':result}),flush=True)
'''); binary.chmod(0o755)
        for profile, threshold, switch in [('c04-32k', 32000, 'off'), ('c04-long-128k', 128000, 'on')]:
            with self.subTest(profile=profile):
                visible = {'prompt': self.task['prompt'],
                           'automatic_compaction': dict(self.task['automatic_compaction'], threshold_tokens=threshold)}
                (self.root/'app.py').write_text(INITIAL_REFERENCE)
                (service/'stage').unlink(missing_ok=True)
                (service/'serve-ready.json').unlink(missing_ok=True)
                process = subprocess.run([sys.executable, automatic_session.__file__, '--harness','grapt',
                                         '--binary',str(binary),'--product',str(product),'--service',str(service),
                                         '--model','openai/gpt-6-luna', '--trigger-profile', profile,
                                         '--server-side', switch], input=json.dumps(visible),
                                         cwd=self.root, capture_output=True, text=True, timeout=15)
                self.assertEqual(process.returncode,0,process.stdout+process.stderr)
                log = self.root/'agent.jsonl';log.write_text(process.stdout)
                report = run.telemetry(log,'grapt')
                self.assertEqual(report['observed_compactions'],1)
                self.assertTrue(report['continued_same_session'])
                self.assertTrue(report['new_work_after_compaction'])
                self.assertEqual(report['live_acp_updates']['agent_message_chunk'],14)
                self.assertNotIn('dGVzdDpzeW50aGV0aWM=',process.stdout)
                self.assertEqual((service/'stage').read_text(),'14')
                completed = next(json.loads(line) for line in process.stdout.splitlines()
                                 if json.loads(line).get('type') == 'benchmark.automatic.completed')
                self.assertEqual(completed['trigger_profile'], profile)
                self.assertEqual(completed['threshold_tokens'], threshold)
                self.assertEqual(completed['server_side'], switch == 'on')


if __name__ == '__main__':
    unittest.main()
