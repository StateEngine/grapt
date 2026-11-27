#!/usr/bin/env python3
"""Every lock-poison recovery site under crates/, checked in as JSON.

The foundation design's phase 0 step: inventory every place a poisoned
lock guard is recovered with `into_inner`, with the guarded value, whether
the site takes the guard for mutation, and a proposed action (fail closed
or needs review). No site is converted in this phase; the checked-in
JSON is the baseline the later conversions hold themselves to.

Inclusion is unconditional on tokens: every non-declaration line under
crates/ that names `into_inner`, `PoisonError`, `clear_poison` or
`is_poisoned` is a candidate. The detector only lists candidates; it
never clears one. Text cannot resolve symbols, so the inventory has no
exemptions at all: every site is `needs_review`, and the only actions
are `needs-review` and `fail-closed`. The kinds are `std-lock`,
`channel-timeout` and `unresolved` (a candidate the detector cannot
classify: it reads text, not types, and does not expand
macro-generated code, so a type that reaches a site only through
inference is left for review rather than risk a false call).
`suggested_exempt` is advisory only: where an `X::into_inner` receiver
resolves, exactly and to the end, to a std type that cannot return a
`PoisonError` (`ManuallyDrop`, `Rc`, `Arc`) at a written std path or
std import, or to a repo type whose definition this repo can see with
an in-repo `into_inner`, it carries a short suggestion (for example
`ManuallyDrop (std path written at the call site)`); like
`state_bearing`, it never changes `needs_review` or `action`, and the
phase 2 compiler-checked door (FD-2p) does the real proving. Every
site stays in the inventory, so a new unclassified token still fails
the check.
`poison_recovery` tags the rest: `certain` when the site matches a
positive poison shape (`std-lock`, `channel-timeout`) or the enclosing
function shows a positive poison signal (`PoisonError`, `Poisoned(`,
`.lock()`, `.read()`, `.write()`, `.try_lock(`), and `unknown`
otherwise; both stay `needs_review`.
`state_bearing` defaults to
true: a recovery scope that only calls the documented read-only guard
methods (`len`, `is_empty`, `get`, `contains_key`, `iter`, `keys`,
`values`, `first`, `last`, `clone`, `as_ref`, `as_str`, `to_vec`,
`to_owned`, `copied`, `cloned`) is proven read-only and false; it is
otherwise false only where the detector proves the guarded value is never
written in the recovery scope, and `needs_review` marks the sites that
could not be proven either way.

usage: poison_inventory.py [--root PATH] [--check]
  no args   regenerate the JSON next to this file
  --check   regenerate in memory and fail on any drift from the JSON

Runs on the standard library alone (no rg), so the bare hosted Linux
runner qualifies it. CRLF and CR line endings are normalized, so a
Windows checkout produces the same inventory, and an empty discovery
fails loudly instead of building a zero-site baseline.
"""
import argparse
import json
import posixpath
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
BASELINE = Path(__file__).with_name('poison-inventory.json')
LOCK_CALL = re.compile(
    r'\.(?:lock|try_lock|try_lock_for|try_lock_until|read|write|try_read|try_write|'
    r'read_lock|write_lock|get_mut)\s*\(')
WAIT_TIMEOUT = re.compile(r'\.wait_timeout\s*\(')
FN = re.compile(r'^(?:pub(?:\([^)]*\))?\s+)?(?:async\s+|unsafe\s+)*fn\b\s*([A-Za-z_]\w*)')
POISON_ARM = re.compile(r'Err\((?:[^()]|\([^()]*\))*?Poison(?:[^()]|\([^()]*\))*\)\s*=>')
IDENT = re.compile(r'[a-z_][a-z0-9_]*')
STATIC = re.compile(r'[A-Z][A-Z0-9_]*$')
MUTATION_NAMES = ('insert', 'push', 'push_back', 'push_front', 'retain', 'remove', 'drain',
                 'extend', 'clear', 'swap', 'replace', 'take', 'set', 'register', 'record',
                 'commit', 'notify', 'emit', 'offer', 'add', 'update', 'apply', 'mark',
                 'settle', 'write', 'resize', 'pop', 'swap_remove', 'append',
                 'truncate', 'reset', 'put', 'push_str', 'remove_swapped', 'fetch_add',
                 'fetch_sub', 'fetch_or', 'fetch_and', 'swap_with', 'resize_default',
                 'extend_from_slice',
                 'insertion_sort', 'store_mut', 'borrow_mut', 'as_mut', 'make_mut',
                 'into_iter_mut', 'iter_mut')
MUTATING = re.compile(r'\b(?:' + '|'.join(MUTATION_NAMES) + r')\s*\(')
COMPOUND = r'(?:\+=|-=|\*=|/=|%=|&=|\|=|\^=)'
CANDIDATE = re.compile(r'\binto_inner\b|\bPoisonError\b|\bclear_poison\b|\bis_poisoned\b')
DECL_LINE = re.compile(r'^(?:macro_rules!\s|(?:pub(?:\([^)]*\))?\s+)?(?:use|type|fn|struct|enum|impl|trait|const|static|mod)\b)')
POISON_ALIAS = re.compile(r'\bPoisonError\b\s+as\s+([A-Za-z_]\w*)')
BASE_LOCK_TYPE = ('PoisonError', 'LockResult', 'TryLockError', 'TryLockResult',
                  'MutexGuard', 'RwLockReadGuard', 'RwLockWriteGuard', 'Mutex',
                  'RwLock')
LOCK_WORD = r'\b(?:' + '|'.join(BASE_LOCK_TYPE) + r')\b'
LOCK_WORD_RE = re.compile(LOCK_WORD)
POISON_SIGNAL = re.compile(
    r'PoisonError|Poisoned\(|\.lock\(\)|\.read\(\)|\.write\(\)|\.try_lock\(\)')
STD_NON_POISON = ('ManuallyDrop', 'Rc', 'Arc')
ADVISORY_DEPTH = 12
PRELUDE_NAMES = ('Option', 'Result', 'Some', 'None', 'Ok', 'Err', 'Box',
                 'String', 'Vec', 'Rc', 'Arc')
EXTERN_ROOTS = ('std', 'core', 'alloc')
CRATE_ROOTS = ('crate', 'self', 'super')
TYPE_ALIAS_DECL = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s+)?type\s+([A-Za-z_]\w*)\s*(?:<[^;{=]*>)?\s*=\s*(.*)$')
USE_AS = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s*)?use\s+(.+?)\s+as\s+([A-Za-z_]\w*)\s*;\s*$')
USE_PLAIN = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s*)?use\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*;\s*$')
USE_BRACE = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s*)?use\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*::\s*\{(.*)\}\s*;\s*$')
USE_GLOB = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s*)?use\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)\s*::\s*\*\s*;\s*$')
ITEM_NAME = re.compile(
    r'^(?:pub(?:\([^)]*\))?\s*)?(struct|enum|union|trait)\s+([A-Za-z_]\w*)')
IMPL_START = re.compile(r'^\s*(?:(?:pub(?:\([^)]*\))?\s*)?)?impl\b')
INCLUDE_CALL = re.compile(r'^\s*include!\s*\(\s*"([^"]+)"\s*\)\s*;')
READ_ONLY_METHODS = ('len', 'is_empty', 'get', 'contains_key', 'iter', 'keys',
                     'values', 'first', 'last', 'clone', 'as_ref', 'as_str',
                     'to_vec', 'to_owned', 'copied', 'cloned')
