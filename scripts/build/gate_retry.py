"""One isolated retry for failed Rust tests outside the affected crate set."""
import hashlib
import io
import json
import os
from pathlib import Path
import re
import subprocess
import stat
import time

MAX_LOG = 64 * 1024 * 1024
MAX_TESTS = 16


def read_log(path):
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_LOG:
        raise ValueError('Gate log must be a regular file within 64 MB')
    descriptor = os.open(path,os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
    with os.fdopen(descriptor,'rb') as source:
        info = os.fstat(source.fileno())
        if not stat.S_ISREG(info.st_mode) or info.st_size > MAX_LOG:
            raise ValueError('Gate log must be a regular file within 64 MB')
        raw = source.read(MAX_LOG+1)
    if len(raw) > MAX_LOG:
        raise ValueError('Gate log exceeds its 64 MB bound')
    return raw


def retry_labels(raw):
    labels = []
    for line in io.BytesIO(raw):
        if line.startswith(b'+ isolated retry: '):
            if len(labels) >= MAX_TESTS or len(line) > 8192:
                raise ValueError('Retry markers exceed their bound')
            labels.append(line.decode('utf-8').rstrip('\r\n'))
    return labels


def diagnostic_command(command):
    if command[:2] != ['cargo', 'test']:
        return None
    result = list(command)
    result.insert(result.index('--') if '--' in result else len(result),
                  '--message-format=json-render-diagnostics')
    return result


def failed_tests(raw, packages, root):
    """Require Cargo artifact identities and complete libtest failure summaries."""
    if len(raw) > MAX_LOG:
        raise ValueError('Original test output exceeds its retry bound')
    members = {p['id']:p for p in packages}
    artifacts = {}
    failures = []
    current = None
    scopes = []
    built = False
    for line in raw.decode('utf-8').splitlines():
        if len(line) > 65536:
            raise ValueError('Test output line exceeds its retry bound')
        if line.startswith('{'):
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if not isinstance(event, dict):
                continue
            if event.get('reason') == 'build-finished':
                built = event.get('success') is True
            if (event.get('reason') == 'compiler-artifact' and event.get('executable')
                    and event.get('profile', {}).get('test') is True):
                package = members.get(event.get('package_id'))
                target = event.get('target')
                if (package is None or not isinstance(target, dict)
                        or not any(all(t.get(k) == target.get(k) for k in ('name','kind','src_path'))
                                   for t in package['targets'])):
                    raise ValueError('Test artifact does not match workspace metadata')
                executable = event['executable']
                if not isinstance(executable, str) or not Path(executable).is_absolute():
                    raise ValueError('Test artifact executable is not absolute')
                artifacts[executable] = {
                    'package_id': package['id'], 'package':package['name'],
                    'target':{k:target[k] for k in ('name','kind','src_path')},
                    'executable':executable, 'features':event.get('features',[]), 'tests':[]}
        match = re.fullmatch(r'\s*Running .+ \((.+)\)', line)
        if match:
            if current is not None:
                raise ValueError('Previous test binary has no complete result')
            artifact = artifacts.get(str((root / match[1]).resolve()))
            if artifact is None:
                raise ValueError('Running test binary has no Cargo artifact identity')
            current = dict(artifact, tests=[])
            scopes = [dict(tests=current['tests'], pending=None, started=False, failures=False)]
        if re.fullmatch(r'\s*Doc-tests .+',line):
            if current is not None:
                raise ValueError('Previous test binary has no complete result')
            current = {'doc':True,'tests':[]}
            scopes = [dict(tests=current['tests'], pending=None, started=False, failures=False)]
        if re.fullmatch(r'running \d+ tests?', line):
            if not scopes:
                raise ValueError('Test run has no Cargo binary identity')
            scope = scopes[-1]
            if scope['started']:
                # Libtest can inherit stdout while its parent test's status is
                # unfinished, or appear in captured failure stdout. Its result
                # belongs to that child, never to the enclosing Cargo artifact.
                if len(scopes) >= 16 or not (scope['pending'] or scope['failures']):
                    raise ValueError('Nested test run has no bounded parent context')
                scopes.append(dict(tests=[], pending=None, started=True, failures=False))
            else:
                scope['started'] = True
        if line == 'failures:' and scopes:
            scopes[-1]['failures'] = True
        match = re.fullmatch(r'test (.+) \.\.\.\s*', line)
        if match:
            if not scopes or not 0 < len(match[1]) <= 1024 or scopes[-1]['pending']:
                raise ValueError('Unfinished test has no bounded parent identity')
            scopes[-1]['pending'] = match[1]
        match = re.fullmatch(r'test (.+) \.\.\. FAILED', line)
        if match:
            if not scopes or not 0 < len(match[1]) <= 1024:
                raise ValueError('Failed test has no bounded binary identity')
            scopes[-1]['tests'].append(match[1])
        if line in ('ok', 'FAILED', 'ignored') and scopes:
            # Crash-writer witnesses deliberately exit before libtest prints
            # a child result. A bare status with no child test in progress is
            # the enclosing parent's completion, not a child summary.
            if (len(scopes) > 1 and not scopes[-1]['pending']
                    and not scopes[-1]['tests'] and scopes[-2]['pending']):
                scopes.pop()
            if scopes[-1]['pending']:
                if line == 'FAILED':
                    scopes[-1]['tests'].append(scopes[-1]['pending'])
                scopes[-1]['pending'] = None
        match = re.match(r'test result: (ok|FAILED)\. \d+ passed; (\d+) failed;', line)
        if match:
            if current is None:
                raise ValueError('Test result has no binary identity')
            scope = scopes[-1]
            tests = scope['tests']
            if scope['pending']:
                raise ValueError('Test summary has an unfinished parent test')
            if len(tests) != int(match[2]) or len(set(tests)) != len(tests):
                raise ValueError('Failure summary does not enumerate every failed test')
            if len(scopes) > 1:
                if (match[1] == 'ok') != (not tests):
                    raise ValueError('Nested test summary disagrees with its failures')
                scopes.pop()
                continue
            if match[1] == 'FAILED':
                if not tests or current.get('doc'):
                    raise ValueError('Failed binary names no failed tests')
                failures.append(current)
            elif tests:
                raise ValueError('Successful binary contains failed tests')
            current = None
            scopes = []
        if line.startswith('error:') and not (line.startswith('error: test failed, to rerun pass ')
                                              or re.fullmatch(r'error: \d+ targets? failed:',line)):
            raise ValueError('Cargo reported a failure other than enumerated tests')
    if (current is not None or not built or not failures
            or not 0 < sum(len(f['tests']) for f in failures) <= MAX_TESTS):
        raise ValueError('Incomplete or excessive original test failures')
    return failures


def passed_retry(raw, name, failure, root):
    lines = raw.decode('utf-8').splitlines()
    artifacts = []
    runs = []
    for line in lines:
        if line.startswith('{'):
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if (isinstance(event,dict) and event.get('reason') == 'compiler-artifact'
                    and event.get('executable') and event.get('profile',{}).get('test') is True):
                if (event.get('package_id') != failure['package_id']
                        or any(event.get('target',{}).get(k) != v for k,v in failure['target'].items())):
                    return False
                artifacts.append(event['executable'])
        match = re.fullmatch(r'\s*Running .+ \((.+)\)',line)
        if match:
            runs.append(str((root/match[1]).resolve()))
    return ('running 1 test' in lines and f'test {name} ... ok' in lines
            and artifacts == [failure['executable']] and runs == [failure['executable']]
            and any(re.match(r'test result: ok\. 1 passed; 0 failed;',line) for line in lines)
            and not any('... FAILED' in line or line.startswith('test result: FAILED') for line in lines))


def retry_command(command, failure, name):
    # Cargo supplies the package working directory and dynamic-library search
    # paths. Preserve its build flags while selecting just one failed test.
    prefix = command[:command.index('--')] if '--' in command else command
    retained = []
    args = iter(prefix[2:])
    for arg in args:
        if arg in ('-p','--package','--exclude','--bin','--test','--bench','--example'):
            next(args)
        elif arg == '--features':
            next(args)
        elif arg in ('--target','--profile'):
            retained.extend([arg,next(args)])
        elif arg in ('--all-features','--no-default-features'):
            pass
        elif arg in ('--locked','--offline','--frozen','--release'):
            retained.append(arg)
        elif arg in ('--workspace','--no-fail-fast','--lib','--bins','--tests','--benches','--examples','--all-targets'):
            pass
        elif arg.startswith('-'):
            raise ValueError('Unsupported original Cargo flag for isolated retry')
        # The original positional name filter is replaced by the failed name.
    # Cargo reports the selected artifact's resolved package features. Replay
    # those, rather than lending workspace-qualified features to one package.
    # passed_retry still refuses a different executable if dependency feature
    # unification cannot be reproduced by this isolated invocation.
    features = failure.get('features', [])
    if (not isinstance(features, list) or len(features) > 256
            or any(not isinstance(feature, str) or not 0 < len(feature) <= 256
                   or re.search(r'[\s,]', feature) for feature in features)):
        raise ValueError('Failed artifact has invalid resolved Cargo features')
    retained.append('--no-default-features')
    if features:
        retained.extend(['--features', ','.join(features)])
    target = failure['target']
    library = {'lib','rlib','dylib','staticlib','cdylib','proc-macro'}
    if target['kind'] and set(target['kind']) <= library:
        selectors = ['--lib']
    elif len(target['kind']) == 1 and target['kind'][0] in ('test','bin','example','bench'):
        selectors = [{'test':'--test','bin':'--bin','example':'--example','bench':'--bench'}[target['kind'][0]],target['name']]
    else:
        raise ValueError('Unsupported failed Cargo test target')
    return diagnostic_command(['cargo','test',*retained,'-p',failure['package'],*selectors,name,
                               '--','--exact','--test-threads=1'])


def span(log, began):
    log.flush()
    log.seek(0, 2)
    length = log.tell()-began
    if not 0 < length <= MAX_LOG:
        raise ValueError('Test output exceeds its retry bound')
    raw = read_log(Path(log.name))
    if len(raw) != began+length:
        raise ValueError('Gate log changed while collecting retry output')
    raw = raw[began:]
    return raw, {'offset':began,'length':length,'sha256':hashlib.sha256(raw).hexdigest()}


def run(command, log, deadline, *, execute, packages, affected, index, retry_budget=MAX_TESTS, **options):
    diagnostic = diagnostic_command(command)
    if diagnostic is None:
        return execute(command,log,deadline,**options), None
    began = time.monotonic()
    log.write('# Cargo diagnostics: '+' '.join(diagnostic)+'\n')
    log.flush()
    offset = log.tell()
    try:
        return execute(diagnostic,log,deadline,**options), None
    except subprocess.CalledProcessError as error:
        if error.returncode != 101:
            raise
        raw, original = span(log,offset)
        try:
            failures = failed_tests(raw,packages,Path(options.get('cwd') or Path.cwd()))
        except (ValueError,UnicodeError,KeyError,TypeError):
            raise error
        if sum(len(f['tests']) for f in failures) > retry_budget:
            raise
        if any(f['package'] in affected for f in failures):
            raise
    retries = []
    for position,failure in enumerate(failures):
        binary = Path(failure['executable'])
        if not binary.is_file() or binary.is_symlink():
            raise ValueError('Failed test executable changed before retry')
        for name in failure['tests']:
            print(f"Retry outside affected set: {failure['package']}::{failure['target']['name']}::{name}",flush=True)
            log.write(f"+ isolated retry: {failure['package']}::{failure['target']['name']}::{name}\n")
            log.flush()
            offset = log.tell()
            retry = retry_command(command,failure,name)
            log.write('+ '+' '.join(retry)+'\n')
            log.flush()
            offset = log.tell()
            execute(retry,log,deadline,**options)
            raw, output = span(log,offset)
            if not passed_retry(raw,name,failure,Path(options.get('cwd') or Path.cwd())):
                raise ValueError('Isolated retry must pass exactly the failed test using the original binary')
            retries.append({'failure':position,'test':name,'exit':0,'command':retry,'log':output})
    return time.monotonic()-began, {
        'command_index':index,'original_exit':101,'original':original,
        'affected':sorted(affected),'failures':failures,'retries':retries}


def verify(records, commands, log, packages, affected, root):
    if not isinstance(records,list) or not 0 < len(records) <= MAX_TESTS:
        raise ValueError('PASS_WITH_RETRY needs bounded retry evidence')
    raw = read_log(log)
    previous_end = 0
    previous_command = -1
    total = 0
    labels = []

    def segment(proof):
        nonlocal previous_end
        if (not isinstance(proof,dict) or set(proof) != {'offset','length','sha256'}
                or type(proof['offset']) is not int or type(proof['length']) is not int
                or proof['offset'] < previous_end or not 0 < proof['length'] <= MAX_LOG
                or proof['offset']+proof['length'] > len(raw)):
            raise ValueError('Retry log span is invalid or overlaps earlier evidence')
        previous_end = proof['offset']+proof['length']
        value = raw[proof['offset']:previous_end]
        if hashlib.sha256(value).hexdigest() != proof['sha256']:
            raise ValueError('Original failure or isolated retry log changed')
        return value

    for record in records:
        if (not isinstance(record,dict) or set(record) != {'command_index','original_exit','original',
                                                         'affected','failures','retries'}
                or type(record['command_index']) is not int
                or not previous_command < record['command_index'] < len(commands)
                or diagnostic_command(commands[record['command_index']]) is None
                or type(record['original_exit']) is not int or record['original_exit'] != 101
                or record['affected'] != sorted(affected)):
            raise ValueError('Invalid original retry command or affected-set proof')
        previous_command = record['command_index']
        failures = failed_tests(segment(record['original']),packages,root)
        if failures != record['failures'] or any(f['package'] in affected for f in failures):
            raise ValueError('Retry includes an affected test or changed original failure set')
        expected = [(i,name) for i,f in enumerate(failures) for name in f['tests']]
        retries = record['retries']
        total += len(expected)
        if not isinstance(retries,list) or len(retries) != len(expected) or total > MAX_TESTS:
            raise ValueError('Retry does not cover exactly the original failed tests')
        for outcome,(position,name) in zip(retries,expected):
            if (not isinstance(outcome,dict) or set(outcome) != {'failure','test','exit','command','log'}
                    or outcome['failure'] != position or outcome['test'] != name
                    or outcome['command'] != retry_command(commands[record['command_index']],failures[position],name)
                    or type(outcome['exit']) is not int or outcome['exit'] != 0
                    or not passed_retry(segment(outcome['log']),name,failures[position],root)):
                raise ValueError('Every isolated retry must run once and pass')
            failure = failures[position]
            labels.append(f"+ isolated retry: {failure['package']}::{failure['target']['name']}::{name}")
    if retry_labels(raw) != labels:
        raise ValueError('Retry markers do not match exactly the recorded attempts')
