"""Select the existing affected gate; unresolved inputs run the full workspace."""
import json
import os
from pathlib import Path
import subprocess
import sys
import urllib.request

ROOT = Path(__file__).resolve().parents[2]
WORKFLOW = ".github/workflows/ci.yml"
sys.path.insert(0, str(ROOT / "scripts/build"))
import gate_plan
import gate_policy


def git(*args):
    return subprocess.check_output(["git", *args], cwd=ROOT, timeout=45)


def changed_paths(event):
    base = event["pull_request"]["base"]["sha"]
    head = event["pull_request"]["head"]["sha"]
    base = git("merge-base", base, head).decode().strip()
    return base, set(filter(None, git("diff", "--name-only", "--no-renames", "-z", base, head).decode().split("\0")))


def batch_paths():
    """A batch is diffed against main, the commit its gate composes onto."""
    base = git("merge-base", "origin/main", "HEAD").decode().strip()
    return base, set(filter(None, git("diff", "--name-only", "--no-renames", "-z", base, "HEAD").decode().split("\0")))


def human_document(path):
    return (path in {"README.md", "THIRD_PARTY_NOTICES.md"} or
            path.startswith("docs/") and path.endswith(".md") or
            path.startswith("notices/") and path.endswith((".md", ".txt")))


def referenced(documents):
    """Whether any tracked source names one of these documents."""
    sources = git("ls-files", "-z", "*.rs", "*.toml", "*.py", "*.sh", "*.ps1",
                  "*.js", "*.jsx", "*.ts", "*.tsx").decode().split("\0")
    names = {Path(p).name for p in documents}
    for source in sources:
        if not source or (ROOT / source).resolve() == Path(__file__).resolve():
            continue
        text = (ROOT / source).read_text(encoding="utf-8")
        if any(name in text for name in names):
            return True
    return False


def scope(event, kind, diff=None):
    if diff is None and kind not in {"pull_request", "pull_request_target"}:
        return ["*"]
    base, paths = changed_paths(event) if diff is None else diff
    if not paths:
        return ["*"]
    # Human documents and legal fragments still receive the mandatory license
    # checks. A referenced input or any unknown path keeps compilation.
    documents = {p for p in paths if human_document(p)}
    if documents and not referenced(documents):
        if documents == paths:
            return []
        # Unreferenced documents beside code don't widen the code's scope.
        paths = paths - documents
    if gate_policy.required_mode(paths, root=ROOT, base=base) == "full":
        return ["*"]
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version=1"], cwd=ROOT, timeout=60))
    # Unowned paths (including changed/deleted manifests) are never an exemption.
    packages = metadata["packages"]
    directories = [Path(p["manifest_path"]).parent.relative_to(ROOT).as_posix() + "/" for p in packages]
    if any(not any(p.startswith(directory) for directory in directories) for p in paths):
        return ["*"]
    selected = gate_plan.select(packages, paths, ROOT)
    return sorted(set(selected) | {"grapt-types", "grapt-store"}) if selected else ["*"]


def matrix(event, kind):
    """Hosted minutes are explicit Windows dispatches and nightly/opt-in macOS."""
    platforms = []
    inputs = event.get("inputs", {})
    if kind == "workflow_dispatch" and inputs.get("windows") in (True, "true"):
        platforms.append({"os": "windows-2025", "platform": "windows"})
    if kind == "schedule" or (kind == "workflow_dispatch" and inputs.get("macos") in (True, "true")):
        platforms.append({"os": "macos-15", "platform": "macos"})
    return {"include": platforms}


def proven_batch(kind):
    """The passing batch run whose exact commit main now points at, if any."""
    if kind != "push" or os.environ.get("GITHUB_REF") != "refs/heads/main":
        return ""
    sha = os.environ["GITHUB_SHA"]
    api = os.environ.get("GITHUB_API_URL", "https://api.github.com")
    url = (f"{api}/repos/{os.environ['GITHUB_REPOSITORY']}/actions/runs"
           f"?head_sha={sha}&event=push&status=success&per_page=100")
    request = urllib.request.Request(url, headers={
        "Accept": "application/vnd.github+json",
        "Authorization": f"Bearer {os.environ['GH_TOKEN']}"})
    with urllib.request.urlopen(request, timeout=30) as response:
        runs = json.load(response)["workflow_runs"]
    for run in runs:
        if (run.get("head_sha") == sha and run.get("conclusion") == "success"
                and run.get("path") == WORKFLOW
                and str(run.get("head_branch", "")).startswith("batch/")):
            return run["html_url"]
    return ""


def main():
    kind = os.environ.get("GITHUB_EVENT_NAME", "unknown")
    try:
        proven = proven_batch(kind)
    except (OSError, ValueError, KeyError, TypeError) as error:
        # An unreadable verdict never skips a gate.
        print(f"Batch verdict unavailable; gating main normally: {error}")
        proven = ""
    if proven:
        print(f"Main already passed this exact commit's batch gate: {proven}")
        with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
            for key, value in {"proven": "true", "rust": "false", "packages": "[]", "mode": "full",
                               "precheck": "false", "base": "", "matrix": '{"include": []}',
                               "windows": "false", "hosted": "false",
                               "clippy-matrix": '{"include": []}'}.items():
                output.write(f"{key}={value}\n")
        return
    # A missing event cannot authorize skipping an explicitly requested host.
    event = json.loads(Path(os.environ["GITHUB_EVENT_PATH"]).read_text(encoding="utf-8"))
    platforms = matrix(event, kind)
    mode = "full"
    precheck = False
    batch = kind == "push" and os.environ.get("GITHUB_REF", "").startswith("refs/heads/batch/")
    base = ""
    if batch:
        # A batch is gated against main, so its scans and Windows checks see every landed commit.
        # Failing here fails the run rather than gating the batch against itself.
        base = git("merge-base", "origin/main", "HEAD").decode().strip()
    try:
        diff = changed_paths(event) if kind in {"pull_request", "pull_request_target"} else None
        if diff is None and batch:
            diff = batch_paths()
        packages = scope(event, kind, diff)
        if kind in {"pull_request", "pull_request_target"}:
            # Pull requests precheck affected crates; their batch is the landing gate.
            mode, precheck = "affected", True
        elif batch and packages and packages != ["*"]:
            # A batch lands on the affected gate; core crates, build tooling and
            # release paths widen the scope to the workspace and keep it full.
            mode = "affected"
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        print(f"Change detection unavailable; running all ordinary checks: {error}")
        mode, precheck, packages = "full", False, ["*"]
    windows = any(row["platform"] == "windows" for row in platforms["include"])
    values = {"proven": "false", "rust": str(bool(packages)).lower(), "packages": json.dumps(packages), "mode": mode,
              "precheck": str(precheck).lower(), "base": base,
              "matrix": json.dumps(platforms), "windows": str(windows).lower(),
              "hosted": str(bool(platforms["include"])).lower(),
              "clippy-matrix": json.dumps({"include": [row for row in platforms["include"]
                                                       if row["platform"] == "windows"]})}
    with open(os.environ["GITHUB_OUTPUT"], "a", encoding="utf-8") as output:
        for key, value in values.items():
            output.write(f"{key}={value}\n")
    print(json.dumps(values))


if __name__ == "__main__":
    main()
