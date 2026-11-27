"""Publication scan for new commits: identity, trailer, shape and message hygiene.

Checks every new commit between base and head, merges included, and the lines
each commit adds to its first parent, so content committed and later removed
is still caught. Every failure names the commit or file and the rule it broke.
Upstream/license phrases and the fixed test sentinels never flag, but they
exempt only themselves, never the rest of their line. Only the scanner's own
control file is an excluded path: it necessarily carries its patterns.
"""
import os
import re
import subprocess

def project_identity(field, root=None):
    override = os.environ.get('SECODE_COMMIT_AUTHOR_' + field.upper())
    if override:
        return override
    configured = subprocess.run(['git', 'config', 'user.' + field], cwd=root, capture_output=True, text=True)
    if configured.returncode not in (0, 1):
        raise ValueError('Cannot read the configured commit identity')
    if configured.stdout.strip():
        return configured.stdout.strip()
    return ''


REQUIRED_AUTHOR_NAME = project_identity('name')
REQUIRED_AUTHOR_EMAIL = project_identity('email')
SUBJECT_MAX = 72  # the subject must be under this many characters
BODY_MAX_LINES = 2  # the publication ruling; the upstream and trailer lines don't count

TRAILER_RE = re.compile(r"^Co-Authored-By: .+ <([^<>\s]+)@([^<>\s]+)>$")
# GitHub links co-authors by address, so the trailer names the harness that wrote
# the commit with a fixed address; any other name or address could credit a stranger.
CO_AUTHORS = frozenset({
    "Co-Authored-By: Claude <noreply@anthropic.com>",
    "Co-Authored-By: Codex <noreply@openai.com>",
    "Co-Authored-By: secode-seats <353560407+secode-seats[bot]@users.noreply.github.com>",
})
# Commits authored before 2026-11-11T00:00:00Z may keep their reviewed model trailers.
HARNESS_TRAILERS_FROM = 1794355200
SUBJECT_RE = re.compile(r"^[a-z][a-z0-9-]*\([a-z0-9./_-]+\): \S")
UPSTREAM_RE = re.compile(
    r"\bapache[- ]?2\.0|\bmit licen[cs]e|\bcopyright\b|adapted? from|ported from|"
    r"copied from|hard fork|\bopenai/codex\b|\banomalyco/opencode\b|\bxai-org/grok-build\b"
    r"|\bcrossterm-rs/crossterm\b", re.IGNORECASE)
# Fixed synthetic sentinels from the test fixtures; a line may quote them.
ALLOWED_SENTINELS = ("native-tui-r5", "private-secret", "env:NAME", "cred_openai")
# Rust identifiers and wire vocabulary that merely share a word with a seat name,
# by exact form: diff and instruction delta types, and model release stages.
EXEMPT_IDENTIFIERS = re.compile(
    r"\bInstructionError::Delta\b|^\s*Delta\(#\[from\] InstructionDeltaError\)"
    r"|\bstruct Delta\b|-> Delta\b|^\s*Delta \{$"
    r'|^\s*Alpha => "alpha",$|^\s*Beta => "beta",$')
# Exact test lines whose value must equal an external frozen fixture we do not own:
# form-suspend-settle-after-replacement/expected/durable.jsonl (crates/grapt-form/tests/form_service.rs).
EXEMPT_LINES = frozenset({
    'svc.answer(&ev("evt_r")?, &id, &json!({"name": "Beta"}), 0)?,',
    'svc.answer(&ev("evt_a1")?, &suspend_id, &json!({"name": "Beta"}), 0)?;',
})
# Upstream words in third-party data and vendored sources, by exact path and exact
# span; every other rule still applies to the rest of those lines.
CATALOG_TEXT = re.compile(r'"(?:name|description)":"((?:[^"\\]|\\.)*)"')
CATALOG_WORD = re.compile(r"(?<![A-Za-z])(?:Alpha|Beta)(?![A-Za-z])")


def _catalog_words(line):
    """Release stages and the vendor Aleph Alpha, only inside a catalog entry's
    `name` or `description` string value."""
    spans = []
    for value in CATALOG_TEXT.finditer(line):
        start = value.start(1)
        spans += [(start + word.start(), start + word.end())
                  for word in CATALOG_WORD.finditer(value.group(1))]
    return spans


def _regex_spans(pattern):
    return lambda line: [match.span() for match in pattern.finditer(line)]


