"""Judge failed batch attribution using complete, identity-bound reports."""
import hashlib
import io
import json
import unittest
import xml.etree.ElementTree as ET
import zipfile

import batch_report as report


HEAD, BASE, RUN = 'a' * 40, 'b' * 40, '123'
PACKAGES = {'leaf', 'other', 'shared'}


def junit(package='leaf', binary='public_seams', name='keeps_the_receipt'):
    root = ET.Element('testsuites', tests='2', failures='1', errors='0', skipped='0')
    suite = ET.SubElement(root, 'testsuite', name=f'{package}::{binary}',
                          tests='2', failures='1', errors='0', skipped='0')
    case = ET.SubElement(suite, 'testcase', classname=suite.get('name'), name=name)
    ET.SubElement(case, 'failure', message='behavior differed')
    ET.SubElement(suite, 'testcase', classname=suite.get('name'), name='control')
    return root


def encoded(root):
    return ET.tostring(root, encoding='utf-8')


def artifact(raw, **changed):
    manifest = dict(version=1, head=HEAD, base=BASE, run=RUN,
                    junit_sha256=hashlib.sha256(raw).hexdigest())
    manifest.update(changed)
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as bundle:
        bundle.writestr('manifest.json', json.dumps(manifest))
        bundle.writestr('gate.log.nextest.xml', raw)
    stream.seek(0)
    return stream


