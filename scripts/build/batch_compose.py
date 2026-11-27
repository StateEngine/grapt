"""Compose pull requests while preserving reviewed source and legal text."""
from contextlib import chdir
import importlib.util
import json
from pathlib import Path
import re
import tempfile

import heavy
import publication_scan

NOTICE = 'licenses/dependencies/NOTICE.html'
DIGEST = re.compile(r'<!-- Dependency inputs SHA-256: [0-9a-f]{64} -->')
TRAILER = 'Co-Authored-By: Codex <noreply@openai.com>'


def command(root, *args, check=True):
    return heavy.run(list(args), cwd=root, capture_output=True, text=True,
                     check=check, timeout=120)


def git(root, *args):
    return command(root, 'git', *args).stdout.strip()


def scan(root, base, head='HEAD'):
    with chdir(root):
        return publication_scan.check(base, head)


def resolve(root):
    paths = git(root, 'diff', '--name-only', '--diff-filter=U', '-z').split('\0')
    paths = [path for path in paths if path]
    if set(paths) - {NOTICE}:
        raise ValueError('Unsupported merge conflicts: ' + ', '.join(paths))
    for path in paths:
        versions = [command(root, 'git', 'show', f':{stage}:{path}').stdout for stage in (1, 2, 3)]
        # Only the generated input digest may differ. Never choose a side
        # when the dependency register or license text changed as well.
        normalized = [DIGEST.sub('<!-- dependency inputs -->', v) for v in versions]
        if len(set(normalized)) != 1 or any(len(DIGEST.findall(v)) != 1 for v in versions):
            raise ValueError('License text conflict requires register regeneration and review')
        (root / path).write_text(versions[1], encoding='utf-8')
        git(root, 'add', '--', path)
    if (root / NOTICE).is_file():
        module = root / 'scripts/release/licenses.py'
        spec = importlib.util.spec_from_file_location('batch_dependency_licenses', module)
        licenses = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(licenses)
        path = root / NOTICE
        contents = path.read_text(encoding='utf-8')
        if len(DIGEST.findall(contents)) != 1:
            raise ValueError('Dependency register must have exactly one input digest')
        contents = DIGEST.sub(f'<!-- Dependency inputs SHA-256: {licenses.dependency_digest(root)} -->', contents)
        path.write_text(contents, encoding='utf-8')
        git(root, 'add', '--', NOTICE)


def reviewed_head(root, accepted, head, main):
    """A review survives only mechanical merges of accepted main ancestry."""
    ancestry = command(root, 'git', 'merge-base', '--is-ancestor', accepted, head, check=False)
    if ancestry.returncode:
        raise ValueError('ACCEPT does not name an ancestor of the current head')
    commits = git(root, 'rev-list', '--first-parent', f'{accepted}..{head}').split()
    for commit in commits:
        parents = git(root, 'show', '-s', '--format=%P', commit).split()
        if len(parents) != 2:
            raise ValueError('Head changed after ACCEPT with non-merge commits')
        ancestry = command(root, 'git', 'merge-base', '--is-ancestor', parents[1], main, check=False)
        if ancestry.returncode:
            raise ValueError('Head changed after ACCEPT by merging a branch outside main')
        merged = command(root, 'git', 'merge-tree', '--write-tree', *parents, check=False)
        if merged.returncode or merged.stdout.strip() != git(root, 'rev-parse', commit + '^{tree}'):
            raise ValueError('Head changed after ACCEPT with manual merge edits')


def merge(root, number, head, main):
    """Squash the whole PR when its history cannot be published."""
    before = git(root, 'rev-parse', 'HEAD')
    base = git(root, 'merge-base', main, head)
    squash = bool(scan(root, base, head))
    result = command(root, 'git', '-c', 'core.hooksPath=/dev/null',
                     '-c', 'rerere.enabled=false', 'merge',
                     '--squash' if squash else '--no-ff', '--no-commit', head, check=False)
    if result.returncode and not git(root, 'diff', '--name-only', '--diff-filter=U'):
        raise ValueError(result.stderr or result.stdout)
    resolve(root)
    # A message file and strip cleanup omit Git's generated conflict appendix.
    with tempfile.TemporaryDirectory(prefix='batch-message-') as temporary:
        message = Path(temporary) / 'message'
        body = (f'Lands pull request {number}.\n\nCo-Authored-By: Claude <noreply@anthropic.com>'
                if squash else TRAILER)
        message.write_text(f'merge(batch): land pull request {number}\n\n{body}\n', encoding='utf-8')
        git(root, '-c', 'core.hooksPath=/dev/null', 'commit', '-F', str(message), '--cleanup=strip')
    violations = scan(root, before)
    if violations:
        raise ValueError('\n'.join(violations))
    return git(root, 'rev-parse', 'HEAD')


def discard_merge(root, before):
    """Only the new, owned composition checkout may be rolled back."""
    # No shared or pushed branch is reset; callers create a detached worktree.
    git(root, 'reset', '--hard', before)