PATH_INTO = re.compile(r'([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)::into_inner\b')
IMPL_HEAD = re.compile(
    r'^\s*(?:pub(?:\([^)]*\))?\s+)?impl(?:<[^>]*>)?\s+(?:[A-Za-z_]\w*::)*([A-Za-z_]\w*)'
    r'(?:<[^>]*>)?\s*\{')
INTO_FN = re.compile(r'\bfn\s+into_inner\b')
NEWTYPE_MACRO = re.compile(
    r'\bnamed_newtype!\s*[({]\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_]\w*)')


def discover(root):
    """The crates/**/*.rs files under root, walked by the standard
    library so a bare runner needs no rg: the same view rg has of a
    checkout (target/ build trees, hidden paths and symlinks skipped),
    sorted. Fails loudly when none are found: a silent empty discovery
    would build a zero-site inventory and report every checked-in site
    as removed."""
    root_path = Path(root)
    crate_root = root_path / 'crates'
    files = []
    if crate_root.is_dir():
        for path in crate_root.rglob('*.rs'):
            parts = path.relative_to(root_path).parts
            if 'target' in parts or any(part.startswith('.') for part in parts):
                continue
            # lstat before follow: a symlinked .rs source would let
            # load_files read outside root/crates, which rg never does.
            if path.is_symlink() or not path.is_file():
                continue
            files.append('/'.join(parts))
    files.sort()
    if not files:
        raise SystemExit(
            f'poison inventory: no crates/**/*.rs found under {root}; '
            'check the checkout root before regenerating or checking.')
    return files


def char_literal(line, i):
    """True when the quote at line[i] starts a char literal, not a
    lifetime: its pair must hold exactly one char or one escape."""
    if i + 1 >= len(line):
        return False
    nxt = line[i + 1]
    if nxt == '\\':
        return True
    if not (nxt.isalpha() or nxt == '_'):
        return False
    return i + 2 < len(line) and line[i + 2] == "'"


def strip_code(lines):
    """Blank out comments and string/char literal contents, line for line.

    Attribute lines are kept verbatim: #[path] and friends carry their file
    names in strings that the test-file resolution reads back.
    """
    out, in_block = [], False
    for line in lines:
        attr_line = line.lstrip().startswith('#[') and not in_block
        buf, i, n, in_str, in_char = [], 0, len(line), False, False
        while i < n:
            c = line[i]
            if in_block:
                if line[i:i+2] == '*/':
                    in_block, i = False, i + 2
                    buf.append('  ')
                else:
                    buf.append(' '); i += 1
                continue
            if in_str:
                if c == '\\':
                    buf.append('  '); i += 2; continue
                if c == '"':
                    in_str = False
                buf.append('"'); i += 1; continue
            if in_char:
                if c == '\\':
                    buf.append('  '); i += 2; continue
                if c == "'":
                    in_char = False
                buf.append(' '); i += 1; continue
            if line[i:i+2] == '//':
                buf.append(' ' * (n - i)); break
            if line[i:i+2] == '/*':
                in_block = True; buf.append('  '); i += 2; continue
            if c == '"' and not attr_line:
                in_str = True; buf.append('"'); i += 1; continue
            if c == "'" and not attr_line and char_literal(line, i):
                in_char = True; buf.append("'"); i += 1; continue
            buf.append(c); i += 1
        out.append(''.join(buf))
    return out


def load_files(root, files):
    """(stripped lines, raw lines) per file; raw keeps the string
    literals of non-attribute lines that include! carries. Line endings
    are normalized (CRLF/CR to LF) so a Windows checkout inventories
    identically to the Linux gate box."""
    tree, raw = {}, {}
    for f in files:
        with open(f'{root}/{f}', encoding='utf-8', newline='') as handle:
            text = handle.read()
        text = text.replace('\r\n', '\n').replace('\r', '\n')
        lines = text.splitlines()
        raw[f] = lines
        tree[f] = strip_code(lines)
    return tree, raw


def logical_stmt(lines, i, cap=100):
    """(text, end index) of the use/type statement starting at lines[i].

    Returns (None, i) when no `;` terminates the statement within cap
    lines; macro_rules bodies can hold use-looking lines, and callers
    treat that as no statement.
    """
    s = lines[i]
    j = i
    while ';' not in s:
        j += 1
        if j - i >= cap or j >= len(lines):
            return None, i
        s = s + ' ' + lines[j]
    return s, j


def enclosing_fn(lines, idx):
    for j in range(idx, -1, -1):
        m = FN.match(lines[j].strip())
        if m:
            return m.group(1)
    return None


def fn_range(lines, fn_idx):
    """(fn_idx, end) for the fn declared at fn_idx, by brace balance."""
    balance, in_body = 0, False
    for j in range(fn_idx, len(lines)):
        s = lines[j].strip()
        if not in_body and '{' in s:
            in_body = True
        if in_body:
            balance += s.count('{') - s.count('}')
            if balance <= 0:
                return fn_idx, j
        elif ';' in s:
            return fn_idx, j
    return fn_idx, len(lines) - 1


STATEMENT_START = re.compile(
    r'^(?:let\b|match\b|if\b|else\b|for\b|while\b|loop\b|return\b|break\b|'
    r'continue\b|\{|\)\s*\{|\}\s*else\b)')


def statement_start(lines, idx, cap=40):
    """First line of the statement that contains lines[idx].

    Walks up from the site tracking the net brace balance of the lines
    crossed; a line that ends a statement (`;` or a closing brace) or starts
    one (`let`, `match`, `if`, a bare block, ...) bounds the walk whenever the
    crossed lines are brace-balanced at or above zero. A positive balance
    means the line opens a block still open at the site (a match arm, a
    multi-line signature), so it is a statement start, not a run-through.
    """
    bal = 0
    for j in range(idx - 1, max(idx - cap, -1), -1):
        s = lines[j].strip()
        if bal >= 0 and (s.endswith(';') or re.match(r'^}[\s\];]*$', s)
                         or FN.match(s) or STATEMENT_START.match(s)):
            if s.endswith(';') or re.match(r'^}[\s\];]*$', s):
                return j + 1
            return j
        bal += s.count('{') - s.count('}')
    return max(idx - cap, 0)


def statement_end(lines, idx, cap=40):
    p = b = 0
    for j in range(idx, min(idx + cap, len(lines) - 1)):
        s = lines[j].strip()
        p += s.count('(') - s.count(')')
        b += s.count('{') - s.count('}')
        if p <= 0 and b <= 0 and (s.endswith(';') or re.match(r'^}[\s\];]*$', s)):
            return j
    return min(idx + cap, len(lines) - 1)


def receiver_of(line, pos):
    j = pos - 1
    while j >= 0 and line[j] in ' \t*':
        j -= 1
    if j < 0:
        return None
    c = line[j]
    if c.isalnum() or c in '_$':
        k = j
        while k >= 0 and (line[k].isalnum() or line[k] in '_:$'):
            k -= 1
        return line[k+1:j+1]
    if c == ')':
        depth = 0
        while j >= 0:
            if line[j] == ')':
                depth += 1
            elif line[j] == '(':
                depth -= 1
                if depth == 0:
                    return 'expr'
            j -= 1
    return None