class BatchAttribution(unittest.TestCase):
    def test_one_owned_failure_excludes_only_its_touched_pull_request(self):
        failed = report.failures(encoded(junit()), PACKAGES)
        self.assertEqual(failed, [report.Failure('leaf', 'leaf::public_seams',
                                                'keeps_the_receipt')])
        self.assertEqual(report.attribute(failed, {11: {'leaf'}, 12: {'other'}}),
                         {'kind': 'unique', 'excluded': [11]})

    def test_shared_and_outside_touched_failures_never_choose_a_culprit(self):
        for package, owners in [('shared', {11: {'shared'}, 12: {'shared'}}),
                                ('other', {11: {'leaf'}, 12: set()})]:
            with self.subTest(package=package):
                failed = report.failures(encoded(junit(package)), PACKAGES)
                self.assertEqual(report.attribute(failed, owners),
                                 {'kind': 'ambiguous', 'excluded': []})

    def test_one_ambiguous_failure_prevents_partial_exclusion(self):
        failed = [report.Failure('leaf', 'leaf::public_seams', 'owned'),
                  report.Failure('shared', 'shared::unit', 'shared')]
        self.assertEqual(report.attribute(failed, {11: {'leaf', 'shared'}, 12: {'shared'}}),
                         {'kind': 'ambiguous', 'excluded': []})

    def test_a_quarantined_final_failure_still_excludes_its_unique_owner(self):
        failed = report.failures(encoded(junit()), PACKAGES)
        self.assertEqual(report.attribute(failed, {11: {'leaf'}}, quarantined=failed),
                         {'kind': 'unique', 'excluded': [11]})

    def test_quarantine_must_match_package_binary_and_test(self):
        failure = report.Failure('leaf', 'leaf::public_seams', 'keeps_the_receipt')
        near = [report.Failure('other', failure.binary, failure.test),
                report.Failure('leaf', 'leaf::unit', failure.test),
                report.Failure('leaf', failure.binary, 'keeps_the_receipt_extra')]
        for quarantined in near:
            with self.subTest(quarantined=quarantined):
                self.assertEqual(report.attribute([failure], {11: {'leaf'}}, [quarantined]),
                                 {'kind': 'unique', 'excluded': [11]})

    def test_quarantined_final_failure_outside_touched_crates_stops_the_batch(self):
        old = report.Failure('shared', 'shared::unit', 'known_flake')
        new = report.Failure('leaf', 'leaf::public_seams', 'new_failure')
        result = report.attribute([old, new], {11: {'leaf'}}, [old])
        self.assertEqual(result['kind'], 'quarantine-outside')
        self.assertEqual(result['excluded'], [])

    def test_same_test_name_in_multiple_binaries_cannot_choose_a_culprit(self):
        failed = [report.Failure('leaf', 'leaf::unit', 'same_name'),
                  report.Failure('leaf', 'leaf::public_seams', 'same_name')]
        self.assertEqual(report.attribute(failed, {11: {'leaf'}}),
                         {'kind': 'ambiguous', 'excluded': []})

    def test_a_pass_after_retry_is_report_only_and_never_a_final_failure(self):
        root = junit()
        root.set('tests', '3')
        suite = root.find('testsuite')
        suite.set('tests', '3')
        retried = ET.SubElement(suite, 'testcase', classname=suite.get('name'),
                                name='passed_after_retry')
        ET.SubElement(retried, 'flakyFailure', message='first attempt failed')
        failed = report.failures(encoded(root), PACKAGES)
        self.assertEqual(failed, [report.Failure('leaf', 'leaf::public_seams',
                                                'keeps_the_receipt')])
        self.assertEqual(report.attribute(failed, {11: {'leaf'}}),
                         {'kind': 'unique', 'excluded': [11]})

    def test_empty_failure_set_cannot_authorize_exclusion(self):
        self.assertEqual(report.attribute([], {11: {'leaf'}}),
                         {'kind': 'ambiguous', 'excluded': []})

    def test_unknown_workspace_identity_refuses_attribution(self):
        with self.assertRaises(ValueError):
            report.failures(encoded(junit('unknown')), PACKAGES)

    def test_each_suite_count_must_describe_the_complete_report(self):
        for key, value in [('tests', '3'), ('failures', '0'), ('errors', '1'), ('skipped', '1')]:
            with self.subTest(key=key):
                root = junit()
                root.find('testsuite').set(key, value)
                with self.assertRaises(ValueError):
                    report.failures(encoded(root), PACKAGES)

    def test_root_counts_cannot_claim_missing_suites_or_results(self):
        for key, value in [('tests', '3'), ('failures', '2'), ('errors', '1'), ('skipped', '1')]:
            with self.subTest(key=key):
                root = junit()
                root.set(key, value)
                with self.assertRaises(ValueError):
                    report.failures(encoded(root), PACKAGES)

    def test_a_failure_cannot_also_be_a_skipped_result(self):
        root = junit()
        root.set('skipped', '1')
        suite = root.find('testsuite')
        suite.set('skipped', '1')
        ET.SubElement(suite.find('testcase'), 'skipped')
        with self.assertRaises(ValueError):
            report.failures(encoded(root), PACKAGES)

    def test_duplicate_failure_identity_is_not_a_complete_failure_set(self):
        root = junit()
        suite = root.find('testsuite')
        second = suite.findall('testcase')[1]
        second.set('name', 'keeps_the_receipt')
        ET.SubElement(second, 'failure')
        suite.set('failures', '2')
        root.set('failures', '2')
        with self.assertRaises(ValueError):
            report.failures(encoded(root), PACKAGES)

    def test_interrupted_malformed_and_entity_reports_cannot_exclude(self):
        for raw in [b'', b'<testsuites>', b'<testcase/>',
                    b'<!DOCTYPE x [<!ENTITY x "value">]><testsuites/>']:
            with self.subTest(raw=raw):
                with self.assertRaises((ValueError, ET.ParseError)):
                    report.failures(raw, PACKAGES)


class BatchArtifactBinding(unittest.TestCase):
    def test_exact_report_bytes_survive_binding(self):
        raw = encoded(junit())
        self.assertEqual(report.unpack(artifact(raw), head=HEAD, base=BASE, run=RUN), raw)

    def test_wrong_run_base_head_version_or_digest_refuses_the_report(self):
        raw = encoded(junit())
        for key, value in [('run', '124'), ('base', 'c' * 40), ('head', 'd' * 40),
                           ('version', 2), ('junit_sha256', '0' * 64)]:
            with self.subTest(key=key):
                with self.assertRaises(ValueError):
                    report.unpack(artifact(raw, **{key: value}), head=HEAD, base=BASE, run=RUN)

    def test_a_manifest_without_the_report_is_not_success_or_no_failures(self):
        stream = io.BytesIO()
        with zipfile.ZipFile(stream, 'w') as bundle:
            bundle.writestr('manifest.json', json.dumps(dict(version=1, head=HEAD, base=BASE,
                                                            run=RUN, junit_sha256=None)))
        stream.seek(0)
        with self.assertRaises(ValueError):
            report.unpack(stream, head=HEAD, base=BASE, run=RUN)


if __name__ == '__main__':
    unittest.main()
