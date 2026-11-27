"""Bind failed-gate JUnit to its exact workflow run before attributing tests."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import sys
import xml.etree.ElementTree as ET
import zipfile
from datetime import datetime, date, timezone
import tomllib

import batch_compose as compose

MAX_BYTES = 64 * 1024**2
ARTIFACT = 'batch-gate-report'


@dataclass(frozen=True)
class Failure:
    package: str
    binary: str
    test: str


def bounded(path, maximum=MAX_BYTES):
    descriptor = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > maximum:
            raise ValueError('Gate report must be a bounded regular file')
        value = source.read(maximum + 1)
    if len(value) > maximum:
        raise ValueError('Gate report exceeded its bound')
    return value


def collect(directory, head, base, run, attempt=1):
    """Bind the batch job report; an absent JUnit stays absent."""
    if not re.fullmatch('[0-9a-f]{40}', head) or not re.fullmatch('[0-9a-f]{40}', base):
        raise ValueError('Gate report requires exact commit identities')
    directory.mkdir(parents=True, exist_ok=True)
    report = directory / 'gate.log.nextest.xml'
    digest = hashlib.sha256(bounded(report)).hexdigest() if report.exists() else None
    (directory / 'manifest.json').write_text(json.dumps(dict(
        version=1, head=head, base=base, run=str(run), attempt=attempt, junit_sha256=digest)) + '\n')


def unpack(archive, *, head, base, run, attempt=1):
    with zipfile.ZipFile(archive) as bundle:
        entries = bundle.infolist()
        if (len(entries) != 2 or {e.filename for e in entries} != {'manifest.json', 'gate.log.nextest.xml'}
                or sum(e.file_size for e in entries) > MAX_BYTES + 4096
                or any(e.flag_bits & 1 for e in entries)):
            raise ValueError('Failed gate artifact has no bounded complete report')
        manifest = bundle.getinfo('manifest.json')
        if manifest.file_size > 4096:
            raise ValueError('Oversized gate manifest')
        row = json.loads(bundle.read(manifest))
        raw = bundle.read('gate.log.nextest.xml')
    if (row.get('version') != 1 or row.get('head') != head or row.get('base') != base
            or row.get('run') != str(run) or row.get('attempt', 1) != attempt
            or row.get('junit_sha256') != hashlib.sha256(raw).hexdigest()):
        raise ValueError('Gate report does not name this exact failed run')
    return raw


def outcomes(raw, packages, *, flaky=False):
    if len(raw) > MAX_BYTES or b'<!DOCTYPE' in raw.upper() or b'<!ENTITY' in raw.upper():
        raise ValueError('Unsafe or excessive JUnit')
    try:
        document = ET.fromstring(raw)
    except ET.ParseError as error:
        raise ValueError('Malformed JUnit') from error
    if document.tag != 'testsuites':
        raise ValueError('Expected nextest test suites')
    result, executed = [], 0
    identities = {}
    totals = dict(tests=0, errors=0, failures=0, skipped=0)
    suites = document.findall('testsuite')
    if not suites or len(suites) > 4096:
        raise ValueError('Missing or excessive JUnit suites')
    for suite in suites:
        cases = suite.findall('testcase')
        if len(cases) != int(suite.get('tests', '-1')) or len(cases) > 100000:
            raise ValueError('Incomplete JUnit suite')
        failed = sum(case.find('failure') is not None for case in cases)
        errors = sum(case.find('error') is not None for case in cases)
        skipped = sum(case.find('skipped') is not None for case in cases)
        if any(sum(case.find(tag) is not None for tag in ('failure', 'error', 'skipped')) > 1 for case in cases):
            raise ValueError('JUnit testcase has conflicting outcomes')
        if any(case.find('flakyFailure') is not None and any(case.find(tag) is not None
               for tag in ('failure', 'error', 'skipped')) for case in cases):
            raise ValueError('Retried testcase has a conflicting terminal outcome')
        if (errors or int(suite.get('errors', '0')) or failed != int(suite.get('failures', '-1'))
                or skipped != int(suite.get('skipped', '-1'))):
            raise ValueError('Incomplete or interrupted JUnit results')
        executed += len(cases) - skipped
        for key, value in dict(tests=len(cases), errors=errors, failures=failed, skipped=skipped).items():
            totals[key] += value
        for case in cases:
            identities.setdefault(case.get('name', ''), set()).add(case.get('classname', ''))
            if case.find('flakyFailure' if flaky else 'failure') is None:
                continue
            binary, name = case.get('classname', ''), case.get('name', '')
            package = binary.partition('::')[0]
            if (package not in packages or binary != suite.get('name') or not 0 < len(name) <= 2048
                    or len(binary) > 512 or any(c in name + binary for c in '\n\r\0')):
                raise ValueError('Failed test has no exact workspace identity')
            result.append(Failure(package, binary, name))
    if any(document.get(key) is not None and int(document.get(key)) != value for key, value in totals.items()):
        raise ValueError('JUnit root totals disagree with its suites')
    if flaky and totals['failures']:
        raise ValueError('A passing gate report contains final test failures')
    if any(len(identities[f.test]) != 1 for f in result):
        raise ValueError('Test name appears under several classnames')
    if not executed or len(result) > 256 or len(set(result)) != len(result) or not flaky and not result:
        raise ValueError('No bounded unique failure set')
    return result


def failures(raw, packages):
    return outcomes(raw, packages)


def flakies(raw, packages):
    return outcomes(raw, packages, flaky=True)


def quarantine(root, today=None):
    """Read only exact annotated overrides; other filters grant no quarantine."""
    path = root / '.config/nextest.toml'
    if not path.exists():
        return {}
    text = bounded(path, 65536).decode('utf-8')
    profile = tomllib.loads(text).get('profile', {})
    if not isinstance(profile, dict) or not isinstance(profile.get('gate', {}), dict):
        raise ValueError('Quarantine profiles must be tables')
    rows = profile.get('gate', {}).get('overrides', [])
    headers = list(re.finditer(r'(?m)^\[\[profile\.gate\.overrides\]\][ \t]*$', text))
    if (not isinstance(rows, list) or len(rows) > 64 or len(rows) != len(headers)
            or any(not isinstance(row, dict) for row in rows)):
        raise ValueError('Quarantine tables have no unambiguous source annotations')
    result = {}
    today = today or datetime.now(timezone.utc).date()
    for row, header in zip(rows, headers):
        lines = text[:header.start()].splitlines()
        comment = lines[-1] if lines else ''
        metadata = re.fullmatch(r'# flaky-quarantine: owner=([A-Za-z0-9][A-Za-z0-9_-]*) scorecard=([^\s=]+) expiry=(\d{4}-\d{2}-\d{2})', comment)
        if metadata is None:
            raise ValueError('Every gate override requires a valid quarantine annotation')
        value = row.get('filter', '')
        selected = (re.fullmatch(r'test\(=([A-Za-z_][A-Za-z0-9_]*(?:::[A-Za-z_][A-Za-z0-9_]*)*)\)', value)
                    if isinstance(value, str) else None)
        expiry = date.fromisoformat(metadata[3])
        if (set(row) != {'filter', 'retries'} or type(row['retries']) is not int or row['retries'] != 1
                or selected is None or not 0 <= (expiry - today).days <= 7):
            raise ValueError('Invalid or expired exact quarantine override')
        name = selected[1]
        if name in result:
            raise ValueError('Duplicate quarantine test path')
        result[name] = dict(owner=metadata[1], scorecard=metadata[2], expiry=metadata[3])
    return result


def touched(root, base, head, packages):
    ancestor = compose.git(root, 'merge-base', base, head)
    paths = compose.git(root, 'diff', '--name-only', '--no-renames', '-z', ancestor, head).split('\0')
    return {package['name'] for package in packages for path in paths if path.startswith(
        Path(package['manifest_path']).parent.relative_to(root).as_posix() + '/')}


def attribute(failed, changes, quarantined=()):
    # Final failures are real even when a retry override was configured.
    # Only <flakyFailure> represents a failed attempt followed by success.
    names = {}
    for failure in failed:
        names.setdefault(failure.test, set()).add(failure.binary)
    if any(len(binaries) != 1 for binaries in names.values()):
        return dict(kind='ambiguous', excluded=[])
    outside = [f for f in failed if f in quarantined and not any(f.package in crates for crates in changes.values())]
    if outside:
        return dict(kind='quarantine-outside', excluded=[], tests=[f'{f.binary}::{f.test}' for f in outside])
    excluded = set()
    for failure in failed:
        owners = [number for number, crates in changes.items() if failure.package in crates]
        if len(owners) != 1:
            return dict(kind='ambiguous', excluded=[])
        excluded.add(owners[0])
    return dict(kind='unique', excluded=sorted(excluded)) if excluded else dict(kind='ambiguous', excluded=[])


if __name__ == '__main__':
    if len(sys.argv) != 2:
        raise SystemExit('Use batch_report.py REPORT_DIRECTORY in the failed batch workflow')
    collect(Path(sys.argv[1]), os.environ['GITHUB_SHA'], os.environ['CI_BASE_SHA'],
            os.environ['GITHUB_RUN_ID'], int(os.environ.get('GITHUB_RUN_ATTEMPT', '1')))