def chain_before(lines, j, k):
    """Receiver chain ending just before (line j, column k), over lines."""
    parts = []
    while True:
        while k >= 0 and lines[j][k] in ' \t*':
            k -= 1
        if k < 0:
            if j == 0:
                break
            j -= 1
            k = len(lines[j].rstrip()) - 1
            continue
        c = lines[j][k]
        if c.isalnum() or c in '_$':
            t = k
            while t >= 0 and (lines[j][t].isalnum() or lines[j][t] in '_$'):
                t -= 1
            parts.append(lines[j][t+1:k+1])
            k = t
            while k >= 0 and lines[j][k] in ' \t':
                k -= 1
            if k < 0:
                break
            if lines[j][k] == '.':
                k -= 1
                continue
            if lines[j][k] == ':' and k >= 1 and lines[j][k-1] == ':':
                k -= 2
                continue
            break
        elif c == ')':
            parts.append('expr')
            break
        else:
            break
    return '.'.join(reversed(parts)) or None


def call_receiver(lines, start, end, pattern):
    best = None
    for j in range(start, end + 1):
        for m in pattern.finditer(lines[j]):
            chain = chain_before(lines, j, m.start() - 1)
            if chain:
                best = chain
    return best


def arm_line(lines, j, recv):
    s = lines[j]
    return (re.search(r'Err\(\s*' + re.escape(recv) + r'\s*\)\s*=>', s)
            or POISON_ARM.search(s)
            or re.search(r'if\s+let\s+Err\(\s*' + re.escape(recv) + r'\s*\)\s*=', s))


def find_match_line(lines, arm_idx, cap=60):
    """The match/if-let line that owns the arm at arm_idx, or None."""
    depth = 0
    for k in range(arm_idx, max(arm_idx - cap, -1), -1):
        sk = lines[k].strip()
        if (re.match(r'^(?:\w[\w]*\s*=\s*)?(?:let\s+[^=]*=\s*)?match\b', sk)
                or re.match(r'if\s+let\s+Err', sk)):
            return k
        depth += sk.count('}') - sk.count('{')
        if depth > 1 or FN.match(sk):
            return None
    return None


def resolve_guard(lines, idx, recv, start):
    """(guard expression, kind) for a classified site."""
    g = call_receiver(lines, start, idx, LOCK_CALL)
    if g:
        return g, 'std-lock'
    if recv and IDENT.fullmatch(recv):
        for j in range(idx, max(idx - 40, -1), -1):
            if arm_line(lines, j, recv):
                k = find_match_line(lines, j)
                if k is not None:
                    region = max(0, k - 3)
                    g2 = call_receiver(lines, region, j, LOCK_CALL)
                    if g2:
                        return g2, 'std-lock'
                    w2 = call_receiver(lines, region, j, WAIT_TIMEOUT)
                    if w2:
                        return w2, 'channel-timeout'
                    sk = lines[k].strip()
                    m = (re.search(r'match\s+([A-Za-z_]\w*)\s*\{', sk)
                         or re.search(r'if\s+let\s+Err[^=]*=\s*([A-Za-z_]\w*)\s*$', sk))
                    if m:
                        local = m.group(1)
                        for q in range(k, max(k - 15, -1), -1):
                            if re.search(r'\b' + local + r'\s*=[^=]', lines[q]):
                                g3 = call_receiver(lines, max(0, q - 3), q, LOCK_CALL)
                                if g3:
                                    return g3, 'std-lock'
                                w3 = call_receiver(lines, max(0, q - 3), q, WAIT_TIMEOUT)
                                if w3:
                                    return w3, 'channel-timeout'
                return None, 'unresolved'
    return None, 'unresolved'


def closure_param(lines, idx, recv, cap=10):
    """The line carrying the closure parameter `|recv|`, at or above idx."""
    for j in range(idx, max(idx - cap, -1), -1):
        if re.search(r'\|\s*' + re.escape(recv) + r'\s*\|', lines[j]):
            return j
    return None


def multiline_recv(lines, idx):
    """Receiver of a chain continuation: the last identifier on the line above."""
    if idx == 0:
        return None
    m = re.search(r'([A-Za-z_]\w*)\s*$', lines[idx - 1].rstrip())
    return m.group(1) if m else None


def lock_evidence(lines, idx):
    """A lock call on the site's statement or on the statement before it."""
    start = statement_start(lines, idx)
    if any(LOCK_CALL.search(lines[j]) for j in range(start, idx + 1)):
        return True
    prev_start = statement_start(lines, start - 1) if start > 0 else 0
    return any(LOCK_CALL.search(lines[j]) for j in range(prev_start, start))


def classify(lines, idx, recv, aliases):
    """The known shape at lines[idx], or None when the detector cannot tell."""
    line = lines[idx]
    if recv and 'poison' in recv.lower():
        return 'poison-named'
    if recv and IDENT.fullmatch(recv):
        if re.search(r'\b' + recv + r'\.into_inner\(\)', line) \
                and closure_param(lines, idx, recv) is not None \
                and lock_evidence(lines, idx):
            return 'closure-lock'
        for j in range(idx, max(idx - 40, -1), -1):
            prev = lines[j]
            if re.search(r'Err\(\s*' + recv + r'\s*\)\s*=>', prev):
                return arm_result(lines, j)
            if POISON_ARM.search(prev):
                return 'arm-poison-pattern'
            if re.search(r'if\s+let\s+Err\(\s*' + recv + r'\s*\)\s*=', prev):
                region = '\n'.join(lines[max(0, j - 3):j + 1])
                if LOCK_CALL.search(region) or WAIT_TIMEOUT.search(region):
                    return 'arm-lock'
            if j < idx and (prev.rstrip().endswith(';')
                            or re.match(r'^}[\s\];]*$', prev.strip())
                            or FN.match(prev.strip())):
                break
    for m in re.finditer(r'([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)::into_inner\b', line):
        name = m.group(1)
        if 'poison' in name.lower() or any(seg in aliases for seg in name.split('::')):
            return 'path-poison'
    return None


def arm_result(lines, arm_idx):
    for j in range(arm_idx, max(arm_idx - 15, -1), -1):
        prev = lines[j].strip()
        if prev.startswith('match') or re.match(r'if\s+let\s+Err', prev):
            region = '\n'.join(lines[j:arm_idx])
            if LOCK_CALL.search(region) or WAIT_TIMEOUT.search(region):
                return 'arm-lock'
            if any(LOCK_CALL.search(lines[q]) or WAIT_TIMEOUT.search(lines[q])
                   for q in range(max(j - 5, 0), j)):
                return 'arm-lock'
            return None
        if FN.match(prev) or prev.endswith(';'):
            return None
    return None


def binding_of(stmt_text, recv):
    """The innermost `let` whose brace scope still contains the recovery."""
    m = re.search(re.escape(recv) + r'(?:\.into_inner\(\)|::into_inner)', stmt_text)
    if not m:
        return None
    head = stmt_text[:m.start()]
    for lm in reversed(list(re.finditer(r'\blet\s+(?:mut\s+)?([a-z_]\w*)\s*=', head))):
        balance = 0
        for ch in head[lm.end():]:
            if ch == '{':
                balance += 1
            elif ch == '}':
                balance -= 1
                if balance < 0:
                    break
        else:
            return lm.group(1)
    return None


