"""Checkpoint batch attempts, attribute failures, and publish one qualified head."""
from dataclasses import asdict
import json
from pathlib import Path
import subprocess
import zipfile

import batch_compose as compose
import batch_github as github
from batch_mirror import mirror
import batch_notify
import batch_report
from batch_staging import qualify
from batch_state import save


class StageFailure(RuntimeError):
    def __init__(self, message, *, log=None):
        super().__init__(message)
        self.log = log


class Pipeline:
    def __init__(self, options, state):
        self.options, self.state = options, state
        self.path = options.state / 'batch.json'

    def record(self, **values):
        self.state.update(values)
        save(self.path, self.state)

    def notice(self, phase, result, numbers=None, extra=()):
        numbers = self.state['requested'] if numbers is None else numbers
        key = f'{self.state.get("attempt", 0)}/{phase}/{self.state.get("head", "")}'
        batch_notify.enqueue(self.state, key, phase, result[:3000], numbers, extra)
        self.record()
        batch_notify.flush(self.state, self.record)

    def exclude(self, number, reason):
        if number not in {item['pr'] for item in self.state['excluded']}:
            self.state['excluded'].append({'pr': number, 'reason': str(reason)[:2048]})
            self.record()
        print(f'Excluded pull request {number}: {reason}', flush=True)

    def checkout(self, rows, attempt):
        root = Path(attempt['root'])
        compose.git(self.options.repository, 'worktree', 'add', '--detach', str(root), self.state['base'])
        included = []
        for row in rows:
            before = compose.git(root, 'rev-parse', 'HEAD')
            try:
                compose.merge(root, row.number, row.head, self.state['base'])
            except (ValueError, subprocess.CalledProcessError) as error:
                compose.discard_merge(root, before)
                self.exclude(row.number, error)
            else:
                included.append(row)
        violations = compose.scan(root, self.state['base'])
        if violations:
            raise ValueError('\n'.join(violations))
        attempt.update(head=compose.git(root, 'rev-parse', 'HEAD'),
                       included=[asdict(row) for row in included], phase='composed')
        self.record(active=attempt, checkout=str(root), head=attempt['head'])
        self.notice('composition', f'Composed {attempt["head"]}', [r.number for r in included])

    def push_new(self, root, head, branch):
        existing = compose.git(root, 'ls-remote', '--heads', 'github', 'refs/heads/' + branch).split()
        if existing:
            if existing[0] == head:
                return
            raise ValueError('Preserve remote ref at a different commit: ' + branch)
        compose.git(root, 'push', 'github', f'{head}:refs/heads/{branch}')
        actual = compose.git(root, 'ls-remote', '--heads', 'github', 'refs/heads/' + branch).split()
        if not actual or actual[0] != head:
            raise ValueError('Remote ref was not read back at the expected commit')

    def attempt_gate(self, rows):
        wanted = [asdict(row) for row in rows]
        attempt = self.state.get('active')
        if not attempt or attempt['rows'] != wanted or attempt['phase'] == 'composing':
            count = self.state.get('attempt', 0) + 1
            name = self.options.name + (f'-{count}' if count > 1 else '')
            attempt = dict(rows=wanted, phase='composing', name=name,
                           root=str(self.options.worktrees / ('landing-' + name)))
            self.record(attempt=count, active=attempt, phase='composition')
            self.checkout(rows, attempt)
        root, head = Path(attempt['root']), attempt['head']
        included = [github.PullRequest(**row) for row in attempt['included']]
        if rows and not included:
            raise ValueError('No admitted pull requests remain after composition')
        if compose.git(root, 'rev-parse', 'HEAD') != head:
            raise ValueError('Staging source moved since composition')
        compose.git(root, 'diff', '--exit-code', head, '--')
        name = attempt['name']
        staging, batch = 'staging/' + name, 'batch/' + name
        if attempt['phase'] == 'failed':
            failed = attempt['failure']
            if failed['kind'] == 'stage':
                raise StageFailure(failed['message'], log=Path(failed['log']))
            raise github.GateFailure(failed['message'], run=failed['run'], head=head, branch=batch,
                                     attempt=failed.get('attempt', 1))
        self.record(phase='staging-push', head=head, batch=batch)
        self.push_new(root, head, staging)
        if not attempt.get('staged'):
            serial = attempt.get('prechecks', 0) + 1
            attempt['prechecks'] = serial
            log = self.options.state / f'{name}.staging-{serial}.log'
            attempt['log'] = str(log)
            self.record(phase='staging-tests', staging_log=str(log))
            try:
                seconds = qualify(root, self.state['base'], log, self.options.staging_timeout)
            except (ValueError, subprocess.SubprocessError) as error:
                attempt.update(phase='failed', failure=dict(kind='stage', message=str(error), log=str(log)))
                self.record()
                raise StageFailure(f'Staging qualification failed; inspect {log}', log=log) from error
            attempt.update(staged=True, staging_seconds=seconds, phase='staged')
            self.record()
            self.notice('staging', f'PASS in {seconds}s: {head}', [r.number for r in included])
        self.record(phase='batch-push')
        self.push_new(root, head, batch)
        self.record(phase='ci')
        try:
            url = github.wait_gate(root, self.options.project, batch, head, self.options.timeout)
        except github.GateFailure as error:
            attempt.update(phase='failed', failure=dict(kind='gate', message=str(error), run=error.run,
                                                        attempt=error.attempt))
            self.record()
            raise
        attempt.update(phase='qualified', gate=url)
        self.record(phase='qualified', gate=url)
        self.report_flakes(root, url)
        self.notice('gate', f'PASS: {url}', [r.number for r in included])
        return dict(root=str(root), head=head, rows=attempt['included'], gate=url, branch=batch)

    def metadata(self, root):
        metadata = json.loads(compose.command(root, 'cargo', 'metadata', '--locked', '--no-deps', '--format-version=1').stdout)
        members = set(metadata['workspace_members'])
        return [p for p in metadata['packages'] if p['id'] in members]

    def report_flakes(self, root, gate):
        policy = batch_report.quarantine(root)
        if not policy:
            return
        path = self.options.state / f'{self.state["active"]["name"]}.passed.zip'
        if path.exists():
            raw = batch_report.unpack(path, head=gate.head, base=self.state['base'],
                                      run=gate.run, attempt=gate.attempt)
        else:
            raw = github.failure_report(root, self.options.project, gate, path, self.state['base'])
        flaky = batch_report.flakies(raw, {p['name'] for p in self.metadata(root)})
        if any(f.test not in policy for f in flaky):
            raise ValueError('Retried test is not in the validated quarantine')
        if flaky:
            labels = [f'{f.binary}::{f.test}' for f in flaky]
            self.record(flaky=labels, flaky_report=str(path))
            self.notice('flake', '; '.join(labels),
                        extra=batch_notify.owners({policy[f.test]['owner'] for f in flaky}))

    def assessment(self, failure, rows):
        root = Path(self.state['active']['root'])
        try:
            if isinstance(failure, StageFailure):
                raw = batch_report.bounded(Path(str(failure.log) + '.nextest.xml'))
            else:
                path = self.options.state / f'{self.state["active"]["name"]}.failure.zip'
                if path.exists():
                    batch_report.bounded(path)
                    raw = batch_report.unpack(path, head=failure.head, base=self.state['base'],
                                              run=failure.run, attempt=failure.attempt)
                else:
                    raw = github.failure_report(root, self.options.project, failure, path, self.state['base'])
            packages = self.metadata(root)
            failed = batch_report.failures(raw, {p['name'] for p in packages})
            included = {row['number'] for row in self.state['active']['included']}
            changes = {r.number: batch_report.touched(root, self.state['base'], r.head, packages)
                       for r in rows if r.number in included}
            # The exact quarantine contract is supplied by the checked source policy.
            policy = batch_report.quarantine(root)
            quarantined = [f for f in failed if f.test in policy]
            result = batch_report.attribute(failed, changes, quarantined)
            if result['kind'] == 'quarantine-outside':
                result['owners'] = sorted({policy[f.test]['owner'] for f in quarantined
                                          if not any(f.package in crates for crates in changes.values())})
        except (OSError, ValueError, KeyError, TypeError, zipfile.BadZipFile, subprocess.SubprocessError) as error:
            result = dict(kind='ambiguous', excluded=[], reason=str(error)[:1024])
        self.record(attribution=result)
        self.notice('failure', f'{failure}; attribution: {result["kind"]}; {result.get("tests", [])}',
                    extra=batch_notify.owners(result['owners']) if result.get('owners') else ())
        return result

    def prefix(self, rows):
        prefix = self.state.get('prefix')
        if prefix is None:
            try:
                green = self.attempt_gate([])
            except (StageFailure, github.GateFailure) as error:
                self.assessment(error, [])
                self.record(phase='base-failed')
                raise RuntimeError('Base gate failed; no PR can be attributed or excluded') from error
            prefix = dict(accepted=[], green=green)
            self.record(prefix=prefix)
        accepted = [github.PullRequest(**row) for row in prefix['accepted']]
        excluded = {item['pr'] for item in self.state['excluded']}
        for row in rows:
            if row.number in excluded or row in accepted:
                continue
            try:
                candidate = self.attempt_gate([*accepted, row])
            except (StageFailure, github.GateFailure) as error:
                result = self.assessment(error, [*accepted, row])
                if result['kind'] == 'quarantine-outside':
                    self.record(phase='quarantine-outside')
                    raise RuntimeError('Quarantined failure outside touched crates; no prefix bisection') from error
                self.exclude(row.number, f'Fails when added to a green prefix: {error}')
            else:
                accepted = [github.PullRequest(**r) for r in candidate['rows']]
                prefix.update(accepted=candidate['rows'], green=candidate)
                self.record(prefix=prefix)
        return prefix['green']

    def run(self, rows):
        if self.state.get('publication'):
            return self.publish(self.state['publication'])
        if self.state.get('prefix'):
            result = self.prefix(rows)
        else:
            while True:
                excluded = {item['pr'] for item in self.state['excluded']}
                remaining = [r for r in rows if r.number not in excluded]
                if not remaining:
                    raise ValueError('No qualified pull requests remain; main was not moved')
                try:
                    result = self.attempt_gate(remaining)
                    break
                except (StageFailure, github.GateFailure) as error:
                    attribution = self.assessment(error, remaining)
                    if attribution['kind'] == 'quarantine-outside':
                        self.record(phase='quarantine-outside')
                        raise RuntimeError('Quarantined failure outside touched crates; no prefix bisection') from error
                    if attribution['kind'] == 'unique':
                        for number in attribution['excluded']:
                            self.exclude(number, 'Failed test in the only PR touching its crate')
                        continue
                    result = self.prefix(remaining)
                    break
        if not result['rows']:
            raise ValueError('No qualified pull requests remain; main was not moved')
        self.record(publication=result, phase='main-push')
        return self.publish(result)

    def publish(self, result):
        root, head = Path(result['root']), result['head']
        rows = [github.PullRequest(**r) for r in result['rows']]
        if not self.state.get('landed'):
            observed = compose.git(root, 'ls-remote', '--heads', 'github', 'refs/heads/main').split()[0]
            if observed == self.state['base']:
                github.wait_gate(root, self.options.project, result['branch'], head, self.options.timeout)
                for row in rows:
                    if github.admission(root, self.options.project, row.number, self.state['base']) != row:
                        raise ValueError('Pull request head or review changed after qualification')
                compose.git(root, 'merge-base', '--is-ancestor', self.state['base'], head)
                compose.git(root, 'push', 'github', head + ':refs/heads/main')
                observed = compose.git(root, 'ls-remote', '--heads', 'github', 'refs/heads/main').split()[0]
            if observed != head:
                # Publication may have succeeded before the last local write,
                # and a later controller may already have advanced main again.
                compose.git(root, 'fetch', '--no-tags', 'github', 'main')
                current = compose.git(root, 'rev-parse', 'FETCH_HEAD')
                proof = compose.command(root, 'git', 'merge-base', '--is-ancestor', head, current, check=False)
                if proof.returncode:
                    raise ValueError('Main moved or publication is unconfirmed; preserve the checkpoint')
            self.record(landed=head, phase='reconcile')
        elif self.state['landed'] != head:
            raise ValueError('Publication checkpoint names a different landed commit')
        self.notice('publication', f'Landed {head}', [r.number for r in rows])
        receipts = self.state.setdefault('pull_requests', {})
        for row in rows:
            if str(row.number) not in receipts:
                receipts[str(row.number)] = github.reconcile(root, self.options.project, row, head)
                self.record()
        self.notice('reconciliation', f'Reconciled included pull requests at {head}', [r.number for r in rows])
        if not self.state.get('mirrored'):
            self.record(phase='mirror')
            mirror(root, self.options.mirror_host, self.options.mirror_path, head)
            self.record(mirrored=True)
            self.notice('mirror', f'Updated mirror through {head}', [r.number for r in rows])
        self.record(phase='notifications')
        complete = batch_notify.flush(self.state, self.record)
        self.record(phase='complete' if complete else 'notifications')
        print('Landed ' + head + ('' if complete else '; notification delivery pending'), flush=True)
