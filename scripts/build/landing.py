"""Qualify canonical composition without relabelling an old gate."""
from operator_paths import worktrees
import base64
from contextlib import contextmanager, nullcontext
import hashlib
import importlib
import inspect
import json
import os
import math
from pathlib import Path
import re
import signal
import stat
import subprocess
import sys
import tempfile
import time
import heavy
import gate_policy
import reviewed_composition
import full_capacity

GENERATED_REGISTER = "licenses/dependencies/NOTICE.html"
# Historical: THIRD_PARTY_NOTICES.md was the one hand-edited document Git could merge.
# It is now generated from notices/ fragments and untracked, so a candidate based on an
# older tree still takes this path and refuses by name once canonical has deleted it.
MERGEABLE_DOCUMENTS = frozenset({"THIRD_PARTY_NOTICES.md"})


def git(root, *arguments):
    value = heavy.check_output(
        ["git", "--no-optional-locks", "-C", str(root), *arguments], timeout=60)
    if len(value) > 1024 * 1024:
        raise ValueError("Landing Git output exceeds 1 MiB")
    return value


def source(root, commit, branch):
    if (git(root, "rev-parse", "HEAD").decode().strip() != commit or
            git(root, "branch", "--show-current").decode().strip() != branch or
            git(root, "status", "--porcelain", "--untracked-files=all")):
        raise ValueError("Landing needs the exact clean owning branch")
    return {"commit": commit, "clean": True}


def paths(root, base, commit):
    return {p.decode() for p in git(root, "diff", "--no-renames", "--name-only",
                                  "-z", base, commit, "--").split(b"\0") if p}


def raw_diff(root, base, commit, generated=False, documents=()):
    excluded = set(documents) | ({GENERATED_REGISTER} if generated else set())
    scope = [".", *(f":(exclude){path}" for path in sorted(excluded))] if excluded else []
    return git(root, "diff", "--raw", "--no-abbrev", "--no-renames", "-z", base, commit, "--", *scope)


def assert_disjoint(owned, advanced):
    generated = False
    for left in sorted(owned):
        for right in sorted(advanced):
            a, b = left.casefold(), right.casefold()
            if a == b or a.startswith(b + "/") or b.startswith(a + "/"):
                if left == right == GENERATED_REGISTER:
                    generated = True
                    continue
                if left == right and left in MERGEABLE_DOCUMENTS:
                    continue
                raise ValueError(f"Canonical advance overlaps candidate path: {left}")
    return generated


def document_blob(root, commit, path):
    entry = git(root, "ls-tree", "-z", commit, "--", path)
    if not entry.startswith(b"100644 blob ") or entry.count(b"\0") != 1:
        raise ValueError(f"Composition needs a regular document at every base: {path}")
    object_id = entry.split(b"\t", 1)[0].split()[-1].decode("ascii")
    if int(git(root, "cat-file", "-s", object_id)) > 1024 * 1024:
        raise ValueError(f"Composition document exceeds 1 MiB: {path}")
    value = git(root, "cat-file", "blob", object_id)
    if b"\0" in value:
        raise ValueError(f"Composition document is binary: {path}")
    try:
        value.decode("utf-8")
    except UnicodeDecodeError as error:
        raise ValueError(f"Composition document is not UTF-8: {path}") from error
    return value


def merged_documents(root, base, accepted, advanced, owned):
    overlap = MERGEABLE_DOCUMENTS & owned & paths(root, base, advanced)
    merged = {}
    for path in sorted(overlap):
        with tempfile.TemporaryDirectory(prefix="secode-land-document-") as directory:
            files = [Path(directory) / name for name in ("advanced", "base", "accepted")]
            for file, revision in zip(files, (advanced, base, accepted)):
                file.write_bytes(document_blob(root, revision, path))
            # merge-file ignores repository merge drivers and compares only these
            # bounded text inputs. Conflicts refuse before the owning branch moves.
            result = heavy.run(["git", "merge-file", "--stdout", *map(str, files)],
                               capture_output=True, timeout=60)
        if 1 <= result.returncode <= 127:
            raise ValueError(f"Document composition has conflicting hunks: {path}")
        if result.returncode:
            raise ValueError(f"Document composition failed: {path}")
        if len(result.stdout) > 1024 * 1024:
            raise ValueError(f"Composed document exceeds 1 MiB: {path}")
        merged[path] = result.stdout
    return merged