def block_window(lines, idx, cap=120):
    """(text, bounded) for the lines from idx through its block's end."""
    b = 0
    out = []
    for j in range(idx, min(idx + cap, len(lines))):
        s = lines[j].strip()
        b += s.count('{') - s.count('}')
        if b < 0:
            break
        out.append(lines[j])
    return '\n'.join(out), b >= 0


MUT_CALL = r'(?:' + '|'.join(MUTATION_NAMES) + r')'


def write_scan(text, name):
    """True when text writes the value bound as name."""
    p = re.escape(name)
    patterns = (
        r'(?<![\.:])\b' + p + r'\s*' + COMPOUND,
        r'(?<![\.:])\b' + p + r'(?:\s*\.\s*[a-z_]\w*)*\s*' + COMPOUND,
        r'(?<![\.:])\b' + p + r'\s*\[[^\]]*\]\s*(?:=|' + COMPOUND + r')(?!=)',
        r'\*\s*' + p + r'\s*(?:=|' + COMPOUND + r')(?!=)',
        r'(?<![\.:])\b' + p + r'(?:\s*\.\s*[a-z_]\w*)*\s*=(?!=)',
        r'&\s*mut\s+\*?\s*' + p + r'(?:\s*\.\s*[a-z_]\w*)*',
        r'(?<![\.:])\b' + p + r'(?:\s*\.\s*[a-z_]\w*)*\s*\.\s*' + MUT_CALL + r'\s*\(',
        r'\*\s*' + p + r'(?:\s*\.\s*[a-z_]\w*)*\s*\.\s*' + MUT_CALL + r'\s*\(',
    )
    return any(re.search(pat, text) for pat in patterns)


def ambiguous_use(text, name):
    """True when a use of name in text could not be proven a read."""
    for m in re.finditer(r'(?<![\.:])\b' + re.escape(name) + r'\b', text):
        rest = text[m.end():]
        sm = re.match(r'\s*([.\[(])', rest)
        if sm:
            c = sm.group(1)
            if c == '(':
                return True
            if c == '[':
                continue
            chain = re.match(r'(?:\s*\.[a-z_]\w*)+', rest)
            after = rest[chain.end():] if chain else rest
            if re.match(r'\s*\(', after):
                if chain:
                    last = chain.group(0).split('.')[-1].strip()
                    if last in READ_ONLY_METHODS:
                        continue
                return True
            continue
        before = text[:m.start()].rstrip()
        bchar = before[-1] if before else ''
        if bchar in '(,':
            if not re.search(r'drop\s*\(\s*$', before):
                return True
        elif bchar != '*':
            return True
    return False


def scoped_text(window, names, name, binding):
    """window with name's own declaration blanked and, for the binding, the
    declarations of its aliases (tracked reads, not bare uses)."""
    text = window
    if name == binding:
        text = re.sub(r'\blet\s+(?:mut\s+)?' + re.escape(name) + r'\s*=', ' ',
                      text, count=1)
        for other in names:
            if other != name:
                text = re.sub(r'\blet\s+(?:mut\s+)?' + re.escape(other) + r'\s*='
                              r'\s*' + re.escape(name) + r'\s*;', ' ', text)
    else:
        text = re.sub(r'\blet\s+(?:mut\s+)?' + re.escape(name) + r'\s*=\s*'
                      + re.escape(binding) + r'\s*;', ' ', text, count=1)
    return text



def state_bearing(lines, idx, recv, start, end):
    """(state_bearing, needs_review): true unless the guarded value is proven
    never written in the recovery scope."""
    site_line = lines[idx]
    if re.search(r'&\s*mut\s+' + re.escape(recv)
                 + r'\s*(?:\.into_inner\(\)|::into_inner)', site_line):
        return True, False
    marker = None
    for cand in (recv + '.into_inner()', recv + '::into_inner'):
        if cand in site_line:
            marker = cand
            break
    if marker:
        after = site_line[site_line.find(marker) + len(marker):]
        if MUTATING.search(' ' + after):
            return True, False
        if re.search(r'(?<![=<>!+\-*/&|])=(?!=)', after):
            return True, False
    stmt = '\n'.join(lines[start:end + 1])
    binding = binding_of(stmt, recv)
    if not binding:
        m = re.search(re.escape(recv) + r'\s*(?:\.into_inner\(\)|::into_inner)', stmt)
        tail = stmt[m.end():] if m else ''
        if MUTATING.search(' ' + tail):
            return True, False
        if re.search(r'(?<![=<>!+\-*/&|])=(?!=)', tail):
            return True, False
        return True, True
    let_idx = None
    for j in range(start, end + 1):
        if re.search(r'let\s+(?:mut\s+)?' + re.escape(binding) + r'\s*=', lines[j]) \
                and statement_end(lines, j) >= idx:
            let_idx = j
            break
    if let_idx is None:
        return True, True
    window, bounded = block_window(lines, let_idx)
    if bounded:
        return True, True
    names = [binding]
    for m in re.finditer(r'\blet\s+(?:mut\s+)?([a-z_]\w*)\s*=\s*'
                         + re.escape(binding) + r'\s*;', window):
        names.append(m.group(1))
    names = list(dict.fromkeys(names))
    for name in names:
        if write_scan(scoped_text(window, names, name, binding), name):
            return True, False
    for name in names:
        if ambiguous_use(scoped_text(window, names, name, binding), name):
            return True, True
    return False, False



MOD_DECL = re.compile(r'\s*(?:(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*([;{]))')


def attr_lines(lines, i):
    """Attribute and blank lines directly above lines[i]."""
    attrs = []
    j = i - 1
    while j >= 0:
        s = lines[j].strip()
        if not s or s.startswith('#['):
            attrs.append(s)
            j -= 1
            continue
        break
    return attrs


def cfg_test_above(lines, i):
    return any('cfg(test)' in a for a in attr_lines(lines, i))


def path_attr_above(lines, i):
    for a in attr_lines(lines, i):
        m = re.match(r'#\[path\s*=\s*"([^"]+)"\]', a)
        if m:
            return m.group(1)
    return None


def resolve_mod(files, base, name, path):
    """The file a module declaration in files[base] refers to, or None.

    #[path] is relative to the declaring file's own directory; a pathless
    mod in `dir/stem.rs` resolves under `dir/stem/`, and one in
    `dir/mod.rs` (or a crate root) under `dir/`.
    """
    d, stem = ('', base) if '/' not in base else base.rsplit('/', 1)
    d += '/'
    if path is not None:
        cands = [posixpath.normpath(d + path)]
    elif stem == 'mod.rs' or stem in ('lib.rs', 'main.rs'):
        cands = [d + name + '.rs', d + name + '/mod.rs']
    else:
        sub = d + stem[:-3] + '/'
        cands = [sub + name + '.rs', sub + name + '/mod.rs']
    for c in cands:
        if c in files:
            return c
    return None


