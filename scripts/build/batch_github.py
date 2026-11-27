"""Read review verdicts and wait for an exact batch workflow result."""
from dataclasses import dataclass
import json
import re
import time
from pathlib import Path
import tempfile

from batch_compose import command, git, reviewed_head
import heavy
import batch_report


@dataclass(frozen=True)
class PullRequest:
    number: int
    head: str
    accepted: str


def api(root, repository, suffix):
    result = command(root, 'gh', 'api', '--paginate', '--slurp', f'repos/{repository}/{suffix}')
    pages = json.loads(result.stdout)
    if not pages:
        raise ValueError('GitHub returned no response')
    if isinstance(pages[0], list):
        return [item for page in pages for item in page]
    for key in ('workflow_runs', 'jobs', 'artifacts'):
        if key in pages[0]:
            return {**pages[0], key: [item for page in pages for item in page[key]]}
    return pages[0]


def write(root, repository, suffix, method, value):
    with tempfile.TemporaryDirectory(prefix='batch-request-') as temporary:
        path = Path(temporary) / 'body.json'
        path.write_text(json.dumps(value), encoding='utf-8')
        result = command(root, 'gh', 'api', '--method', method, '--input', str(path),
                         f'repos/{repository}/{suffix}')
    return json.loads(result.stdout)


def reconcile(root, repository, row, landed):
    """A retry discovers remote success instead of repeating a comment or close."""
    pull = api(root, repository, f'pulls/{row.number}')
    if pull.get('merged') or pull['state'] == 'closed':
        return 'merged' if pull.get('merged') else 'closed'
    if pull['head']['sha'] != row.head:
        raise ValueError(f'Pull request {row.number} moved after landing; preserve it for review')
    marker = f'<!-- batch-land:{landed}:pr:{row.number} -->'
    comments = api(root, repository, f'issues/{row.number}/comments?per_page=100')
    body = f'Landed in `{landed}`.\n\n{marker}'
    if not any(item.get('body') == body for item in comments):
        write(root, repository, f'issues/{row.number}/comments', 'POST', dict(body=body))
        comments = api(root, repository, f'issues/{row.number}/comments?per_page=100')
        if not any(item.get('body') == body for item in comments):
            raise ValueError('Landing comment delivery is unconfirmed')
    # Recheck immediately before closure: never close an unlanded correction.
    current = api(root, repository, f'pulls/{row.number}')
    if current['state'] == 'open' and current['head']['sha'] != row.head:
        raise ValueError('Pull request changed while recording its landing')
    if current['state'] == 'open':
        write(root, repository, f'issues/{row.number}', 'PATCH', dict(state='closed'))
    if api(root, repository, f'pulls/{row.number}')['state'] != 'closed':
        raise ValueError('Landed pull request closure is unconfirmed')
    return 'closed'


def verdict(review):
    """Read an explicit opening verdict; quoted or ambiguous prose cannot grant ACCEPT."""
    if review['state'] == 'PENDING':
        return None
    if review['state'] in {'DISMISSED', 'CHANGES_REQUESTED'}:
        return 'block'
    body = review['body'].strip()
    headline = re.split(r'\n\s*\n', body, maxsplit=1)[0]
    headline = re.sub(r'[*_`]', '', headline)
    blocking = r'\b(?:REQUEST(?:ED)?\s+CHANGES|CHANGES\s+REQUESTED|RETURN(?:ED)?|REJECT(?:ED)?)\b'
    if re.search(blocking, headline, re.I):
        return 'block'
    if re.search(r'\bACCEPT\b', headline):
        count = r'\d+\s+(?:Important|Nits?|Pre-existing)'
        prefix = rf'(?:(?:{count})(?:\s*,\s*{count})*[.;]\s*)?'
        explicit = re.match(prefix + r'(?:Verdict:\s*)?ACCEPT'
                            r'(?=$|[.!;]|\s+(?:at|on)\b|\s+carries over to\b)', headline)
        uncertain = re.search(r'\b(?:withdrawn|revoked|conditional|pending)\b|ACCEPT\s*\?', headline, re.I)
        return 'accept' if explicit and not uncertain else 'block'
    # A verdict outside the opening paragraph needs an explicit replacement,
    # rather than allowing an earlier acceptance to survive ambiguous prose.
    if re.search(blocking + r'|\bACCEPT\b', body, re.I):
        return 'block'
    return None