def verify_documents(root, candidate, documents):
    for path, expected in documents.items():
        if document_blob(root, candidate, path) != expected:
            raise ValueError(f"Rebase changed the approved document merge: {path}")


def rebase(root, advanced, generated):
    if not generated:
        heavy.run(["git", "rebase", advanced], cwd=root, check=True, timeout=120)
        return
    deadline = time.monotonic() + 120
    def run(command, check=False):
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise RuntimeError("Generated-register rebase exceeded its time limit")
        return heavy.run(command, cwd=root, check=check, timeout=remaining)
    command = ["git", "rebase", advanced]
    for _ in range(256):
        result = run(command)
        if result.returncode == 0:
            return
        conflicts = git(root, "diff", "--name-only", "--diff-filter=U", "-z").split(b"\0")
        if conflicts != [GENERATED_REGISTER.encode(), b""]:
            names = ", ".join(p.decode() for p in conflicts if p)
            raise ValueError(f"Rebase conflicts outside the generated license register: {names}")
        # 'ours' during rebase is the composed base. The full generator replaces
        # this derived file after every reviewed source commit has been replayed.
        run(["git", "checkout", "--ours", "--", GENERATED_REGISTER], check=True)
        run(["git", "add", "--", GENERATED_REGISTER], check=True)
        command = ["git", "-c", "core.editor=true", "rebase", "--continue"]
    raise ValueError("License-register composition exceeds 256 rebase conflicts")


def register_proof(root, receipt_path):
    return {"path": GENERATED_REGISTER,
            "sha256": digest(root / GENERATED_REGISTER, 1024 * 1024),
            "log_sha256": digest(receipt_path.with_suffix(".licenses.log"))}


def regenerate_register(root, receipt_path, lease_fd):
    sys.path.insert(0, str(root / "scripts/build"))
    import gate
    importlib.reload(gate)
    deadline = time.monotonic() + 1200
    with receipt_path.with_suffix(".licenses.log").open("x", encoding="utf-8") as log:
        for mode in ("generate", "check"):
            command = [sys.executable, "scripts/release/licenses.py", mode]
            log.write("# command: " + json.dumps(command) + "\n")
            log.flush()
            gate.execute(command, log, deadline, cwd=root, lease_fd=lease_fd)
    changed = {p.decode() for p in git(root, "diff", "--name-only", "-z", "HEAD").split(b"\0") if p}
    if changed - {GENERATED_REGISTER} or git(root, "ls-files", "--others", "--exclude-standard", "-z"):
        raise ValueError("License regeneration changed a non-generated source path")
    if changed:
        heavy.run(["git", "commit", "-F", "-", "--", GENERATED_REGISTER], cwd=root,
                       input=(b"build(release): regenerate dependency licenses\n\n"
                              b"Co-Authored-By: secode-seats <353560407+secode-seats[bot]@users.noreply.github.com>\n"),
                       check=True, timeout=60)
    return register_proof(root, receipt_path)


def original_gate(root, request):
    path = Path(request["gate_log"])
    before = digest(path.with_suffix(path.suffix + ".receipt.json"), 65536)
    command = [sys.executable, "scripts/build/receipt.py", request["gate_log"],
               request["accepted"], "--base", request["old_base"]]
    if request.get('retained_full'):
        for key in ('commit', 'base', 'log'):
            command.extend(['--retained-' + key, request['retained_full'][key]])
    elif request["full"]:
        command.append("--require-full")
    # A receipt says whether its gate ran in batch mode; verification must ask
    # for that same policy or refuse the mismatch.
    if receipt_record(path).get('batch') is True:
        command.append("--batch")
    heavy.run(command, cwd=root, check=True, timeout=120)
    after = digest(path.with_suffix(path.suffix + ".receipt.json"), 65536)
    if before != after:
        raise ValueError("Original gate receipt changed during verification")
    return after