def module_parents(tree, raw):
    """child file -> the file whose module its lines join.

    A `mod name;` file joins the module of the declaring file; an
    `include!("file")` target is even closer, its lines being part of the
    including file's module, so it sees that file's whole scope.
    """
    files = set(tree)
    parents = {}
    for f in sorted(tree):
        lines = tree[f]
        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if m and (m.group(2) == ';' or path_attr_above(lines, i) is not None):
                p = resolve_mod(files, f, m.group(1), path_attr_above(lines, i))
                if p:
                    parents.setdefault(p, f)
        for rline in raw[f]:
            inc = INCLUDE_CALL.match(rline)
            if inc:
                p = posixpath.normpath(posixpath.dirname(f) + '/' + inc.group(1))
                if p in files:
                    parents.setdefault(p, f)
    return parents


def test_files(tree):
    """Files that only compile under cfg(test): every cfg(test) module,
    resolved to its file (including #[path] renames), plus, by inheritance,
    every module a test file declares."""
    files = set(tree)
    test = set()
    frontier = []
    for f, lines in tree.items():
        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if m and cfg_test_above(lines, i):
                p = resolve_mod(files, f, m.group(1), path_attr_above(lines, i))
                if p and p not in test:
                    test.add(p)
                    frontier.append(p)
    while frontier:
        f = frontier.pop()
        flines = tree[f]
        for i, line in enumerate(flines):
            m = MOD_DECL.match(line)
            if m:
                p = resolve_mod(files, f, m.group(1), path_attr_above(flines, i))
                if p and p not in test:
                    test.add(p)
                    frontier.append(p)
    return test


def is_test_site(file, lines, idx, tfiles):
    if file in tfiles:
        return True
    parts = file.split('/')
    if len(parts) >= 3 and parts[0] == 'crates' and parts[2] == 'tests':
        return True
    fn_idx = idx
    for j in range(idx, -1, -1):
        if FN.match(lines[j].strip()):
            fn_idx = j
            break
    for j in range(fn_idx - 1, max(fn_idx - 100, -1), -1):
        s = lines[j].strip()
        if re.match(r'^}[\s\];]*$', s):
            return False
        m = MOD_DECL.match(s)
        if m and m.group(2) == '{' and cfg_test_above(lines, j):
            return True
    return False


def lock_type_in(text):
    m = re.search(r'\b(?:Mutex|RwLock)<', text)
    if not m:
        return None
    i = m.end()
    depth = 1
    while i < len(text) and depth:
        if text[i] == '<':
            depth += 1
        elif text[i] == '>':
            depth -= 1
        i += 1
    return text[m.end():i-1].strip() or None


def guard_type(tree, file, lines, fn_idx, idx, guard):
    if not guard or guard == 'expr':
        return None
    seg = guard.rsplit('.', 1)[-1]
    if not re.fullmatch(r'[A-Za-z_]\w*|\d+', seg):
        return None
    for line in lines:
        m = re.match(r'\s*(?:(?:pub(?:\([^)]*\))?)\s+)?' + re.escape(seg)
                    + r'\s*:\s*(.+)$', line)
        if m and '&' not in m.group(1):
            t = lock_type_in(m.group(1))
            if t:
                return t
    region = lines[max(0, fn_idx - 1):idx + 1]
    for line in region:
        m = re.search(r'\b' + re.escape(seg) + r'\s*:\s*(.+)$', line)
        if m:
            t = lock_type_in(m.group(1))
            if t:
                return t
        m = re.search(r'\blet\s+' + re.escape(seg) + r'\s*=\s*(?:Arc::new\()?Mutex::new\(', line)
        if m:
            return None  # type comes from the initialiser, not textually recoverable
    return None


def scope_of(site):
    guard = site.get('guard') or ''
    seg = guard.rsplit('.', 1)[-1]
    if site['test']:
        return 'test'
    if STATIC.fullmatch(seg):
        return 'process-shared'
    if site['file'].startswith('crates/grapt-store/'):
        return 'process-shared'
    if 'shared_inbox' in (site.get('function') or ''):
        return 'shared-hub'
    if (site['file'] == 'crates/grapt-product/src/dogfood/mod.rs'
            and site['function'] == 'lock_inbox'):
        return 'shared-hub'
    return 'single-scope'


def action_of(site, scope):
    if scope == 'shared-hub':
        return ('fail-closed',
                'Shared inbox across sessions; a poisoned guard must not be swallowed '
                'here (the design removes this swallow in a later phase).')
    if scope == 'process-shared':
        where = ('the store' if site['file'].startswith('crates/grapt-store/')
                 else 'the process')
        return ('fail-closed',
                'Process-shared lock for ' + where + '; a poisoned guard there must not '
                'be swallowed, so the caller fails closed instead.')
    if scope == 'test':
        return ('needs-review',
                'Test-only code path; no production lock is involved.')
    return ('needs-review',
            'Single-scope state (a session, connection or request); if confirmed '
            'as a plain value, recover by re-establishing that scope rather '
            'than swallowing a poisoned guard.')


def impl_into_inner_types(tree):
    """Types whose in-repo impl block or newtype macro defines into_inner,
    mapped to the files that define them."""
    impls = {}
    for f, flines in tree.items():
        for m in NEWTYPE_MACRO.finditer('\n'.join(flines)):
            impls.setdefault(m.group(1), set()).add(f)
    for f, flines in tree.items():
        for i, line in enumerate(flines):
            m = IMPL_HEAD.match(line.strip())
            if not m:
                continue
            start = i if '{' in line else None
            if start is None:
                for j in range(i, min(i + 10, len(flines))):
                    if '{' in flines[j]:
                        start = j
                        break
            if start is None:
                continue
            balance = 0
            for j in range(start, len(flines)):
                if INTO_FN.search(flines[j]):
                    impls.setdefault(m.group(1), set()).add(f)
                balance += flines[j].count('{') - flines[j].count('}')
                if balance <= 0:
                    break
    return impls


def bind_alias(bind, joined):
    """Record, through bind(name, kind, payload), the names a (joined)
    use statement binds; globs bind names tracked per scope instead."""
    m = USE_AS.match(joined)
    if m:
        bind(m.group(2), 'import', m.group(1).strip())
        return
    if USE_GLOB.match(joined):
        return
    m = USE_PLAIN.match(joined)
    if m and '*' not in m.group(1):
        bind(m.group(1).rsplit('::', 1)[-1], 'import', m.group(1))
        return
    m = USE_BRACE.match(joined)
    if m:
        base = m.group(1)
        for item in m.group(2).split(','):
            item = item.strip()
            am = re.match(r'([A-Za-z_]\w*)\s+as\s+([A-Za-z_]\w*)$', item)
            if am:
                bind(am.group(2), 'import', base + '::' + am.group(1))
            elif re.fullmatch(r'[A-Za-z_]\w*', item):
                bind(item, 'import', base + '::' + item)



def generic_names(text):
    """The top-level names of a `<...>` generic parameter list."""
    names = set()
    for part in text.split(','):
        nm = re.match(r'\s*([A-Za-z_]\w*)', part)
        if nm:
            names.add(nm.group(1))
    return names