def admission(root, repository, number, main):
    row = api(root, repository, f'pulls/{number}')
    if (row['state'] != 'open' or row['draft'] or row['base']['ref'] != 'main'
            or row['head']['repo']['full_name'] != repository):
        raise ValueError('Require an open, ready same-repository pull request targeting main')
    head = row['head']['sha']
    command(root, 'git', 'fetch', '--no-tags', 'github', f'refs/pull/{number}/head')
    if git(root, 'rev-parse', 'FETCH_HEAD') != head:
        raise ValueError('Pull request head moved during admission; retry with a fresh review')
    reviews = api(root, repository, f'pulls/{number}/reviews?per_page=100')
    verdicts = [review for review in reviews if verdict(review) is not None]
    verdicts.sort(key=lambda review: (review['submitted_at'], review['id']))
    if not verdicts or verdict(verdicts[-1]) != 'accept':
        raise ValueError('Latest review body has no ACCEPT verdict')
    review = verdicts[-1]
    reviewed_head(root, review['commit_id'], head, main)
    return PullRequest(number, head, review['commit_id'])


class GateFailure(RuntimeError):
    """A completed gate failed; this alone does not identify a culprit."""
    def __init__(self, message, *, run=None, head=None, branch=None, attempt=1):
        super().__init__(message)
        self.run, self.head, self.branch = run, head, branch
        self.attempt = attempt


class GateSuccess(str):
    def __new__(cls, url, run, head, branch, attempt):
        result = super().__new__(cls, url)
        result.run, result.head, result.branch, result.attempt = run, head, branch, attempt
        return result


def failure_report(root, repository, failure, destination, base):
    if failure.run is None:
        raise ValueError('Failed gate has no exact run identity')
    artifacts = api(root, repository, f'actions/runs/{failure.run}/artifacts?per_page=100')
    found = [row for row in artifacts['artifacts'] if row['name'] == batch_report.ARTIFACT and not row['expired']]
    if len(found) != 1 or not 0 < found[0]['size_in_bytes'] <= batch_report.MAX_BYTES:
        raise ValueError('Failed gate has no unique bounded artifact')
    with tempfile.NamedTemporaryFile(dir=destination.parent, prefix=destination.name, suffix='.part', delete=False) as output:
        temporary = Path(output.name)
        heavy.run(['gh', 'api', f'repos/{repository}/actions/artifacts/{found[0]["id"]}/zip'],
                  cwd=root, stdout=output, check=True, timeout=120)
    batch_report.bounded(temporary)
    raw = batch_report.unpack(temporary, head=failure.head, base=base, run=failure.run, attempt=failure.attempt)
    temporary.replace(destination)
    return raw


def wait_gate(root, repository, branch, head, timeout):
    deadline = time.monotonic() + timeout
    run_id = None
    while time.monotonic() < deadline:
        if run_id is None:
            runs = api(root, repository, f'actions/workflows/ci.yml/runs?branch={branch}&event=push&per_page=100')
            exact = [row for row in runs['workflow_runs'] if row['head_sha'] == head
                     and row['head_branch'] == branch and row['event'] == 'push']
            if len(exact) > 1:
                raise ValueError('Multiple workflow runs found for the immutable batch ref')
            if exact:
                run_id = exact[0]['id']
        if run_id is not None:
            row = api(root, repository, f'actions/runs/{run_id}')
            if row['head_sha'] != head or row['head_branch'] != branch:
                raise ValueError('CI workflow source changed')
            if row['status'] == 'completed':
                jobs = api(root, repository, f'actions/runs/{run_id}/jobs?per_page=100')
                required = {name: [j for j in jobs['jobs'] if j['name'] == name]
                            for name in ('changes', 'linux', 'check')}
                if row['conclusion'] == 'failure':
                    raise GateFailure(f'Batch gate failed: {row["html_url"]}', run=run_id,
                                      head=head, branch=branch, attempt=row.get('run_attempt', 1))
                if row['conclusion'] != 'success' or any(
                        len(items) != 1 or items[0]['conclusion'] != 'success' for items in required.values()):
                    raise RuntimeError(f'Batch gate is incomplete or cancelled: {row["html_url"]}')
                return GateSuccess(row['html_url'], run_id, head, branch, row.get('run_attempt', 1))
        time.sleep(min(30, max(0, deadline - time.monotonic())))
    raise TimeoutError('Batch CI did not finish; preserve the batch ref and inspect its queued runner')