EXEMPT_DATA = {
    "crates/grapt-connections/src/catalog/models.dev-2026-11-03.json": _catalog_words,
    # The Unicode property alias Alpha, for Alphabetic.
    "third-party/regress-0.10.5/src/unicodetables.rs":
        _regex_spans(re.compile(r'^\s*"Alpha"(?= \| "Alphabetic" =>)')),
    "third-party/regress-0.10.5/tests/unicode_property_escapes.rs":
        _regex_spans(re.compile(r'(?<=\\p\{)Alpha(?=\})')),
    # Dated history kept as recorded: evidence roots, receipt run roots and
    # review finding keys, whose digests and cross-references name them.
    "docs/acp-compatibility/RESULTS.md": _regex_spans(re.compile(r"(?<![A-Za-z0-9_])E:\\factory-evidence\\acp-gamma-r1(?=\\)")),
    "crates/grapt-runtime/mutations/manifest.json": _regex_spans(re.compile(r"(?<![A-Za-z0-9_-])(?:U2-)?ETA-R1-F[12](?![A-Za-z0-9_-])")),
    "crates/grapt-runtime/src/provider_source/tests.rs": _regex_spans(re.compile(r"(?<![A-Za-z0-9_-])U2-ETA-R1-F[12](?![A-Za-z0-9_-])")),
    "crates/grapt-permission/mutations/manifest.json": _regex_spans(re.compile(r"(?<![A-Za-z0-9_-])ETA-R2-01(?![A-Za-z0-9_-])")),
    # A live Windows bench directory and its lock, held for the box-infra rename.
    "scripts/build/windows_native_worker.py": _regex_spans(re.compile(r"(?<![A-Za-z0-9_])E:/temp/kappa-perf-bench02-20261024(?![A-Za-z0-9_.-])")),
    "scripts/stateengine-native/WINDOWS-BOUND-MCP.md": _regex_spans(re.compile(r"(?<![A-Za-z0-9_])E:/temp/kappa-perf-bench02-20261024(?![A-Za-z0-9_.-])")),
}
EXEMPT_DATA.update({
    f"fixtures/toolchain-activation/receipts/{name}.json": _regex_spans(re.compile(r"(?<![A-Za-z0-9_])C:\\\\fixtures\\\\(?:toolchain-delta-r1(?=\\\\)|rust-toolchain-delta-r1(?=\"))"))
    for name in (
        "base-6fe79ddf-counterexample",
        "tma-188ea36a-workspace-final-windows-serialized-run",
        "tma-2e4946a1-workspace-final-carrier-run",
        "tma-2e4946a1-workspace-final-windows-serialized-run",
        "tma-65ba30a7-workspace-final-windows-serialized-run",
        "tma-b139d590-workspace-final-windows-serialized-run",
        "tma-e495a153-carrier-run",
    )
})
HYGIENE = (
    # The account segment's first character may be anything that cannot close a
    # quoted bare root or end a path: no separator, whitespace, quote or closing
    # punctuation. So `"/home/"` (its closing quote would otherwise match) and a
    # Windows root ending in a separator are not read as accounts, while every
    # real segment is — including one that starts with a non-ASCII character,
    # which an ASCII-only class would silently exempt.
    ("real-path", re.compile(
        r"/home/(?!example(?:/|\b)|user(?:/|\b))[^/\\\s\"'`,;:)\]}>][^/\s]*|/mnt/[^/\s]+/(?:secode|grapt)|[A-Z]:[/\\](?:secode|rust-secode|grapt|rust-grapt)|[A-Z]:[/\\]Users[/\\](?!example(?:[/\\]|\b))[^/\\\s\"'`,;:)\]}>][^/\\\s]*")),  # grapt-rename: keep
    ("seat-id", re.compile(r"agt-[a-z0-9]{4}-[a-z0-9]{4}-[a-z0-9]{4}")),
    ("floor-msg-id", re.compile(r"msg-[0-9a-f]{8,}")),
    ("seat-name", re.compile(r"(^|[^A-Za-z])(Alpha|Beta|Gamma|Delta|Epsilon|Zeta|Theta|Iota|"
                             r"Kappa|Lambda|Mu|Nu|Omicron|Rho|Sigma|Tau|Eta|Xi)($|[^A-Za-z])")),
    # Lowercase seat names as folder, run and round names, and seat finding keys.
    ("seat-folder", re.compile(
        r"(?<![A-Za-z0-9])(?:alpha|beta|gamma|delta|epsilon|zeta|eta|theta|iota|kappa|lambda)"
        r"(?:[-_]seat(?![a-z])|(?:-[a-z0-9]+)+-20[0-9]{6}(?![0-9])|-r[0-9]+(?![A-Za-z0-9]))"
        r"|(?<![A-Za-z0-9])(?:ALPHA|BETA|GAMMA|DELTA|EPSILON|ZETA|ETA|THETA|IOTA|KAPPA|LAMBDA)-R[0-9]")),
)
SELF_PATH = "scripts/build/publication_scan.py"
EMPTY_TREE = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"