def file_scope(lines):
    """(bindings, globs) for one file: the first visible binding per name
    (import path, alias body, item kind, or generic with no payload), and
    the paths of its glob imports."""
    bindings = {}
    globs = []

    def bind(name, kind, payload):
        bindings.setdefault(name, (kind, payload))

    i, n = 0, len(lines)
    while i < n:
        s = lines[i].strip()
        if re.match(r'^(?:pub(?:\([^)]*\))?\s*)?(?:use|type)\b', s):
            joined, j = logical_stmt(lines, i)
            if joined is not None:
                m = TYPE_ALIAS_DECL.match(joined)
                if m:
                    body = m.group(2).strip().rstrip(';').strip()
                    bind(m.group(1), 'type', body)
                    gm = re.search(r'<([^<>]*(?:<[^<>]*>[^<>]*)*)>', s)
                    if gm:
                        for g in generic_names(gm.group(1)):
                            bind(g, 'generic', None)
                else:
                    gm = USE_GLOB.match(joined)
                    if gm:
                        globs.append(gm.group(1))
                    else:
                        bind_alias(bind, joined)
                        if USE_BRACE.match(joined) is None and re.search(
                                r'\{[^{}]*\*[^{}]*\}', joined):
                            m2 = re.match(
                                r'^(?:pub(?:\([^)]*\))?\s*)?'
                                r'use\s+([A-Za-z_]\w*(?:::[A-Za-z_]\w*)*)',
                                joined)
                            if m2:
                                globs.append(m2.group(1))
            i = j + 1 if joined is not None else i + 1
            continue
        m = ITEM_NAME.match(s)
        if m:
            bind(m.group(2), 'item', m.group(1))
        gm = re.search(r'(?:fn|impl)\s+[A-Za-z_]\w*\s*<([^<>]*(?:<[^<>]*>[^<>]*)*)>', s)
        if gm:
            for g in generic_names(gm.group(1)):
                bind(g, 'generic', None)
        i += 1
    return bindings, globs


class Scope:
    """Name visibility across files: a file sees its own bindings first,
    then those of its parent module's file, up the module chain to the
    crate root; an include! target sees the including file's scope."""

    def __init__(self, tree, parents, impl_files):
        self.tree = tree
        self.parents = parents
        self.impl_files = impl_files
        self.files = set(tree)
        self.children = {}
        for child, parent in parents.items():
            self.children.setdefault(parent, []).append(child)
        self.scopes = {f: file_scope(tree[f]) for f in tree}
        self.chain_cache = {}
        self.subtree_cache = {}

    def chain(self, file):
        if file not in self.chain_cache:
            out, cur, seen = [], file, set()
            while cur is not None and cur not in seen:
                seen.add(cur)
                out.append(cur)
                cur = self.parents.get(cur)
            self.chain_cache[file] = out
        return self.chain_cache[file]

    def subtree_names(self, file):
        if file not in self.subtree_cache:
            names, stack, seen = set(), [file], set()
            while stack:
                cur = stack.pop()
                if cur in seen or cur not in self.scopes:
                    continue
                seen.add(cur)
                names.update(self.scopes[cur][0])
                stack.extend(self.children.get(cur, ()))
            self.subtree_cache[file] = names
        return self.subtree_cache[file]

    def workspace_crate(self, name):
        for cand in (name, name.replace('_', '-'), name.replace('-', '_')):
            for stem in ('src/lib.rs', 'src/main.rs'):
                p = 'crates/' + cand + '/' + stem
                if p in self.files:
                    return p
        return None

    def module_child(self, file, name):
        lines = self.tree.get(file)
        if lines is None:
            return None
        for i, line in enumerate(lines):
            m = MOD_DECL.match(line)
            if m and m.group(1) == name and (m.group(2) == ';'
                                             or path_attr_above(lines, i) is not None):
                return resolve_mod(self.files, file, name,
                                   path_attr_above(lines, i))
        return None

    def glob_source(self, file, path):
        """('std'|'crate'|'crate-external'|'unknown', info) for a glob."""
        segs = [s for s in path.split('::') if s]
        if not segs:
            return 'unknown', None
        root = segs[0]
        if root in EXTERN_ROOTS:
            return 'std', None
        if root == 'super':
            base = self.parents.get(file)
            kind = 'crate'
            if base is None:
                return 'unknown', None
        elif root in ('crate', 'self'):
            base = self.chain(file)[-1]
            kind = 'crate'
        else:
            base = self.workspace_crate(root)
            kind = 'crate-external'
            if base is None:
                return 'unknown', None
        segs = segs[1:]
        cur = base
        for seg in segs:
            if seg in ('crate', 'self'):
                continue
            cur = self.module_child(cur, seg)
            if cur is None:
                return 'unknown', None
        return kind, cur

    def glob_binds(self, file, path, name):
        kind, info = self.glob_source(file, path)
        if kind == 'std':
            return (name in PRELUDE_NAMES or name in BASE_LOCK_TYPE
                    or name in STD_NON_POISON)
        if kind == 'unknown':
            return True
        return name in self.subtree_names(info)

    def resolve(self, file, name):
        """The first visible binding of name to file, or None."""
        for f in self.chain(file):
            bindings, globs = self.scopes[f]
            if name in bindings:
                return bindings[name]
            for path in globs:
                if self.glob_binds(f, path, name):
                    return ('glob', path)
        return None

    def prove_in_repo(self, file, last):
        """True when last is bound to this repo's into_inner type at file
        through a visible declaration, import or parent-module glob."""
        occ = self.resolve(file, last)
        if occ is None:
            return False
        kind, payload = occ
        if kind == 'import':
            iroot = payload.split('::')[0]
            if iroot in EXTERN_ROOTS:
                return False
            if iroot in CRATE_ROOTS:
                return True
            return self.workspace_crate(iroot) is not None
        if kind == 'glob':
            gkind, _info = self.glob_source(file, payload)
            return gkind in ('crate', 'crate-external')
        if kind == 'type':
            if LOCK_WORD_RE.search(payload):
                return False
            m = re.match(r'\s*([A-Za-z_]\w*)', payload)
            return bool(m) and m.group(1) in self.impl_files
        if kind == 'item':
            return payload in ('struct', 'enum', 'union')
        return False

    def enclosing_generics(self, lines, idx):
        """(generic parameter names, fn signature text) of the fn and the
        innermost enclosing impl of the site at lines[idx]."""
        gens, sig = set(), ''
        for j in range(idx, -1, -1):
            if FN.match(lines[j].strip()):
                sig = fn_signature(lines, j)
                gens |= fn_generics(sig)
                break
        for j in range(idx, -1, -1):
            if not IMPL_START.match(lines[j]):
                continue
            head = lines[j]
            m = re.search(r'impl\s*<([^>]*)>', head)
            if m is None:
                buf = head
                for q in range(j + 1, min(j + 6, len(lines))):
                    buf += ' ' + lines[q]
                    if '>' in buf:
                        m = re.search(r'impl\s*<([^>]*)>', buf)
                        break
            if m:
                gens |= generic_names(m.group(1))
            start = None
            for q in range(j, min(j + 15, len(lines))):
                if '{' in lines[q]:
                    start = q
                    break
            if start is None:
                continue
            if idx < start:
                return gens, sig  # the site sits in this impl's header
            balance = 0
            for q in range(start, min(start + 3000, len(lines))):
                balance += lines[q].count('{') - lines[q].count('}')
                if balance <= 0:
                    if q >= idx:
                        return gens, sig
                    break  # impl closed above the site: keep scanning
        return gens, sig