def receipt_record(path):
    """The batch flag alone, read without trusting other fields.

    verify() re-checks every field it relies on; this only chooses which policy
    to ask it for. An unreadable record is left to verify() to refuse.
    """
    record = path.with_suffix(path.suffix + ".receipt.json")
    try:
        return json.loads(record.read_text())
    except (OSError, ValueError):
        return {}


def commands(root, owned, base, mode="composition"):
    # Import policy only after rebasing; the original gate is verified in its
    # own subprocess, so an older checkout cannot cache the new base's policy.
    sys.path.insert(0, str(root / "scripts/build"))
    # A live original gate may have imported policy from the pre-rebase tree.
    for name in ("test_features", "windows_gate", "gate_plan"):
        if name in sys.modules:
            importlib.reload(sys.modules[name])
    import gate_plan
    metadata = heavy.check_output(
        ["cargo", "metadata", "--locked", "--no-deps", "--format-version", "1"],
        cwd=root, timeout=60)
    if len(metadata) > 8 * 1024 * 1024:
        raise ValueError("Workspace metadata exceeds 8 MiB")
    metadata = json.loads(metadata)
    members = set(metadata["workspace_members"])
    packages = [p for p in metadata["packages"] if p["id"] in members]
    return gate_plan.commands(mode, packages, owned, root, base=base)


def gate_invocation(root, gate, mode, base, timeout, log):
    command = ['bash', 'scripts/gate.sh', mode]
    wait = 10800 if mode == 'full' else 0
    if (heavy.QUEUE_PATH == heavy.DEFAULT_QUEUE_PATH and
            Path(__file__).resolve().parent != (root/'scripts/build').resolve() or
            mode == 'full' and (getattr(gate, 'FULL_CAPACITY_PROTOCOL', None) != 1 or
                               'exclusive' not in inspect.signature(gate.full_slot).parameters)):
        # Select the transported operational runner, preserving checkout policy.
        runner = Path(__file__).resolve().with_name('gate.py')
        command = [sys.executable, '-B', str(runner), mode, '--source-policy', str(root)]
    if mode == 'full':
        command.extend(['--wait', str(wait)])
    command.extend(['--base', base, '--timeout', str(timeout), '--log', str(log)])
    return command, wait + timeout + 100


def run_checks(root, checks, log_path, base, lease_fd=None, mode="composition"):
    if not checks:
        with log_path.open("x", encoding="utf-8"):
            pass
        return
    sys.path.insert(0, str(root / "scripts/build"))
    import gate
    # Live qualification can cache the old checkout's runner before rebase.
    importlib.reload(gate)
    # This is still a gate/landing run: the ordinary supervisor owns its RAM
    # target, memory ceiling, disk fallback and joined cleanup. Leave time for
    # that cleanup outside the compilation deadline.
    with log_path.with_suffix(".driver.log").open("x", encoding="utf-8") as driver:
        if mode != "composition":
            # The ordinary gate writes its own log and receipt. Reserve both
            # names first so this fresh run cannot overwrite an earlier result.
            for path in (log_path, log_path.with_suffix(log_path.suffix + ".receipt.json")):
                with path.open("x", encoding="utf-8"):
                    pass
        # Both affected suites and compile-only compositions can include the
        # expanded Windows checks. Keep their existing reviewed allowance;
        # full qualification retains its own gate bound.
        timeout = (gate.REVIEWED_GATE_TIMEOUT_SECONDS if mode in ("affected", "composition") else
                   gate.parse_args([mode, "--log", str(log_path)]).timeout)
        try:
            command, supervisor_timeout = gate_invocation(root, gate, mode, base, timeout, log_path)
            gate.execute(command, driver, time.monotonic() + supervisor_timeout,
                         cwd=root, lease_fd=lease_fd)
        except subprocess.CalledProcessError as error:
            if error.returncode == 124:
                raise RuntimeError(f"Landing composition timed out at {timeout} s; see {driver.name}") from None
            raise
        except gate.GateTimeout:
            raise RuntimeError(f"Landing gate supervisor timed out at {supervisor_timeout} s; see {driver.name}") from None


