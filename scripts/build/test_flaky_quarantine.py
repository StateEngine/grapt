"""The shrink-only quarantine parses, expires and retries exactly one named test."""
import os
from datetime import date, timedelta
from pathlib import Path
import subprocess
import tempfile
import unittest

from fixture_environment import isolated_environment
import flaky_quarantine
import nextest_gate

HEADER=('nextest-version = "0.9.143"\n[profile.gate]\nretries = 0\nfail-fast = false\n')

def entry(owner='reviewer', scorecard='gate33-slow', expiry='2026-11-23',
          test='a::b::c', retries=1):
    comment='# flaky-quarantine: owner=%s scorecard=%s expiry=%s' % (owner,scorecard,expiry)
    return comment+'\n[[profile.gate.overrides]]\nfilter = \'test(='+test+')\'\nretries = '+str(retries)+'\n'

def file_with(blocks):
    return HEADER+''.join(blocks)


class ParseTests(unittest.TestCase):
    def test_shipped_config_is_valid_and_unexpired(self):
        raw=(Path(__file__).resolve().parents[2]/'.config'/'nextest.toml').read_text()
        entries=flaky_quarantine.parse_entries(raw)
        self.assertEqual(flaky_quarantine.problems(raw),[])
        self.assertEqual(flaky_quarantine.quarantined_tests(raw),
                         frozenset(row.test for row in entries))
        self.assertEqual(bool(flaky_quarantine.config_block(raw)),bool(entries))

    def test_valid_entry_parses_and_round_trips(self):
        text=file_with([entry()])
        rows=flaky_quarantine.parse_entries(text)
        self.assertEqual(len(rows),1)
        self.assertEqual(rows[0].owner,'reviewer')
        self.assertEqual(rows[0].scorecard,'gate33-slow')
        self.assertEqual(rows[0].expiry,date(2026,11,23))
        self.assertEqual(rows[0].test,'a::b::c')
        self.assertEqual(rows[0].retries,1)
        block=flaky_quarantine.config_block(text)
        self.assertEqual(flaky_quarantine.parse_entries(HEADER+block),rows)
        self.assertEqual(flaky_quarantine.quarantined_tests(text),frozenset({'a::b::c'}))

    def test_comment_without_table_fails(self):
        with self.assertRaisesRegex(ValueError,'needs an override table'):
            flaky_quarantine.parse_entries(HEADER+'# flaky-quarantine: owner=d scorecard=s expiry=2026-11-23\n')

    def test_missing_comment_fields_fail(self):
        for comment in ('# flaky-quarantine: scorecard=s expiry=2026-11-23',
                        '# flaky-quarantine: owner=d expiry=2026-11-23',
                        '# flaky-quarantine: owner=d scorecard=s',
                        '# flaky-quarantine: owner=d scorecard=s expiry=2026/11/23'):
            with self.assertRaises(ValueError, msg=comment):
                flaky_quarantine.parse_entries(
                    comment+'\n[[profile.gate.overrides]]\nfilter = \'test(=a::b::c)\'\nretries = 1\n')

    def test_bad_calendar_date_fails(self):
        with self.assertRaisesRegex(ValueError,'calendar date'):
            flaky_quarantine.parse_entries(file_with([entry(expiry='2026-02-30')]))

    def test_wrong_retries_fails(self):
        with self.assertRaisesRegex(ValueError,'retries must be 1'):
            flaky_quarantine.parse_entries(file_with([entry(retries=2)]))

    def test_wildcard_and_multi_filters_fail(self):
        for text in (file_with([entry(test='a::b::*')]),
                     file_with([entry(test='a::b::c|d::e::f')])):
            with self.assertRaisesRegex(ValueError,'one exact test'):
                flaky_quarantine.parse_entries(text)

    def test_table_keys_must_be_exactly_filter_and_retries(self):
        with self.assertRaisesRegex(ValueError,'exactly filter and retries'):
            flaky_quarantine.parse_entries(
                file_with([entry()[:-len('\nretries = 1\n')]]))
        with self.assertRaisesRegex(ValueError,'exactly filter and retries'):
            flaky_quarantine.parse_entries(
                HEADER+entry()+'slow-time = 10\n')

    def test_hidden_override_keys_are_refused_by_every_consumer(self):
        # Spellings the raw table scan cannot bind to an entry: an indented
        # or quoted key after the annotated block, or a TOML subtable. All of
        # them parse into the same override row, so the parsed shape check
        # must refuse them at every public consumer; config_block must never
        # emit a policy the quarantine does not own.
        extras=('  slow-timeout = { period = "1s", terminate-after = 1 }\n',
                '"slow-timeout" = { period = "1s", terminate-after = 1 }\n',
                '[profile.gate.overrides.slow-timeout]\n'
                'period = "1s"\nterminate-after = 1\n')
        for extra in extras:
            text=HEADER+entry()+'\n'+extra
            for consumer in (flaky_quarantine.parse_entries,flaky_quarantine.problems,
                             flaky_quarantine.config_block,flaky_quarantine.quarantined_tests):
                with self.subTest(extra=extra,consumer=consumer.__name__):
                    with self.assertRaisesRegex(ValueError,'exactly filter and retries'):
                        consumer(text)

    def test_uncommented_gate_override_fails(self):
        with self.assertRaisesRegex(ValueError,'without a flaky-quarantine comment'):
            flaky_quarantine.parse_entries(HEADER+'[[profile.gate.overrides]]\nfilter = \'test(=a::b::c)\'\nretries = 1\n')

    def test_duplicate_test_fails(self):
        with self.assertRaisesRegex(ValueError,'same test twice'):
            flaky_quarantine.parse_entries(file_with([entry(),entry()]))

    def test_entry_cap_fails(self):
        blocks=[entry(owner='d',scorecard='s',test='t::m%d::x'%number)
                for number in range(flaky_quarantine.MAX_ENTRIES+1)]
        with self.assertRaisesRegex(ValueError,'exceeds 64 entries'):
            flaky_quarantine.parse_entries(file_with(blocks))

    def test_unannotated_header_spellings_fail(self):
        # TOML accepts these header spellings; the inventory check must
        # refuse them instead of reading the file as an empty quarantine.
        for header in ('[[profile.gate.overrides]] # unannotated',
                       '[[ profile.gate.overrides ]]',
                       '  [[profile.gate.overrides]]',
                       '[[ "profile".gate."overrides" ]]'):
            text=HEADER+header+'\nfilter = \'test(=a::b::c)\'\nretries = 1\n'
            with self.assertRaisesRegex(ValueError,
                                        'without a flaky-quarantine comment',
                                        msg=header):
                flaky_quarantine.parse_entries(text)

    def test_comment_with_noncanonical_header_fails(self):
        comment='# flaky-quarantine: owner=d scorecard=s expiry=2026-11-23'
        with self.assertRaisesRegex(ValueError,'needs an override table'):
            flaky_quarantine.parse_entries(
                HEADER+comment+'\n[[ profile.gate.overrides ]]\n'
                'filter = \'test(=a::b::c)\'\nretries = 1\n')

    def test_string_retries_refused_by_inventory(self):
        text=(HEADER+'# flaky-quarantine: owner=d scorecard=s expiry=2026-11-23\n'
              '[[profile.gate.overrides]]\nfilter = \'test(=a::b::c)\'\nretries = "1"\n')
        with self.assertRaisesRegex(ValueError,'retries must be 1'):
            flaky_quarantine.parse_entries(text)

    def test_invalid_toml_is_refused(self):
        with self.assertRaisesRegex(ValueError,'not valid TOML'):
            flaky_quarantine.parse_entries(HEADER+'[[unclosed\n'+entry())