def fn_signature(lines, fn_idx, cap=30):
    """Text of the fn signature at fn_idx: name through the parameter list
    and return type, stopping at the body brace or a `;`."""
    parts, depth, opened = [], 0, False
    for j in range(fn_idx, min(fn_idx + cap, len(lines))):
        s = lines[j]
        parts.append(s)
        for ch in s:
            if ch == '(':
                depth += 1
                opened = True
            elif ch == ')':
                depth -= 1
        if opened and depth <= 0:
            tail = s[s.rfind(')') + 1:]
            if '{' in tail or ';' in tail:
                break
    return '\n'.join(parts)


def fn_generics(sig):
    """Generic parameter names declared in a fn signature text."""
    head = sig.split('(', 1)[0]
    m = re.search(r'<([^<>]*(?:<[^<>]*>[^<>]*)*)>\s*$', head.rstrip())
    return generic_names(m.group(1)) if m else set()


def _advisory_text(kind, name, how):
    if kind == 'std':
        return f'{name} ({how})'
    return f'{name} (into_inner defined for {name} in this repo)'


def _resolve_visible(scope, file, name):
    """(file, (kind, payload)) of the first visible binding of name,
    walking the file's scope chain like scope.resolve, or None."""
    for f in scope.chain(file):
        bindings, globs = scope.scopes[f]
        if name in bindings:
            return f, bindings[name]
        for path in globs:
            if scope.glob_binds(f, path, name):
                return f, ('glob', path)
    return None


def _walk_import(scope, file, segs, depth, seen):
    """The advisory for a multi-segment name path, or None: the path
    must resolve, exactly and to the end, to a std non-poison type or
    a repo type with an in-repo into_inner; an unresolved identity, a
    lock word, a cycle or the depth limit all give None."""
    if depth > ADVISORY_DEPTH:
        return None
    if len(segs) == 1:
        return _verify_named(scope, file, segs[0], depth + 1, seen)
    root, last = segs[0], segs[-1]
    if root in EXTERN_ROOTS:
        if last in STD_NON_POISON:
            return _advisory_text('std', last, 'imported from std')
        return None
    if root in ('crate', 'self'):
        cur = scope.chain(file)[-1]
    elif root == 'super':
        cur = scope.parents.get(file)
        if cur is None:
            return None
    else:
        cur = scope.module_child(file, root) or scope.workspace_crate(root)
        if cur is None:
            return None
    for seg in segs[1:-1]:
        cur = scope.module_child(cur, seg)
        if cur is None:
            return None
    return _verify_named(scope, cur, last, depth + 1, seen)


def _verify_named(scope, file, name, depth, seen):
    """The advisory when name, in file's own scope, resolves exactly to
    a std non-poison type or a repo type with an in-repo into_inner,
    else None: follows type aliases, imports and re-exports to the
    end; a lock word anywhere on the way gives None."""
    if depth > ADVISORY_DEPTH or (file, name) in seen:
        return None
    seen = seen | {(file, name)}
    binding = scope.scopes[file][0].get(name)
    if binding is None:
        return None
    kind, payload = binding
    if kind == 'item':
        if name in BASE_LOCK_TYPE or name not in scope.impl_files:
            return None
        return _advisory_text('repo', name, None)
    if kind == 'type':
        if LOCK_WORD_RE.search(payload):
            return None
        m = re.match(r'\s*([A-Za-z_]\w*)', payload)
        if not m or m.group(1) not in scope.impl_files:
            return None
        return _advisory_text('repo', m.group(1), None)
    if kind == 'import':
        segs = [s for s in payload.split('::') if s]
        return _walk_import(scope, file, segs, depth + 1, seen)
    return None


def suggested_exempt(lines, idx, file, scope):
    """Advisory only: a short suggestion where an `X::into_inner`
    receiver resolves, exactly and to the end, to a std type that
    cannot return a `PoisonError` or to a repo type with an in-repo
    `into_inner`. It never changes `needs_review` or `action`: the
    detector reads text, not types, so only the phase 2
    compiler-checked door (FD-2p) may clear a site."""
    if 'into_inner' not in lines[idx]:
        return None
    path = PATH_INTO.search(lines[idx])
    if not path:
        return None
    segs = path.group(1).split('::')
    root, last = segs[0], segs[-1]
    gens, _sig = scope.enclosing_generics(lines, idx)
    if last in gens:
        return None
    if root in EXTERN_ROOTS:
        if last in STD_NON_POISON:
            return _advisory_text(
                'std', last, 'std path written at the call site')
        return None
    if len(segs) > 1:
        return _walk_import(scope, file, segs, 0, frozenset())
    occ = _resolve_visible(scope, file, last)
    if occ is None:
        return None
    bind_file, (kind, payload) = occ
    if kind in ('item', 'type'):
        return _verify_named(scope, bind_file, last, 0, frozenset())
    if kind == 'import':
        return _walk_import(scope, bind_file,
                            [s for s in payload.split('::') if s],
                            0, frozenset())
    if kind == 'glob':
        gkind, target = scope.glob_source(bind_file, payload)
        if gkind == 'std':
            if last in STD_NON_POISON:
                return _advisory_text('std', last, 'std glob import')
            return None
        if gkind in ('crate', 'crate-external'):
            return _verify_named(scope, target, last, 0, frozenset())
    return None


def poison_recovery(lines, idx):
    """'certain' when the enclosing function shows a positive poison
    signal, 'unknown' when the detector sees neither proof nor signal."""
    for j in range(idx, -1, -1):
        if FN.match(lines[j].strip()):
            start, end = fn_range(lines, j)
            return ('certain' if POISON_SIGNAL.search(
                '\n'.join(lines[start:end + 1])) else 'unknown')
    return 'unknown'


def poison_aliases(tree):
    """Names bound to PoisonError through a `use ... PoisonError as X;` form."""
    aliases = set()
    for flines in tree.values():
        for line in flines:
            if 'PoisonError' in line and re.search(r'\buse\b', line):
                for m in POISON_ALIAS.finditer(line):
                    aliases.add(m.group(1))
    return aliases


def decide_site(f, lines, idx, recv, start, scope, aliases):
    """The site record before finish_site: the only place a site's kind,
    state_bearing, needs_review, poison_recovery and suggested_exempt
    are decided. The inventory lists candidates and never clears one:
    every site is needs_review, and suggested_exempt is advisory only
    (it never changes needs_review or action)."""
    reason = classify(lines, idx, recv, aliases)
    if reason is None:
        state = (state_bearing(lines, idx, recv, start,
                               statement_end(lines, idx))[0]
                 if recv else True)
        return {'file': f, 'line': idx + 1,
                'function': enclosing_fn(lines, idx), 'guard': None,
                'kind': 'unresolved', 'state_bearing': state,
                'needs_review': True,
                'poison_recovery': poison_recovery(lines, idx),
                'suggested_exempt': (suggested_exempt(lines, idx, f, scope)
                                     if recv is None else None),
                'snippet': lines[idx].strip()[:100]}
    end = statement_end(lines, idx)
    dot_recv = recv if reason != 'path-poison' else None
    guard, kind = resolve_guard(lines, idx, dot_recv, start)
    state, _nr = state_bearing(lines, idx,
                               'PoisonError' if reason == 'path-poison' else recv,
                               start, end)
    return {'file': f, 'line': idx + 1,
            'function': enclosing_fn(lines, idx), 'guard': guard,
            'kind': kind, 'state_bearing': state, 'needs_review': True,
            'poison_recovery': ('certain' if kind in ('std-lock', 'channel-timeout')
                                else poison_recovery(lines, idx)),
            'suggested_exempt': (suggested_exempt(lines, idx, f, scope)
                                 if kind == 'unresolved' and recv is None
                                 else None)}