def qualification_proof(root, log, candidate, base, mode):
    if mode == "composition":
        return None
    command = [sys.executable, "scripts/build/receipt.py", str(log), candidate, "--base", base]
    if mode == "full":
        command.append("--require-full")
    if receipt_record(log).get('batch') is True:
        command.append("--batch")
    path = log.with_suffix(log.suffix + ".receipt.json")
    before = digest(path, 65536)
    heavy.run(command, cwd=root, check=True, timeout=120)
    if digest(path, 65536) != before:
        raise ValueError("Fresh qualification receipt changed during verification")
    return before


def digest(path, limit=64 * 1024 * 1024):
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > limit:
        raise ValueError("Composition log must be a bounded regular file")
    return hashlib.sha256(path.read_bytes()).hexdigest()


def apply_policy(root, request):
    request = dict(request)
    if 'retained_full' in request:
        if request['full'] or request['run_gate']:
            raise ValueError('Reviewed composition needs its own completed qualification, never a full waiver')
        return request
    request["full"] = request["full"] or gate_policy.required_mode(
        paths(root, request["old_base"], request["accepted"]), root=root,
        base=request["old_base"], head=request["accepted"]) == "full"
    return request


def qualify(root, request):
    request = apply_policy(root, request)
    source(root, request["accepted"], request["branch"])
    return original_gate(root, request)


def compose(root, request):
    if 'retained_full' in request:
        raise ValueError('Reviewed composition cannot rebase; recompose and obtain a new review on current main')
    request = apply_policy(root, request)
    accepted, base = request["accepted"], request["old_base"]
    source(root, accepted, request["branch"])
    # The caller holds the canonical lease. Keep both stages in one priority
    # reservation so neither can queue behind ordinary work while holding it.
    # Qualification and detached composition use different checkout identities.
    # Keep both on NVMe; a RAM reservation is bound to the original checkout.
    capacity = (full_capacity.reserve(10800, shared=True) if request['full'] else nullcontext())
    with capacity, heavy.slot("landing", job_owner=request.get("owner", request["branch"]),
                    refund=request.get("refund"),
                    ram=False,
                    nvme=heavy.nvme_target(Path(os.environ.get("SECODE_CACHE_DIR", str(Path.home()/".cache/secode")))/"target")) as lease:
        if request["run_gate"]:
            sys.path.insert(0, str(root / "scripts/build"))
            import gate
            driver = Path(request["receipt"]).with_suffix(".qualification.log")
            mode = "full" if request["full"] else "affected"
            timeout = (gate.REVIEWED_GATE_TIMEOUT_SECONDS if mode == "affected" else
                       gate.parse_args([mode, "--log", request["gate_log"]]).timeout)
            with driver.open("x", encoding="utf-8") as log:
                command, supervisor_timeout = gate_invocation(root, gate, mode, base, timeout, request["gate_log"])
                gate.execute(command, log, time.monotonic() + supervisor_timeout,
                             cwd=root, lease_fd=lease.fileno())
            source(root, accepted, request["branch"])
        return compose_locked(root, request, lease_fd=lease.fileno())


def export_branch(request):
    # The controller derives the same per-attempt name from its private receipt.
    return "secode-land-" + hashlib.sha256(request["receipt"].encode()).hexdigest()[:32]


