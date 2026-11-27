"""Git pre-push hook body: run the publication scan over the outgoing commits.

Reads `<local ref> <local sha> <remote ref> <remote sha>` lines from stdin and
exits non-zero when any pushed ref has findings. Only new commits are scanned.
"""
import os
import subprocess
import sys

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import publication_scan  # noqa: E402


def _git(*args):
    result = subprocess.run(["git", *args], capture_output=True, text=True)
    return result.stdout.strip().split() if result.returncode == 0 else []


def _is_zero(sha):
    return not sha.strip("0")


def _has_commit(sha):
    return subprocess.run(["git", "cat-file", "-e", sha + "^{commit}"],
                          capture_output=True).returncode == 0


def outgoing(sha, remote_sha):
    """The exact commits this push publishes, or None when no base is known."""
    if not _is_zero(remote_sha) and _has_commit(remote_sha):
        # The remote's own tip is authoritative; local tracking refs may be stale.
        return _git("rev-list", sha, "--not", remote_sha)
    if not _git("for-each-ref", "--count=1", "--format=%(refname)", "refs/remotes"):
        return None
    return _git("rev-list", sha, "--not", "--remotes")


def scan_push(lines):
    findings = []
    for line in lines:
        fields = line.split()
        if len(fields) != 4 or _is_zero(fields[1]):
            continue
        shas = outgoing(fields[1], fields[3])
        if shas is None:
            print(f"no publication scan base for {fields[2]}; skipping "
                  "(CI scan is the backstop)", file=sys.stderr)
            continue
        findings.extend(publication_scan.check_commits(shas))
    return findings


def main():
    findings = scan_push(sys.stdin.read().splitlines())
    if findings:
        print("pre-push: the publication scan rejected this push:", file=sys.stderr)
        for finding in findings:
            print(finding, file=sys.stderr)
        print("Fix the commit messages (see AGENTS.md), then push again.", file=sys.stderr)
    sys.exit(1 if findings else 0)


if __name__ == "__main__":
    main()