def build(root):
    tree, raw = load_files(root, discover(root))
    tfiles = test_files(tree)
    aliases = poison_aliases(tree)
    impl_files = impl_into_inner_types(tree)
    scope = Scope(tree, module_parents(tree, raw), impl_files)
    sites = []
    for f in tree:
        lines = tree[f]
        for idx, line in enumerate(lines):
            if DECL_LINE.match(line.strip()) or not CANDIDATE.search(line):
                continue
            recv = None
            for m in re.finditer(r'\.into_inner\(\)', line):
                recv = receiver_of(line, m.start())
                if recv:
                    break
            if not recv and line.lstrip().startswith('.into_inner'):
                recv = multiline_recv(lines, idx)
            site = decide_site(f, lines, idx, recv, statement_start(lines, idx),
                               scope, aliases)
            finish_site(site, tree, lines, idx, f, tfiles)
            sites.append(site)
    sites.sort(key=lambda s: (s['file'], s['line']))
    return sites


def finish_site(site, tree, lines, idx, f, tfiles):
    site['test'] = is_test_site(f, lines, idx, tfiles)
    fn_idx = idx
    for j in range(idx, -1, -1):
        if FN.match(lines[j].strip()):
            fn_idx = j
            break
    site['guard_type'] = guard_type(tree, f, lines, fn_idx, idx, site['guard'])
    scope = scope_of(site)
    if site['kind'] == 'unresolved':
        action = 'needs-review'
        if site['poison_recovery'] == 'certain':
            reason = ('A positive poison signal is visible in the enclosing '
                      'function, but the detector cannot classify the recovery; '
                      'a human must confirm whether a poisoned lock guard is '
                      'involved before allowlisting or conversion.')
        else:
            reason = ('No positive proof either way: the detector lists this '
                      'candidate but cannot classify the recovery; a human '
                      'must confirm whether a poisoned lock guard is involved '
                      'before allowlisting or conversion.')
    else:
        action, reason = action_of(site, scope)
    site['scope'] = scope
    site['action'] = action
    site['reason'] = reason
    site['note'] = ('Named in the design as the shared-inbox swallow; it is removed '
                    'in a later phase.'
                    if (f == 'crates/grapt-product/src/dogfood/mod.rs'
                        and site['function'] == 'lock_inbox') else '')



def render(sites):
    return json.dumps({
        'comment': ('Every into_inner, PoisonError, clear_poison and is_poisoned '
                    'line under crates/ outside declaration lines, with the '
                    'guarded value and a proposed action. Phase 0 of the '
                    'foundation design is inventory only: no site is converted '
                    'here. The detector lists candidates and never clears '
                    'one: text cannot resolve symbols, so there are no '
                    'exemptions; every site is needs_review, and the only '
                    'actions are needs-review and fail-closed. A site the '
                    'detector cannot classify is kind unresolved (recorded, '
                    'never dropped): it reads text, not types, and does not '
                    'expand macro-generated code. suggested_exempt is '
                    'advisory only: where an X::into_inner receiver '
                    'resolves, exactly and to the end, to a std type that '
                    'cannot return a PoisonError (ManuallyDrop, Rc, Arc) at '
                    'a written std path or std import, or to a repo type '
                    'whose definition this repo can see with an in-repo '
                    'into_inner, it carries a short suggestion; like '
                    'state_bearing it never changes needs_review or action, '
                    'and the phase 2 compiler-checked door (FD-2p) does the '
                    'real proving. poison_recovery is certain '
                    'where the site matches a positive poison shape '
                    '(std-lock, channel-timeout) or the enclosing function '
                    'shows a positive poison signal (PoisonError, Poisoned(, '
                    '.lock(), .read(), .write(), .try_lock()); it is unknown '
                    'otherwise; both stay needs_review. The detector reads '
                    'text, not types, and does not expand macro-generated '
                    'code; a type that reaches a site only through inference '
                    'stays unresolved. state_bearing defaults to true; a '
                    'recovery scope that only calls the documented read-only '
                    'guard methods (len, is_empty, get, contains_key, iter, '
                    'keys, values, first, last, clone, as_ref, as_str, '
                    'to_vec, to_owned, copied, cloned) is proven read-only '
                    'and false. Regenerate with '
                    '`python3 scripts/build/poison_inventory.py`; the gate '
                    'test fails if this file is stale.'),
        'kinds': {'std-lock': 'std Mutex/RwLock PoisonError recovery.',
                  'channel-timeout': 'crossbeam wait_timeout poisoned-wait recovery.',
                  'unresolved': 'A candidate the detector cannot classify; '
                                'needs_review, never dropped; '
                                'suggested_exempt is advisory only.'},
        'actions': {'fail-closed': 'Stop swallowing: let the caller fail.',
                    'needs-review': 'A candidate the detector cannot clear; '
                                    'a human must confirm it before '
                                    'allowlisting or conversion (the phase 2 '
                                    'compiler-checked door, FD-2p, does the '
                                    'real proving).'},
        'sites': sites,
    }, indent=1) + '\n'



def check(root, baseline=BASELINE):
    fresh = json.loads(render(build(root)))
    kept = json.loads(baseline.read_text(encoding='utf-8'))
    if fresh == kept:
        print(f'poison inventory: {len(fresh["sites"])} sites, up to date')
        return 0
    old = {(s['file'], s['line']): s for s in kept.get('sites', [])}
    new = {(s['file'], s['line']): s for s in fresh['sites']}
    for key in sorted(set(new) - set(old)):
        print(f'new site: {key[0]}:{key[1]}')
    for key in sorted(set(old) - set(new)):
        print(f'removed site: {key[0]}:{key[1]}')
    for key in sorted(set(old) & set(new)):
        for field in new[key]:
            if old[key].get(field) != new[key][field]:
                print(f'changed {key[0]}:{key[1]} {field}: '
                      f'{old[key].get(field)!r} -> {new[key][field]!r}')
    for field in ('comment', 'kinds', 'actions'):
        if kept.get(field) != fresh.get(field):
            print(f'changed {field}')
    print('poison inventory: stale; run `python3 scripts/build/poison_inventory.py`')
    return 1


def main(argv):
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', default=str(ROOT))
    parser.add_argument('--check', action='store_true')
    args = parser.parse_args(argv)
    root = Path(args.root)
    if args.check:
        return check(root)
    BASELINE.write_text(render(build(root)), encoding='utf-8')
    print(f'wrote {BASELINE.relative_to(ROOT)}')
    return 0


if __name__ == '__main__':
    sys.exit(main(sys.argv[1:]))
