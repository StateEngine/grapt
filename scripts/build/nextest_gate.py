"""Keep Cargo's test scope while nextest runs isolated tests and retains JUnit."""
from contextlib import contextmanager
import flaky_quarantine
import hashlib
import json
import os
from pathlib import Path
import stat
import tempfile
import xml.etree.ElementTree as ET

MAX_REPORT = 64 * 1024**2


def plan(commands, *, libraries=None):
    result = []
    for command in commands:
        if command[:2] != ['cargo', 'test'] or any(flag in command for flag in ('--test', '--doc', '--no-run')):
            result.append(command)
            continue
        split = command.index('--') if '--' in command else len(command)
        options, arguments = command[2:split], command[split+1:]
        skipped, index = [], 0
        while index < len(arguments):
            arg = arguments[index]
            if arg=='--skip' and index+1<len(arguments):
                skipped.append(arguments[index+1])
                index += 2
            elif arg=='--exact' or arg.startswith('--test-threads='):
                index += 1
            else:
                raise ValueError('Unsupported Cargo test argument; preserve its existing runner: '+arg)
        runner=['cargo','nextest','run',*options,'--profile','gate','--test-threads','3',
                '--retries','0','--no-tests','fail','--user-config-file','none','--ignore-default-filter']
        if skipped:
            if '--exact' not in arguments or any(not name or any(c not in 'abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789_:' for c in name) for name in skipped):
                raise ValueError('Nextest requires bounded exact test skip names')
            runner+=['-E','not ('+' | '.join('test(='+name+')' for name in skipped)+')']
        result.append(runner)
        # Rust doctests use selected libraries and their Cargo features.
        docs = []
        index = 0
        while index < len(options):
            if options[index] == '-p' and libraries is not None:
                if options[index+1] in libraries:
                    docs.extend(options[index:index+2])
                index += 2
            else:
                docs.append(options[index])
                index += 1
        if libraries is not None and '--workspace' not in docs and '--features' in docs:
            # Cargo rejects feature selectors for a binary package omitted from
            # this library-only command. Retain every selected library feature.
            selected = {docs[i+1] for i, arg in enumerate(docs[:-1]) if arg == '-p'}
            position = docs.index('--features')
            features = [feature for feature in docs[position+1].split(',')
                        if '/' not in feature or feature.split('/', 1)[0] in selected]
            docs[position:position+2] = ['--features', ','.join(features)] if features else []
        if libraries is None or '-p' in docs or '--workspace' in docs:
            result.append(['cargo','test',*docs,'--doc',*(['--',*arguments] if arguments else [])])
    return result


def report(log):
    return Path(str(log)+'.nextest.xml')


def read_report(log):
    path = report(log)
    info = path.lstat()
    if not stat.S_ISREG(info.st_mode) or info.st_size>MAX_REPORT:
        raise ValueError('Nextest JUnit must be a bounded regular file')
    descriptor=os.open(path,os.O_RDONLY|os.O_NOFOLLOW|os.O_NONBLOCK)
    with os.fdopen(descriptor, 'rb') as source:
        raw = source.read(MAX_REPORT+1)
    if len(raw)>MAX_REPORT or b'<!DOCTYPE' in raw or b'<!ENTITY' in raw:
        raise ValueError('Nextest JUnit exceeds its safe input bounds')
    return raw


def verify(log, expected=None):
    raw=read_report(log)
    digest=hashlib.sha256(raw).hexdigest()
    if expected is not None and digest!=expected:
        raise ValueError('Retained nextest JUnit changed')
    document=ET.fromstring(raw)
    cases=document.findall('.//testcase')
    if not any(case.find('skipped') is None for case in cases) or document.findall('.//failure') or document.findall('.//error'):
        raise ValueError('Nextest JUnit is empty or contains failing tests')
    return digest


@contextmanager
def invocation(command, log):
    if command[:3]!=['cargo','nextest','run']:
        yield command
        return
    if not isinstance(getattr(log,'name',None),(str,os.PathLike)):
        raise ValueError('Nextest requires a named retained gate log')
    log_path=Path(log.name).resolve()
    config=Path(str(log_path)+'.nextest.toml')
    destination=report(log_path)
    if config.exists() or config.is_symlink() or destination.exists() or destination.is_symlink():
        raise ValueError('Preserve previous nextest output; choose a fresh gate log')
    quarantine=''
    # Gate runs start at the workspace root, where the shipped config
    # carries the shrink-only flaky quarantine. Validate it, then fold
    # its entries into this isolated config so retries happen here too.
    shipped=Path.cwd()/'.config'/'nextest.toml'
    if shipped.is_file() and not shipped.is_symlink():
        raw=shipped.read_text(encoding='utf-8')
        issues=flaky_quarantine.problems(raw)
        if issues:
            raise ValueError('Flaky quarantine policy failed: '+'; '.join(issues))
        quarantine=flaky_quarantine.config_block(raw)
    if quarantine and '--retries' in command:
        # The command-line retries flag outranks profile overrides, so
        # hand retries to the config: profile 0, quarantined tests 1.
        command=list(command)
        index=command.index('--retries')
        del command[index:index+2]
    with tempfile.TemporaryDirectory(prefix='secode-gate-nextest-') as temporary:
        store=Path(temporary)
        text=('nextest-version = "0.9.143"\n[store]\ndir = '+json.dumps(str(store))+\
              '\n[profile.gate]\nretries = 0\nfail-fast = false\n[profile.gate.junit]\npath = "junit.xml"\nreport-skipped = "all"\n')+quarantine
        with config.open('x') as output:
            output.write(text)
        try:
            yield [*command,'--config-file',str(config)]
        finally:
            produced=store/'gate/junit.xml'
            if produced.exists():
                info=produced.lstat()
                if not stat.S_ISREG(info.st_mode) or info.st_size>MAX_REPORT:
                    raise ValueError('Nextest JUnit exceeds its retention bound')
                with produced.open('rb') as source, destination.open('xb') as output:
                    remaining=MAX_REPORT+1
                    while chunk:=source.read(min(65536,remaining)):
                        remaining-=len(chunk)
                        if remaining <= 0:
                            raise ValueError('Nextest JUnit grew beyond its retention bound')
                        output.write(chunk)
                document=ET.fromstring(read_report(log_path))
                for case in document.findall('.//testcase'):
                    elapsed=float(case.get('time','0'))
                    if elapsed>=60 and len(case.get('name',''))<=1024:
                        log.write('# nightly-slow-test: '+case.get('name','')+' '+str(elapsed)+'s ('+case.get('classname','')[:512]+')\n')
                    if case.find('failure') is not None or case.find('error') is not None:
                        name=case.get('classname','')+'::'+case.get('name','')
                        if 0<len(name)<=2048 and '\n' not in name and '\r' not in name:
                            log.write('# nextest failure: '+name+'\n')
                    if case.find('flakyFailure') is not None:
                        name=case.get('classname','')+'::'+case.get('name','')
                        if 0<len(name)<=2048 and '\n' not in name and '\r' not in name:
                            log.write('# nextest flaky: '+name+'\n')
                log.flush()