@contextmanager
def detached_source(root, commit):
    # A stable private path preserves this checkout's compiler cache across
    # attempts. Existing directories/worktree registrations are never adopted.
    name = "secode-land-" + hashlib.sha256(str(root).encode()).hexdigest()[:32]
    parent = worktrees()
    if not parent.is_absolute():
        raise ValueError('Composition worktree parent must be absolute')
    parent.mkdir(parents=True, exist_ok=True)
    checkout = parent / name
    if checkout.exists() or checkout.is_symlink():
        raise ValueError("Private composition checkout already exists; preserve it")
    heavy.run(["git", "worktree", "add", "--detach", str(checkout), commit],
              cwd=root, check=True, timeout=120)
    saved_target = None
    if os.environ.get("SECODE_WORKTREE_ROOT") == str(root):
        saved_target = os.environ.pop("CARGO_TARGET_DIR", None)
    try:
        yield checkout
    finally:
        if saved_target is not None:
            os.environ["CARGO_TARGET_DIR"] = saved_target
        # Only the checkout created above is disposable. A failed rebase may
        # leave conflicts here; the owner's index, files and ref stay intact.
        heavy.run(["git", "worktree", "remove", "--force", str(checkout)],
                  cwd=root, check=True, timeout=120)


def compose_locked(root, request, *, lease_fd):
    source(root, request["accepted"], request["branch"])
    with detached_source(root, request["accepted"]) as checkout:
        record = compose_detached(checkout, request, lease_fd=lease_fd)
    source(root, request["accepted"], request["branch"])
    # Publish a separate ref only after qualification and cleanup succeed.
    # Never overwrite an existing ref, including another attempt's export.
    heavy.run(["git", "update-ref", "refs/heads/" + export_branch(request),
               record["candidate"], "0" * 40], cwd=root, check=True, timeout=60)
    return record


def compose_detached(root, request, *, lease_fd):
    request = apply_policy(root, request)
    accepted, base = request["accepted"], request["old_base"]
    source(root, accepted, "")
    original_receipt = original_gate(root, request)
    # The priority ticket holds the heavy lock. Fetch HERE, after that wait, so a queued
    # quick check does not qualify another already obsolete canonical base.
    heavy.run(["git", "fetch", "--no-tags", "--", request["canonical"], "main"],
                   cwd=root, check=True, timeout=120)
    advanced = git(root, "rev-parse", "FETCH_HEAD").decode().strip()
    git(root, "merge-base", "--is-ancestor", base, advanced)
    owned = paths(root, base, accepted)
    generated = assert_disjoint(owned, paths(root, base, advanced))
    documents = merged_documents(root, base, accepted, advanced, owned)
    original = raw_diff(root, base, accepted, generated, documents)
    if advanced != base:
        rebase(root, advanced, generated)
    receipt_path = Path(request["receipt"])
    register = regenerate_register(root, receipt_path, lease_fd) if generated else None
    candidate = git(root, "rev-parse", "HEAD").decode().strip()
    verify_documents(root, candidate, documents)
    if raw_diff(root, advanced, candidate, generated, documents) != original:
        raise ValueError("Rebase changed the reviewed patch")
    before = source(root, candidate, "")
    mode = ("full" if request["full"] else "affected") if documents else "composition"
    checks = commands(root, owned, advanced, mode) if advanced != base else []
    log = receipt_path.with_suffix(".log")
    run_checks(root, checks, log, advanced, lease_fd=lease_fd, mode=mode)
    qualification = qualification_proof(root, log, candidate, advanced, mode)
    after = source(root, candidate, "")
    record = {"schema": "secode.landing-composition.v1", "exit": 0,
              "accepted": accepted, "old_base": base, "new_base": advanced,
              "candidate": candidate, "gate_log": request["gate_log"],
              "gate_receipt_sha256": original_receipt, "require_full": request["full"],
              "source_before": before, "source_after": after,
              "generated_register": register, "merged_documents": sorted(documents),
              "qualification_mode": mode, "qualification_receipt_sha256": qualification,
              "commands": checks, "log_sha256": digest(log), "finished_unix": time.time()}
    # Exclusive creation refuses both an old result and a pre-created symlink.
    with receipt_path.open("x", encoding="utf-8") as output:
        json.dump(record, output, indent=2)
        output.write("\n")
    print(f"Canonical composition PASS; original qualification retained: {receipt_path}", flush=True)
    return record


