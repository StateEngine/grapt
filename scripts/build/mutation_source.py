"""Rust source text for the mutation inventory: literal and comment masking,
items and their bodies, the module tree (which files compile only under
cfg(test)), and cfg evaluation for code neither compiled run can see.

Text never decides what a door is; the compiler does that in
mutation_inventory.py. This module only places a compiler-reported span in
its function and finds what the compiler could not have seen.
"""
import posixpath
import re
from collections import defaultdict

TESTISH = re.compile(r'(^|/)(tests?|probes|fixtures|benches|examples)(/|$)|(^|/)tests?\.rs$|_tests?\.rs$')


class InventoryError(Exception):
    pass


INTERESTING = re.compile(r'/[/*]|[rb"\']')
RAW_OPEN = re.compile(r'b?r(#*)"')
CHAR_LIT = re.compile(r"'(?:\\(?:u\{[0-9a-fA-F]+\}|x[0-9a-fA-F]{2}|.)|[^\\'\n])'")


def mask(text):
    """Return (code, strings): code with comments and literals blanked to spaces
    (newlines kept, so offsets and lines survive) and the string literals as
    (start, end, value) with offsets into the original text."""
    out = list(text)
    strings = []
    i, n = 0, len(text)

    def blank(a, b):
        for k in range(a, b):
            if out[k] != '\n':
                out[k] = ' '

    while i < n:
        hit = INTERESTING.search(text, i)
        if hit is None:
            break
        i = hit.start()
        c = text[i]
        if text.startswith('//', i):
            j = text.find('\n', i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
        elif text.startswith('/*', i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith('/*', j):
                    depth, j = depth + 1, j + 2
                elif text.startswith('*/', j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            blank(i, j)
            i = j
        elif RAW_OPEN.match(text, i) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == '_')):
            m = RAW_OPEN.match(text, i)
            close = '"' + m.group(1)
            body = m.end()
            j = text.find(close, body)
            if j < 0:
                raise InventoryError('unterminated raw string at offset %d' % i)
            strings.append((i, j + len(close), text[body:j]))
            blank(i, j + len(close))
            i = j + len(close)
        elif c == '"' or (c == 'b' and text.startswith('b"', i) and (i == 0 or not (text[i - 1].isalnum() or text[i - 1] == '_'))):
            start = i
            j = i + (2 if c == 'b' else 1)
            value = []
            while j < n and text[j] != '"':
                if text[j] == '\\':
                    if text[j + 1] == '\n':
                        j += 2
                        while j < n and text[j] in ' \t\n':
                            j += 1
                        continue
                    value.append(text[j:j + 2])
                    j += 2
                else:
                    value.append(text[j])
                    j += 1
            strings.append((start, j + 1, ''.join(value)))
            blank(start, j + 1)
            i = j + 1
        elif c == "'":
            m = CHAR_LIT.match(text, i)
            if m:
                blank(i, m.end())
                i = m.end()
            else:
                i += 1  # a lifetime or label
        else:
            i += 1
    return ''.join(out), strings


TEST_ITEM = re.compile(r'\s*(?:#\[[^\]]*\]\s*)*(?:\{|(?:pub(?:\([^)]*\))?\s+)?'
                       r'(?:(?:async|unsafe|const|extern)\s+)*(?:fn|mod|impl|struct|enum|trait|union)\b)')
FN_HEAD = re.compile(r'\bfn\s+([A-Za-z_]\w*)')
IMPL_HEAD = re.compile(r'^[ \t]*(?:unsafe[ \t]+)?impl\b(?:\s*<[^{};]*?>)?\s+(?:([\w:]+)(?:<[^{};]*?>)?\s+for\s+)?'
                       r'(?:[\w]+::)*([A-Za-z_]\w*)', re.M)
TRAIT_HEAD = re.compile(r'\btrait\s+([A-Za-z_]\w*)')
MOD_HEAD = re.compile(r'\bmod\s+([A-Za-z_]\w*)\s*\{')


def brace_pairs(code):
    """Map every `{` offset to its matching `}` in one pass."""
    pairs, stack = {}, []
    for k, ch in enumerate(code):
        if ch == '{':
            stack.append(k)
        elif ch == '}':
            if not stack:
                raise InventoryError('unbalanced `}` at offset %d' % k)
            pairs[stack.pop()] = k
    if stack:
        raise InventoryError('unbalanced `{` at offset %d' % stack[-1])
    return pairs


def body_open(code, start):
    """Offset of the `{` opening the item that starts at `start`, or None for a
    bodiless declaration (`;` first at depth 0, ignoring generics/parens)."""
    depth = 0
    for k in range(start, len(code)):
        ch = code[k]
        if ch in '([<':
            depth += 1
        elif ch in ')]>':
            depth = max(0, depth - 1) if ch != '>' or code[k - 1] != '-' else depth
        elif ch == ';' and depth == 0:
            return None
        elif ch == '{' and depth == 0:
            return k
    return None


class Source:
    """One Rust file: masked code, line offsets, functions, impl and cfg(test) regions."""

    def __init__(self, rel, text):
        self.rel = rel
        self.text = text
        self.code, self.strings = mask(text)
        self.line_at = [0]
        for k, ch in enumerate(text):
            if ch == '\n':
                self.line_at.append(k + 1)
        self.fns = []      # (name, head, open, close, is_pub)
        self.impls = []    # (type, open, close, trait or None)
        self.tests = []    # (open, close) cfg(test) regions
        pairs = brace_pairs(self.code)
        match_brace = pairs.__getitem__
        for m in IMPL_HEAD.finditer(self.code):
            o = body_open(self.code, m.end())
            if o is not None:
                trait = m.group(1).split('::')[-1] if m.group(1) else None
                self.impls.append((m.group(2), o, match_brace(o), trait))
        for m in FN_HEAD.finditer(self.code):
            o = body_open(self.code, m.end())
            if o is None:
                continue
            line_start = self.code.rfind('\n', 0, m.start()) + 1
            prefix = self.code[line_start:m.start()]
            self.fns.append((m.group(1), m.start(), o, match_brace(o), 'pub' in prefix))
        for m in re.finditer(r'#\[cfg\(\s*test\s*\)\]', self.code):
            item = TEST_ITEM.match(self.code, m.end())
            if item is None:
                continue  # on a field, statement or expression: no item body
            o = item.end() - 1 if item.group(0).endswith('{') else body_open(self.code, item.end())
            if o is not None:
                self.tests.append((m.start(), match_brace(o)))

    def offset(self, line, col):
        return self.line_at[line - 1] + col - 1

    def line_of(self, offset):
        lo, hi = 0, len(self.line_at) - 1
        while lo < hi:
            mid = (lo + hi + 1) // 2
            if self.line_at[mid] <= offset:
                lo = mid
            else:
                hi = mid - 1
        return lo + 1

    def enclosing_fn(self, offset):
        best = None
        for fn in self.fns:
            if fn[1] <= offset <= fn[3] and (best is None or fn[1] > best[1]):
                best = fn
        return best

    def enclosing_impl(self, offset):
        best = None
        for imp in self.impls:
            if imp[1] <= offset <= imp[2] and (best is None or imp[1] > best[1]):
                best = imp
        return best[0] if best else None

    def enclosing_trait(self, offset):
        """The trait of the innermost `impl Trait for Type` around offset."""
        best = None
        for imp in self.impls:
            if imp[1] <= offset <= imp[2] and (best is None or imp[1] > best[1]):
                best = imp
        return best[3] if best else None

    def in_cfg_test(self, offset):
        return any(a <= offset <= b for a, b in self.tests)

    def in_signature(self, offset):
        fn = self.enclosing_fn(offset)
        return fn is not None and fn[1] <= offset < fn[2]

    def first_string_after(self, offset, limit=400):
        for start, end, value in self.strings:
            if offset <= start <= offset + limit:
                between = self.code[offset:start]
                if between.count(';') == 0:
                    return value
        return None


def module_of(rel):
    parts = rel.split('/')
    if len(parts) < 3 or parts[0] != 'crates':
        return rel
    crate, rest = parts[1], parts[2:]
    if rest[0] == 'src':
        rest = rest[1:]
        if rest and rest[-1] in ('lib.rs', 'main.rs', 'mod.rs'):
            rest = rest[:-1]
    else:
        rest = [rest[0]] + rest[1:]
        if rest[-1] == 'mod.rs':
            rest = rest[:-1]
    names = [p[:-3] if p.endswith('.rs') else p for p in rest]
    return '::'.join([crate.replace('-', '_')] + names)


TEST_MOD = re.compile(r'#\[cfg\(\s*test\s*\)\]\s*(?:#\[path\s*=\s*"([^"]+)"\]\s*|#\[[^\]]*\]\s*)*'
                      r'(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;')


def child_dir(rel):
    name = posixpath.basename(rel)
    base = posixpath.dirname(rel)
    if name in ('lib.rs', 'main.rs', 'mod.rs'):
        return base
    return posixpath.join(base, name[:-3])


ANY_MOD = re.compile(r'(?:#\[path\s*=\s*"([^"]+)"\]\s*|#\[[^\]]*\]\s*)*'
                     r'(?:pub(?:\([^)]*\))?\s+)?mod\s+([A-Za-z_]\w*)\s*;')


def declared(rel, m):
    if m.group(1):
        return [posixpath.normpath(posixpath.join(posixpath.dirname(rel), m.group(1)))]
    d = child_dir(rel)
    return [posixpath.join(d, m.group(2) + '.rs'), posixpath.join(d, m.group(2), 'mod.rs')]


def mark_test_files(sources):
    """A file declared by `#[cfg(test)] mod x;` (or with `#[path]`), every
    file a test file declares, and everything beneath them compile only under
    cfg(test). Resolved to a fixpoint over the module declarations."""
    test = {rel for rel in sources if TESTISH.search(rel.split('/src/', 1)[-1])}
    for rel, src in sources.items():
        for m in TEST_MOD.finditer(src.text):
            if src.code[m.start():m.start() + 1] == '#':
                test.update(c for c in declared(rel, m) if c in sources)
    changed = True
    while changed:
        changed = False
        for rel in list(test):
            src = sources.get(rel)
            if src is None:
                continue
            for m in ANY_MOD.finditer(src.code):
                for child in declared(rel, ANY_MOD.match(src.text, m.start()) or m):
                    if child in sources and child not in test:
                        test.add(child)
                        changed = True
    for rel, src in sources.items():
        src.test_file = rel in test


INCLUDE = re.compile(r'\binclude!\s*\(\s*"([^"]+)"\s*\)')


def mark_includes(sources):
    """A file pulled in with `include!` belongs to its includer's module (and
    its test status): `session_list/budget.rs` is `grapt_store::session_list`."""
    for src in sources.values():
        src.module = module_of(src.rel)
    parent = {}
    for rel, src in sources.items():
        for m in INCLUDE.finditer(src.text):
            if src.code[m.start():m.start() + 1] == ' ':
                continue  # inside a comment or string
            child = posixpath.normpath(posixpath.join(posixpath.dirname(rel), m.group(1)))
            if child in sources:
                parent[child] = rel
    for child in parent:
        root, seen = parent[child], {child}
        while root in parent and root not in seen:
            seen.add(root)
            root = parent[root]
        sources[child].module = sources[root].module
        if getattr(sources[root], 'test_file', False):
            sources[child].test_file = True

