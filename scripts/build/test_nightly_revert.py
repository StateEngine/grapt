"""Real Git witnesses for nightly publication, dry runs and attribution guards."""
from contextlib import redirect_stdout
import io
import base64
import json
from pathlib import Path
import subprocess
import re
import tempfile
import unittest
from unittest.mock import patch

import nightly
import nightly_revert
import test_nightly
import landing


class RevertTests(unittest.TestCase):
    # Only the new journeys run here; the inherited fixture helpers operate on
    # actual disposable histories. The original suite owns diagnosis coverage.
    setUp = test_nightly.NightlyTests.setUp
    commit = test_nightly.NightlyTests.commit
    check = test_nightly.NightlyTests.check

    def history_with_defect(self):
        good = self.commit(True, 'baseline')
        nightly.git(self.root, 'update-ref', nightly.GOOD_REF, good)
        self.commit(True, 'unrelated earlier work')
        bad = self.commit(False, 'introduced defect')
        candidate = self.commit(False, 'unrelated later work')
        return good, bad, candidate

    def test_published_revert_removes_only_the_bad_change_and_preserves_later_work(self):
        good, bad, candidate = self.history_with_defect()
        canonical = Path(self.temp.name)/'canonical'
        subprocess.run(['git', 'clone', '-q', str(self.root), str(canonical)], check=True)
        nightly.git(canonical, 'checkout', '-B', 'main', candidate)
        plans = [];messages=[]
        actor='agt-actor-fixture';lead='agt-lead-fixture';author='agt-test-fixture'
        agents=[dict(agent_id=actor,display_name='Actor'),dict(agent_id=lead,display_name='Lead'),
                dict(agent_id=author,display_name='Test')]
        def check(root,state,commit):
            state.mkdir(exist_ok=True)
            green=self.check(root,state,commit)
            log=state/('check-'+commit+'.log');log.write_text('green' if green else 'fixture::red')
            return nightly_revert.Check(green,log,() if green else ('fixture::red',))
        def remote(script,**_):
            if 'agents --project' in script:
                return json.dumps(dict(project_id='secode',source='stateengine-registry',agents=agents,lead_agent_id=lead))
            if 'msg send' in script:
                recipient=re.search(r"--to '([^']+)'",script)[1]
                body=base64.b64decode(re.search(r"FromBase64String\('([^']+)'\)",script)[1]).decode()
                messages.append((recipient,body));return '{}'
            branch=re.search(r"-Branch '([^']+)'",script)[1]
            reverted=re.search(r"-AcceptedCommit '([^']+)'",script)[1]
            # Run the real landing source contract before substituting the
            # remote controller's ancestry-preserving merge on a scratch repo.
            self.assertEqual(landing.source(self.root,reverted,branch),dict(commit=reverted,clean=True))
            nightly.git(canonical,'fetch','-q',str(self.root),reverted)
            nightly.git(canonical,'merge','--ff-only',reverted)
            return f'LANDED {reverted} (was {candidate})\n'
        publisher=nightly_revert.Publisher(actor)
        def publish(root, state, plan):
            plans.append(plan)
            return publisher(root,state,plan)
        with patch.object(nightly_revert,'remote',side_effect=remote):
            self.assertEqual(nightly.audit(self.root,self.state,candidate,check,dry_run=False,publish=publish),0)
        reverted = plans[0]['reverted']
        self.assertEqual(nightly.git(canonical,'rev-parse','main'),reverted)
        self.assertEqual(nightly.git(canonical,'rev-parse','HEAD^'),candidate)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),reverted)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),reverted)
        self.assertTrue(self.check(canonical,self.state,reverted))
        self.assertFalse((canonical/'change-2').exists())
        self.assertEqual((canonical/'change-3').read_text(),'unrelated later work')
        self.assertEqual((canonical/'change-1').read_text(),'unrelated earlier work')
        self.assertIn('REVERTED main '+reverted,(self.state/'last-result').read_text())
        self.assertEqual(plans[0]['culprit'],bad)
        self.assertEqual(plans[0]['good'],good)
        self.assertEqual({recipient for recipient,_ in messages},{lead,author})
        for _,body in messages:
            for field in ('culprit','parent','red_log','parent_log','culprit_log','gate_log'):
                self.assertIn(plans[0][field],body)
            self.assertIn('fixture::red',body)

    def test_dry_run_prints_a_tested_plan_without_publishing_or_creating_a_branch(self):
        good, bad, candidate = self.history_with_defect()
        refs = nightly.git(self.root,'show-ref')
        output = io.StringIO()
        with redirect_stdout(output):
            code = nightly.audit(self.root,self.state,candidate,self.check,dry_run=True,
                                 publish=lambda *_: self.fail('dry run published'))
        self.assertEqual(code,1)
        self.assertIn('DRY RUN plan:',output.getvalue())
        self.assertIn(bad,output.getvalue())
        self.assertEqual(nightly.git(self.root,'show-ref'),refs)
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)

    def test_preexisting_red_at_culprit_parent_cannot_publish_a_full_green_landing(self):
        good, bad, candidate = self.history_with_defect()
        parent = nightly.git(self.root,'rev-parse',bad+'^')
        calls = {}
        def check(root,state,commit):
            calls[commit] = calls.get(commit,0)+1
            # It looked green during bisection; replaying the current failure at
            # the exact parent forbids blaming even a previously qualified tip.
            if commit == parent and calls[commit] > 1:
                return False
            return self.check(root,state,commit)
        reports=[]
        test=self
        class Publisher:
            def __call__(self,*_):test.fail('red parent published')
            def report(self,result,failures):reports.append((result,failures))
        self.assertEqual(nightly.audit(self.root,self.state,candidate,check,dry_run=False,publish=Publisher()),2)
        self.assertIn('parent is red',(self.state/'last-result').read_text())
        self.assertEqual(len(reports),1)
        self.assertIn('parent is red',reports[0][0])
        self.assertEqual(nightly.git(self.root,'rev-parse',nightly.GOOD_REF),good)

    def test_capacity_or_receipt_failure_has_no_source_attribution(self):
        good, bad, candidate = self.history_with_defect()
        def infrastructure(*_):
            return nightly_revert.Check(False)
        self.assertEqual(nightly.audit(self.root,self.state,candidate,infrastructure,dry_run=False,
                         publish=lambda *_:self.fail('infrastructure published')),2)
        self.assertIn('infrastructure',(self.state/'last-result').read_text())

    def test_inconclusive_verdict_names_the_tests_it_could_not_attribute(self):
        good = self.commit(True, 'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        candidate = self.commit(False, 'latest')
        # The failure recurs in the previously green baseline, so no commit can
        # be blamed; the operator still has to see which tests failed.
        def check(*_):
            return nightly_revert.Check(False,failures=('recurring::red',))
        self.assertEqual(nightly.audit(self.root,self.state,candidate,check),2)
        result = (self.state/'last-result').read_text()
        self.assertIn('previously green baseline also fails',result)
        self.assertIn('failing: recurring::red',result)
        self.assertNotIn('RED culprit',result)

    def test_missing_green_baseline_verdict_still_names_its_tests(self):
        self.commit(True, 'parent')
        candidate = self.commit(False, 'bad')
        # Without a last green nightly no culprit can be named at all, but the
        # operator still needs the names of the tests that failed.
        def check(*_):
            return nightly_revert.Check(False,failures=('unbaselined::red',))
        self.assertEqual(nightly.audit(self.root,self.state,candidate,check),2)
        result = (self.state/'last-result').read_text()
        self.assertIn('no last green nightly',result)
        self.assertIn('failing: unbaselined::red',result)
        self.assertNotIn('RED culprit',result)

    def test_different_failure_at_bisection_candidate_is_inconclusive(self):
        good, bad, candidate = self.history_with_defect()
        def check(root,state,commit):
            green = self.check(root,state,commit)
            failure = 'other::failure' if commit == bad else 'current::failure'
            return nightly_revert.Check(green,failures=() if green else (failure,))
        self.assertEqual(nightly.audit(self.root,self.state,candidate,check,dry_run=False,
                         publish=lambda *_:self.fail('different failure published')),2)

    def test_conflicting_later_work_is_preserved_without_publishing(self):
        good = self.commit(True, 'baseline')
        nightly.git(self.root,'update-ref',nightly.GOOD_REF,good)
        self.commit(False,'introduced defect')
        (self.root/'test.py').write_text('raise SystemExit(2)\n')
        nightly.git(self.root,'add','--','test.py')
        nightly.git(self.root,'-c','user.name=Test','-c','user.email=test@localhost',
                    'commit','-m','later overlapping work','--','test.py')
        candidate=nightly.git(self.root,'rev-parse','HEAD')
        self.assertEqual(nightly.audit(self.root,self.state,candidate,self.check,dry_run=False,
                         publish=lambda *_:self.fail('conflicted revert published')),2)
        self.assertEqual((self.root/'test.py').read_text(),'raise SystemExit(2)\n')
        self.assertEqual(nightly.git(self.root,'status','--porcelain'),'')
        self.assertEqual(nightly.git(self.root,'rev-parse','HEAD'),candidate)


class PublisherTests(unittest.TestCase):
    def setUp(self):
        self.enterContext(patch.object(nightly.gate, 'nightly_start_allowed'))
        self.temp = tempfile.TemporaryDirectory();self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name);self.state = self.root/'state';self.state.mkdir()
        self.enterContext(patch.object(nightly.full_capacity, 'LOCK', self.root/'full.lock'))
        self.actor = 'agt-actor-fixture';self.lead = 'agt-lead-fixture';self.author = 'agt-author-fixture'
        self.agents = [dict(agent_id=self.actor,display_name='Actor'),
                       dict(agent_id=self.lead,display_name='Lead'),
                       dict(agent_id=self.author,display_name='Author')]
        self.plan = dict(candidate='a'*40,good='b'*40,culprit='c'*40,parent='e'*40,reverted='d'*40,
                         author_name='Author',author_email=self.author+'@stateengine.local',
                         subject='bad change',failures=['unit::red_test'],gate_log='/tmp/revert-full.log',
                         red_log='/tmp/red.log',parent_log='/tmp/parent.log',culprit_log='/tmp/culprit.log')

    def test_ordinary_helper_gets_exact_base_gate_and_actor_then_notifies_both_owners(self):
        scripts=[];messages=[]
        def remote(script,**_):
            scripts.append(script)
            return 'LANDED '+self.plan['reverted']+' (was '+self.plan['candidate']+')\n'
        with patch.object(nightly_revert,'registry',return_value=(self.agents,self.lead)), \
                patch.object(nightly_revert.subprocess,'run') as run, \
                patch.object(nightly_revert,'remote',side_effect=remote), \
                patch.object(nightly_revert,'send',side_effect=lambda *args:messages.append(args)):
            result=nightly_revert.Publisher(self.actor)(self.root,self.state,self.plan)
        self.assertEqual(result,self.plan['reverted'])
        self.assertIn("$env:SECODE_LANDING_CONTROLLER",scripts[0])
        self.assertIn("-ExpectedBase '"+self.plan['candidate']+"'",scripts[0])
        self.assertIn("-GateLog '/tmp/revert-full.log'",scripts[0])
        self.assertEqual({row[1] for row in messages},{self.lead,self.author})
        self.assertTrue(all('unit::red_test' in row[2] and self.plan['reverted'] in row[2] for row in messages))
        self.assertEqual(run.call_args.args[0][:3],['git','checkout','--quiet'])

    def test_unknown_author_refuses_before_branch_or_canonical_action(self):
        self.plan['author_name']='unknown';self.plan['author_email']='unknown@example.test'
        with patch.object(nightly_revert,'registry',return_value=(self.agents,self.lead)), \
                patch.object(nightly_revert.subprocess,'run') as run, \
                patch.object(nightly_revert,'remote') as remote:
            with self.assertRaisesRegex(ValueError,'identify the landing author'):
                nightly_revert.Publisher(self.actor)(self.root,self.state,self.plan)
        run.assert_not_called();remote.assert_not_called()

    def test_disagreeing_author_name_and_seat_email_are_not_guessed(self):
        self.plan['author_email']=self.actor+'@stateengine.local'
        with self.assertRaisesRegex(ValueError,'identify the landing author'):
            nightly_revert.recipients(self.plan,self.agents,self.lead,self.actor)

    def test_refused_landing_sends_no_false_success_mail(self):
        with patch.object(nightly_revert,'registry',return_value=(self.agents,self.lead)), \
                patch.object(nightly_revert.subprocess,'run'), \
                patch.object(nightly_revert,'remote',side_effect=RuntimeError('lease held')), \
                patch.object(nightly_revert,'send') as send:
            with self.assertRaisesRegex(RuntimeError,'lease held'):
                nightly_revert.Publisher(self.actor)(self.root,self.state,self.plan)
        send.assert_not_called()

    def test_failed_mail_does_not_hide_the_successful_canonical_revert(self):
        with patch.object(nightly_revert,'registry',return_value=(self.agents,self.lead)), \
                patch.object(nightly_revert.subprocess,'run'), \
                patch.object(nightly_revert,'remote',return_value='LANDED '+'d'*40+' (was '+'a'*40+')\n'), \
                patch.object(nightly_revert,'send',side_effect=OSError('mail unavailable')):
            publisher=nightly_revert.Publisher(self.actor)
            self.assertEqual(publisher(self.root,self.state,self.plan),'d'*40)
        pending=json.loads((self.state/'pending-notifications.json').read_text())
        self.assertEqual({row['to'] for row in pending},{self.lead,self.author})
        self.assertIn('revert landed',publisher.warning)
        with patch.object(nightly_revert,'registry') as registry:
            with self.assertRaisesRegex(RuntimeError,'earlier revert notification'):
                publisher(self.root,self.state,self.plan)
        registry.assert_not_called()

    def test_invalid_receipt_does_not_attribute_a_suppressed_test_failure_to_source(self):
        def execute(command,output,deadline,**_):
            if command[:3]==['bash','scripts/gate.sh','full']:
                log=Path(command[command.index('--log')+1])
                log.write_text('test previously_retried ... FAILED\n')
                log.with_suffix(log.suffix+'.receipt.json').write_text('{"mode":"full"}')
            else:
                raise subprocess.CalledProcessError(1,command)
        with patch.object(nightly.gate,'execute',side_effect=execute),patch.object(nightly.heavy,'slot'):
            outcome=nightly.full_gate(self.root,self.state,'a'*40)
        self.assertFalse(outcome)
        self.assertEqual(outcome.failures,())

    def test_failed_gate_carries_the_named_red_test_for_attribution(self):
        def execute(command,output,deadline,**_):
            log=Path(command[command.index('--log')+1])
            log.write_text('test regression::red ... FAILED\n')
            raise subprocess.CalledProcessError(1,command)
        with patch.object(nightly.gate,'execute',side_effect=execute),patch.object(nightly.heavy,'slot'):
            outcome=nightly.full_gate(self.root,self.state,'a'*40)
        self.assertFalse(outcome)
        self.assertEqual(outcome.failures,('regression::red',))

    def test_unittest_failure_does_not_report_its_empty_list_assertion_diff(self):
        def execute(command,output,deadline,**_):
            log=Path(command[command.index('--log')+1])
            output.write('+ python3 -m unittest discover -s scripts/build -p test_*.py\n')
            log.write_text('FAIL: test_current_shipped_tree_passes '
                '(test_branding.BrandingTests.test_current_shipped_tree_passes)\n'
                'AssertionError: Lists differ\n+ []\n')
            raise subprocess.CalledProcessError(1,command)
        with patch.object(nightly.gate,'execute',side_effect=execute),patch.object(nightly.heavy,'slot'):
            outcome=nightly.full_gate(self.root,self.state,'a'*40)
        self.assertFalse(outcome)
        self.assertEqual(outcome.failures,('unittest: test_branding.BrandingTests.test_current_shipped_tree_passes',))

    def test_command_fallback_uses_driver_not_child_output(self):
        def execute(command,output,deadline,**_):
            log=Path(command[command.index('--log')+1])
            output.write('+ cargo clippy --workspace -- -D warnings\n')
            log.write_text('error: compiler diagnostic\n+ []\n+ imaginary child command\n')
            raise subprocess.CalledProcessError(1,command)
        with patch.object(nightly.gate,'execute',side_effect=execute),patch.object(nightly.heavy,'slot'):
            outcome=nightly.full_gate(self.root,self.state,'a'*40)
        self.assertEqual(outcome.failures,('gate command: cargo clippy --workspace -- -D warnings',))

    def test_empty_or_malformed_driver_commands_have_no_source_attribution(self):
        for driver in ('', '+ []\n', '+ \"\"\n', '+    \n', '+ \"unterminated\n'):
            with self.subTest(driver=driver):
                self.assertEqual(nightly.source_failures('+ []\n',driver),())

    def test_unittest_import_error_keeps_its_named_failure(self):
        self.assertEqual(nightly.source_failures(
            'ERROR: test_fixture (unittest.loader._FailedTest.test_fixture)\n', ''),
            ('unittest: unittest.loader._FailedTest.test_fixture',))


if __name__=='__main__':
    unittest.main()