def verify(root, request):
    if 'retained_full' in request:
        raise ValueError('Reviewed composition must land its exact accepted commit without automatic composition')
    source(root, request["accepted"], request["branch"])
    exported = git(root, "rev-parse", "refs/heads/" + export_branch(request)).decode().strip()
    if exported != request["candidate"]:
        raise ValueError("Composition export differs from the qualified candidate")
    with detached_source(root, exported) as checkout:
        record = verify_detached(checkout, request)
    source(root, request["accepted"], request["branch"])
    return record


def verify_detached(root, request):
    if 'retained_full' in request:
        raise ValueError('Reviewed composition must land its exact accepted commit without automatic composition')
    request = apply_policy(root, request)
    path = Path(request["receipt"])
    metadata = path.lstat()
    if not stat.S_ISREG(metadata.st_mode) or metadata.st_size > 65536:
        raise ValueError("Composition receipt must be a bounded regular file")
    record = json.loads(path.read_text(encoding="utf-8"))
    expected = {key: request[key] for key in
                ("accepted", "old_base", "new_base", "candidate", "gate_log")}
    age = time.time() - record["finished_unix"]
    before = source(root, request["candidate"], "")
    if (record.get("schema") != "secode.landing-composition.v1" or record.get("exit") != 0 or
            not math.isfinite(age) or not 0 <= age <= 86400 or
            any(record.get(key) != value for key, value in expected.items()) or
            record.get("source_before") != before or record.get("source_after") != before or
            record.get("log_sha256") != digest(path.with_suffix(".log"))):
        raise ValueError("Composition receipt is stale, changed, or belongs to another source/base")
    # The old policy was checked before rebasing. Re-evaluating its coverage
    # with NEW dependency metadata would reject the very disjoint additions
    # covered by the new workspace check and expanded affected clippy.
    original_path = Path(request["gate_log"])
    original_receipt = original_path.with_suffix(original_path.suffix + ".receipt.json")
    original_digest = digest(original_receipt, 65536)
    original = json.loads(original_receipt.read_text(encoding="utf-8"))
    original_age = time.time() - original["finished_unix"]
    if (record.get("require_full") is not request["full"] or
            record.get("gate_receipt_sha256") != original_digest or
            not math.isfinite(original_age) or not 0 <= original_age <= 86400 or
            original.get("log_sha256") != digest(original_path) or
            request["full"] and original.get("mode") != "full"):
        raise ValueError("Original qualification changed, expired, or has the wrong tier")
    git(root, "merge-base", "--is-ancestor", request["new_base"], request["candidate"])
    owned = paths(root, request["old_base"], request["accepted"])
    generated = assert_disjoint(owned, paths(root, request["old_base"], request["new_base"]))
    register = register_proof(root, path) if generated else None
    if record.get("generated_register") != register:
        raise ValueError("Generated license register or its qualification changed")
    documents = merged_documents(root, request["old_base"], request["accepted"],
                                 request["new_base"], owned)
    if record.get("merged_documents", []) != sorted(documents):
        raise ValueError("Composition document coverage changed")
    verify_documents(root, request["candidate"], documents)
    if (raw_diff(root, request["old_base"], request["accepted"], generated, documents) !=
            raw_diff(root, request["new_base"], request["candidate"], generated, documents)):
        raise ValueError("Composition no longer contains the exact reviewed patch")
    mode = ("full" if request["full"] else "affected") if documents else "composition"
    if record.get("qualification_mode", "composition") != mode:
        raise ValueError("Composition is missing fresh qualification for its tier")
    qualification = qualification_proof(root, path.with_suffix(".log"), request["candidate"], request["new_base"], mode)
    if record.get("qualification_receipt_sha256") != qualification:
        raise ValueError("Fresh composed-tree qualification changed")
    required = commands(root, owned, request["new_base"], mode) if request["new_base"] != request["old_base"] else []
    if record.get("commands") != required:
        raise ValueError("Composition receipt is missing the workspace/affected checks")
    print("Fresh canonical composition PASS receipt verified", flush=True)
    return record


