"""Native-protocol completion controls; no account or provider is used."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

import compact


class CompactionTests(unittest.TestCase):
    def test_plan_makes_no_calls_or_output_directory(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)/'unused'
            result = subprocess.run([sys.executable, str(Path(compact.__file__)), '--harness', 'codex',
                                     '--binary', '/missing/binary', '--output', str(output), '--lines', '0'],
                                    capture_output=True, text=True, check=True, timeout=10)
            self.assertEqual(json.loads(result.stdout)['provider_calls'], 0)
            self.assertFalse(output.exists())

    def probe(self, harness, evidence):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            binary = root/'native'
            binary.write_text('''#!/usr/bin/python3
import json,sys
from pathlib import Path
EVIDENCE = ''' + repr(evidence) + '''
FACTS = {'project':'copper-lantern','limit':73,'protected':'legacy_bridge.py',
         'order':['validate','persist','publish']}
def emit(row): print(json.dumps(row),flush=True)
if sys.argv[1] == 'export':
    messages = [{'info':{'id':'seed','role':'user'},'parts':[{'type':'compaction'}]}]
    if EVIDENCE:
        messages.append({'info':{'id':'summary','role':'assistant','parentID':'seed',
                                 'summary':True,'finish':'stop'},'parts':[]})
    emit({'messages':messages}); raise SystemExit(0)
count = 0
for line in sys.stdin:
    row = json.loads(line)
    if row.get('type') == 'user':
        text = row['message']['content']
        if EVIDENCE != 'missing-session':
            emit({'type':'system','subtype':'init','session_id':'thread','model':'stub',
                  'claude_code_version':'stub'})
        if text == '/compact' and EVIDENCE:
            boundary = {'type':'system','subtype':'compact_boundary',
                        'session_id':'foreign' if EVIDENCE == 'foreign' else 'thread',
                        'compact_metadata':{'trigger':'auto' if EVIDENCE == 'auto' else 'manual',
                                            'pre_tokens':3000,'post_tokens':1000}}
            if EVIDENCE == 'missing-session': boundary.pop('session_id')
            emit(boundary)
        if text.startswith('Now write'): Path('retained.json').write_text(json.dumps(FACTS))
        completed = {'type':'result','subtype':'success','is_error':False,'session_id':'thread'}
        if EVIDENCE == 'missing-session': completed.pop('session_id')
        emit(completed)
        continue
    if 'id' not in row: continue
    method = row['method']; params = row.get('params',{}); request = row['id']
    if method == 'initialize': emit({'id':request,'result':{'agentInfo':{'version':'stub'}}})
    elif method == 'thread/start':
        emit({'id':request,'result':{'thread':{'id':'thread','cliVersion':'stub'},
                                     'model':'stub','modelProvider':'stub'}})
    elif method == 'session/new': emit({'id':request,'result':{'sessionId':'thread'}})
    elif method == 'session/set_config_option': emit({'id':request,'result':{}})
    elif method == 'session/prompt':
        text = params['prompt'][0]['text']
        if text.startswith('Now write'): Path('retained.json').write_text(json.dumps(FACTS))
        emit({'id':request,'result':{'stopReason':'end_turn'}})
    elif method in ['turn/start','thread/compact/start']:
        count += 1; turn = str(count)
        if method == 'turn/start' and params['input'][0]['text'].startswith('Now write'):
            Path('retained.json').write_text(json.dumps(FACTS))
        emit({'id':request,'result':{'turn':{'id':turn}}})
        if method == 'thread/compact/start' and EVIDENCE:
            emit({'method':'item/completed','params':{'threadId':'thread','turnId':turn,
                                                       'item':{'type':'contextCompaction'}}})
        emit({'method':'turn/completed','params':{'threadId':'thread',
                                                 'turn':{'id':turn,'status':'completed'}}})
''')
            binary.chmod(0o755)
            script = Path(compact.__file__).with_name(harness + '_compaction.py')
            result = subprocess.run([sys.executable, str(script), '--binary', str(binary),
                                     '--lines', '0', '--timeout', '5'], cwd=root,
                                    capture_output=True, text=True, timeout=10)
            return result.returncode, json.loads((root/'probe-result.json').read_text())

    def test_codex_acknowledgment_without_completed_compaction_fails(self):
        code, result = self.probe('codex', False)
        self.assertEqual(code, 1)
        self.assertFalse(result['success'])
        self.assertFalse(result['compaction_completed'])

    def test_codex_requires_completed_record_and_retained_edit(self):
        code, result = self.probe('codex', True)
        self.assertEqual(code, 0)
        self.assertTrue(result['compaction_completed'])
        self.assertTrue(result['retention_passed'])

    def test_opencode_acknowledgment_without_exported_summary_fails(self):
        code, result = self.probe('opencode', False)
        self.assertEqual(code, 1)
        self.assertFalse(result['success'])
        self.assertFalse(result['compaction_completed'])

    def test_opencode_requires_completed_summary_and_retained_edit(self):
        code, result = self.probe('opencode', True)
        self.assertEqual(code, 0)
        self.assertTrue(result['compaction_completed'])
        self.assertTrue(result['retention_passed'])

    def test_claude_acknowledgment_without_native_boundary_fails(self):
        code, result = self.probe('claude', False)
        self.assertEqual(code, 1)
        self.assertFalse(result['compaction_completed'])

    def test_claude_requires_manual_boundary_and_retained_edit(self):
        code, result = self.probe('claude', True)
        self.assertEqual(code, 0)
        self.assertTrue(result['compaction_completed'])
        self.assertTrue(result['retention_passed'])

    def test_claude_foreign_session_boundary_does_not_prove_compaction(self):
        code, result = self.probe('claude', 'foreign')
        self.assertEqual(code, 1)
        self.assertFalse(result['compaction_completed'])

    def test_claude_automatic_boundary_does_not_prove_manual_compaction(self):
        code, result = self.probe('claude', 'auto')
        self.assertEqual(code, 1)
        self.assertFalse(result['compaction_completed'])

    def test_claude_missing_initialization_and_ids_cannot_prove_compaction(self):
        code, result = self.probe('claude', 'missing-session')
        self.assertEqual(code, 1)
        self.assertFalse(result['success'])
        self.assertFalse(result['compaction_completed'])


if __name__ == '__main__':
    unittest.main()