def _git(*args):
    # Git output is byte-exact: decode without newline translation so an
    # embedded CR, VT or U+2028 cannot split the line that owns it.
    return subprocess.check_output(["git", *args]).decode("utf-8", "replace")


def commits_between(base, head="HEAD"):
    """New commits between base and head, merges included."""
    return _git("rev-list", base + ".." + head).split()


def rename_mapper(base):
    """The product rename as a text function for `base`, or None without one."""
    script = os.path.join(_git("rev-parse", "--show-toplevel").strip(), "scripts", "rename", "grapt.py")
    if not os.path.exists(script):
        return None
    import importlib.util
    spec = importlib.util.spec_from_file_location("grapt_rename", script)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.mapper_for(base)


def commit_added_lines(sha, rename_aware=False):
    """Lines this commit adds to its first parent, per destination path.

    Rename-aware (the product rename's freeze run): an added line that is
    exactly the rename of a line the commit removes is not new text, so it is
    exempt; every other line is scanned as usual. The whole commit's removed
    lines count, because a moved file whose rename git did not detect arrives
    as a deletion in one block and an addition in another."""
    parents = _git("log", "-1", "--format=%P", sha).split()
    base = parents[0] if parents else EMPTY_TREE
    mapper = rename_mapper(base) if rename_aware and parents else None
    renamed_removed = set()
    out = []
    path = None
    old_path = None
    in_hunk = False
    # Rename detection: a moved file's unchanged lines already exist in the
    # repository, so only lines the move edits count as new text.
    for line in _git("diff", "--find-renames", "-U0", base, sha).split("\n"):
        # Git delimits diff lines with LF only, and each file block starts at
        # 'diff --git'; its headers run until the first hunk, and inside a
        # hunk every line is content, so changed '--'/'++' text cannot be
        # mistaken for a file header.
        if line.startswith("diff --git "):
            path = None
            old_path = None
            in_hunk = False
        elif line.startswith("@@"):
            in_hunk = True
        elif not in_hunk:
            if line.startswith("--- "):
                old_path = line[4:].strip()
                if old_path.startswith("a/"):
                    old_path = old_path[2:]
            elif line.startswith("+++ "):
                path = line[4:].strip()
                if path.startswith("b/"):
                    path = path[2:]
        elif line.startswith("-") and mapper is not None:
            renamed_removed.add(mapper(line[1:], old_path or ""))
        elif line.startswith("+") and path is not None and path != "/dev/null":
            out.append((path, line[1:]))
    if mapper is not None:
        out = [(path, line) for path, line in out if line not in renamed_removed]
    return out


def _record(sha):
    return {
        "name": _git("log", "-1", "--format=%an", sha).strip(),
        "email": _git("log", "-1", "--format=%ae", sha).strip(),
        "subject": _git("log", "-1", "--format=%s", sha).strip(),
        "body": _git("log", "-1", "--format=%b", sha).strip(),
        "authored": int(_git("log", "-1", "--format=%at", sha).strip()),
    }


def _legacy_trailer_allowed(line):
    """Model-named trailers authored before the harness rule; never a GitHub account."""
    local, domain = TRAILER_RE.match(line).groups()
    if (local.lower(), domain.lower()) == ("353560407+secode-seats[bot]", "users.noreply.github.com"):
        return True
    return local.lower() == "noreply" and domain.lower() != "users.noreply.github.com"


def _exempt(line, path=None):
    """Remove the only exempted spans: upstream phrases and fixed sentinels."""
    if line.strip() in EXEMPT_LINES:
        return ""
    spans = [match.span() for match in UPSTREAM_RE.finditer(line)]
    spans += [match.span() for match in EXEMPT_IDENTIFIERS.finditer(line)]
    if path in EXEMPT_DATA:
        spans += EXEMPT_DATA[path](line)
    for sentinel in ALLOWED_SENTINELS:
        start = 0
        while True:
            at = line.find(sentinel, start)
            if at < 0:
                break
            spans.append((at, at + len(sentinel)))
            start = at + len(sentinel)
    kept, cursor = [], 0
    for start, end in sorted(spans):
        if start < cursor:
            continue
        kept.append(line[cursor:start])
        cursor = end
    kept.append(line[cursor:])
    return "".join(kept)


def _hygiene_violation(line, name=None, path=None):
    tokens = re.findall(r'[A-Z][a-z]+', REQUIRED_AUTHOR_NAME if name is None else name)
    if tokens and re.search(r'(^|[^A-Za-z])(?:' + '|'.join(map(re.escape, tokens)) +
                            r')($|[^A-Za-z])', _exempt(line, path)):
        return 'personal-name'
    for rule, rx in HYGIENE:
        if rx.search(_exempt(line, path)):
            return rule
    return None