def parse_request(encoded, mode):
    if len(encoded) > 5464:
        raise ValueError("Landing request exceeds 4 KiB")
    raw = base64.b64decode(encoded, validate=True)
    if len(raw) > 4096:
        raise ValueError("Landing request exceeds 4 KiB")
    request = json.loads(raw)
    fields = {"accepted", "old_base", "branch", "canonical", "gate_log", "full", "run_gate", "receipt"}
    if mode in ("verify", "verify-docs"):
        fields |= {"candidate", "new_base"}
    if isinstance(request, dict) and 'retained_full' in request:
        if mode != 'qualify':
            raise ValueError('Retained full is only supported for exact reviewed qualification')
        reviewed_composition.retained_arguments(request['retained_full'])
        fields |= {'retained_full'}
    if isinstance(request, dict) and "refund" in request:
        if mode != 'compose' or not isinstance(request['refund'], str) or not re.fullmatch(r'[0-9-]{1,80}', request['refund']):
            raise ValueError('Refund requires a compose request and a queue ticket')
        fields |= {'refund'}
    if isinstance(request, dict) and "owner" in request:
        from heavy_queue import OWNER
        if not isinstance(request["owner"], str) or not OWNER.fullmatch(request["owner"]):
            raise ValueError("Invalid landing owner")
        fields |= {"owner"}
    if (not isinstance(request, dict) or set(request) != fields or
            any(type(request[key]) is not bool for key in ("full", "run_gate"))):
        raise ValueError("Invalid landing request fields")
    for key in fields - {"full", "run_gate", "retained_full"}:
        if not isinstance(request[key], str) or not request[key] or "\0" in request[key]:
            raise ValueError("Invalid landing request value")
    for key in {"accepted", "old_base"} | ({"candidate", "new_base"} if mode in ("verify", "verify-docs") else set()):
        if not re.fullmatch("[0-9a-f]{40}", request[key]):
            raise ValueError("Use full commit identities")
    if (not re.fullmatch("[a-z][a-z0-9-]*", request["branch"]) or
            any(not Path(request[key]).is_absolute() for key in ("gate_log", "receipt"))):
        raise ValueError("Use a plain owning branch and absolute receipt paths")
    return request


if __name__ == "__main__":
    cancelled = False
    def interrupted(_signal, _frame):
        global cancelled
        cancelled = True
        raise KeyboardInterrupt
    signal.signal(signal.SIGTERM, interrupted)
    signal.signal(signal.SIGINT, interrupted)
    try:
        if len(sys.argv) != 3 or sys.argv[1] not in ("compose", "verify", "qualify", "docs", "verify-docs"):
            raise ValueError("Use compose|verify|qualify|docs|verify-docs and one encoded request")
        request = parse_request(sys.argv[2], sys.argv[1])
        if sys.argv[1] in ('docs', 'verify-docs'):
            import landing_docs
            action = landing_docs.compose if sys.argv[1] == 'docs' else landing_docs.verify
        else:
            action = {"compose": compose, "verify": verify, "qualify": qualify}[sys.argv[1]]
        action(Path.cwd(), request)
    except heavy.ExclusiveHoldTimeout as error:
        print(error, file=sys.stderr)
        sys.exit(124)
    except TimeoutError as error:
        print(error, file=sys.stderr)
        sys.exit(heavy.LOCK_TIMEOUT)
    except (OSError, ValueError, RuntimeError, subprocess.SubprocessError, KeyboardInterrupt) as error:
        print(f"Landing composition refused: {error}", file=sys.stderr)
        sys.exit(130 if cancelled or isinstance(error, KeyboardInterrupt) or
                 isinstance(error, subprocess.CalledProcessError) and error.returncode == 130 else 1)