class ExpiryTests(unittest.TestCase):
    def test_seven_day_expiry_is_allowed(self):
        today=date(2026,11,16)
        for delta in (0,3,7):
            expiry=(today+timedelta(days=delta)).isoformat()
            self.assertEqual(flaky_quarantine.problems(file_with([entry(expiry=expiry)]),today),[],expiry)

    def test_expired_entry_is_a_problem(self):
        today=date(2026,11,16)
        issues=flaky_quarantine.problems(file_with([entry(expiry='2026-11-15')]),today)
        self.assertEqual(issues,['a::b::c: quarantine expired on 2026-11-15'])

    def test_horizon_beyond_seven_days_is_a_problem(self):
        today=date(2026,11,16)
        issues=flaky_quarantine.problems(file_with([entry(expiry='2026-11-24')]),today)
        self.assertEqual(issues,['a::b::c: quarantine horizon exceeds 7 days (expiry 2026-11-24)'])


class GateIntegrationTests(unittest.TestCase):
    def fixture(self,root):
        (root/'src').mkdir()
        (root/'.config').mkdir()
        (root/'Cargo.toml').write_text('[package]\nname="quarantine-probe"\nversion="0.1.0"\nedition="2021"\n')
        (root/'src/lib.rs').write_text(
            'use std::path::PathBuf;\n'
            '#[test]\nfn flaky_once() {\n'
            '    let marker=PathBuf::from(std::env::var("CARGO_TARGET_DIR").unwrap()).join("flaky-attempted");\n'
            '    if marker.exists() { return; }\n'
            '    std::fs::write(&marker, b"attempted").unwrap();\n'
            '    panic!("first attempt must fail");\n}\n'
            '#[test]\nfn steady() {}\n')

    def test_quarantined_flake_passes_on_retry_and_marks_the_log(self):
        with tempfile.TemporaryDirectory(prefix='secode-quarantine-e2e-') as directory:
            root=Path(directory)
            self.fixture(root)
            expiry=(date.today()+timedelta(days=3)).isoformat()
            (root/'.config'/'nextest.toml').write_text(
                file_with([entry(expiry=expiry,test='flaky_once')]))
            env=dict(isolated_environment(),CARGO_TARGET_DIR=str(root/'target'))
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=env,
                           check=True,capture_output=True,timeout=20)
            planned=nextest_gate.plan([['cargo','test','--locked','-p','quarantine-probe',
                                        '--no-fail-fast','--','--test-threads=1']])
            previous=os.getcwd()
            os.chdir(root)
            try:
                path=root/'gate.log'
                with path.open('w') as log:
                    with nextest_gate.invocation(planned[0],log) as command:
                        self.assertNotIn('--retries',command)
                        outcome=subprocess.run(command,cwd=root,env=env,stdout=log,
                                               stderr=subprocess.STDOUT,timeout=180)
                self.assertEqual(outcome.returncode,0)
                raw=path.read_text()
                self.assertIn('# nextest flaky: quarantine-probe::flaky_once',raw)
                self.assertNotIn('# nextest failure:',raw)
                nextest_gate.verify(path)
            finally:
                os.chdir(previous)

    def test_invocation_refuses_an_expired_quarantine(self):
        with tempfile.TemporaryDirectory(prefix='secode-quarantine-refuse-') as directory:
            root=Path(directory)
            self.fixture(root)
            (root/'.config'/'nextest.toml').write_text(
                file_with([entry(expiry=(date.today()-timedelta(days=1)).isoformat())]))
            env=dict(isolated_environment(),CARGO_TARGET_DIR=str(root/'target'))
            subprocess.run(['cargo','generate-lockfile','--offline'],cwd=root,env=env,
                           check=True,capture_output=True,timeout=20)
            planned=nextest_gate.plan([['cargo','test','--locked','-p','quarantine-probe',
                                        '--no-fail-fast','--','--test-threads=1']])
            previous=os.getcwd()
            os.chdir(root)
            try:
                path=root/'gate.log'
                with path.open('w') as log:
                    with self.assertRaisesRegex(ValueError,'Flaky quarantine policy failed'):
                        with nextest_gate.invocation(planned[0],log):
                            pass
            finally:
                os.chdir(previous)


if __name__=='__main__':
    unittest.main()