def _trailer_index(lines):
    if lines and TRAILER_RE.match(lines[-1]):
        return len(lines) - 1
    return None


def _scan_message(sha, rec, identity=None):
    name, email = identity or (REQUIRED_AUTHOR_NAME, REQUIRED_AUTHOR_EMAIL)
    short = sha[:10]
    out = []
    if (rec["name"] != name or rec["email"] != email
            or re.fullmatch(r"[^\s@]+@users\.noreply\.github\.com", rec["email"]) is None):
        out.append(f"publication-scan: {short}: author-identity: "
                   f"{rec['name']} <{rec['email']}> != "
                   f"{name} <{email}>")
    if not SUBJECT_RE.match(rec["subject"]):
        out.append(f"publication-scan: {short}: subject-shape: not 'type(scope): summary'")
    if len(rec["subject"]) >= SUBJECT_MAX:
        out.append(f"publication-scan: {short}: subject-length: "
                   f"{len(rec['subject'])} characters, must be under {SUBJECT_MAX}")
    lines = [line for line in rec["body"].split("\n") if line.strip()]
    trailer_index = _trailer_index(lines)
    if trailer_index is None:
        earlier = [line for line in lines if TRAILER_RE.match(line)]
        detail = (f"must be the final line: {earlier[-1].strip()[:80]}" if earlier
                  else "missing final 'Co-Authored-By: <harness> <address>' line")
        out.append(f"publication-scan: {short}: trailer: {detail}")
    elif lines[-1].strip() not in CO_AUTHORS and not (
            rec.get("authored", HARNESS_TRAILERS_FROM) < HARNESS_TRAILERS_FROM
            and _legacy_trailer_allowed(lines[-1])):
        out.append(f"publication-scan: {short}: trailer: name the harness with its fixed line "
                   f"(Claude, Codex or secode-seats; see AGENTS.md): {lines[-1].strip()[:80]}")
    content = lines if trailer_index is None else lines[:trailer_index]
    upstream = min(sum(1 for line in content if UPSTREAM_RE.search(line)), 1)
    if len(content) - upstream > BODY_MAX_LINES:
        out.append(f"publication-scan: {short}: body-length: {len(content)} lines, "
                   f"at most {BODY_MAX_LINES} besides the upstream and trailer lines")
    return out


def _scan_commit(sha, identity, contributor=False):
    if contributor:
        # A pull request's author and messages are not the landing's; the
        # strict scan of the landed commits still judges those.
        return []
    rec = _record(sha)
    short = sha[:10]
    out = []
    out.extend(_scan_message(sha, rec, identity))
    lines = [line for line in rec["body"].split("\n") if line.strip()]
    body = lines if _trailer_index(lines) is None else lines[:-1]
    for line in [rec["subject"]] + body:
        rule = _hygiene_violation(line, identity[0])
        if rule:
            out.append(f"publication-scan: {short}: {rule}: {line.strip()[:80]}")
    return out


def _identity_from(commit):
    return (REQUIRED_AUTHOR_NAME or _git('log', '-1', '--format=%an', commit).strip(),
            REQUIRED_AUTHOR_EMAIL or _git('log', '-1', '--format=%ae', commit).strip())


def check_commits(shas, identity=None, rename_aware=False, contributor=False):
    """Return the violation lines for exactly these commits and the lines they add.

    `rename_aware` (or PUBLICATION_SCAN_RENAME_AWARE=1) exempts lines whose
    only change is the product rename; it is for the rename's freeze run.
    `contributor` checks only the added content, for a pull request whose
    commits are not landed as written: any author, message or trailer."""
    rename_aware = rename_aware or os.environ.get("PUBLICATION_SCAN_RENAME_AWARE") == "1"
    if identity is None:
        # Anonymous checkout: the oldest commit's parent supplies what git config lacks.
        oldest = shas[-1] if shas else None
        parents = _git('log', '-1', '--format=%P', oldest).split() if oldest else []
        identity = _identity_from(parents[0] if parents else oldest) if oldest else ('', '')
    out = []
    for sha in shas:
        out.extend(_scan_commit(sha, identity, contributor))
        for path, line in commit_added_lines(sha, rename_aware):
            if path == SELF_PATH or not line.strip():
                continue
            rule = _hygiene_violation(line, identity[0], path)
            if rule:
                out.append(f"publication-scan: {sha[:10]}: {path}: {rule}: {line.strip()[:80]}")
    return out


def check(base, head="HEAD", rename_aware=False, contributor=False):
    """Return the violation lines for the new commits and the lines they add."""
    # An installed controller can be imported outside a repository. Resolve an
    # anonymous hosted checkout's identity only when a scan has an accepted base.
    return check_commits(commits_between(base, head), _identity_from(base), rename_aware,
                         contributor)
