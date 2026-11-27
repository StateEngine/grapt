# Instruction imports and scoped rules

## Relative `@path` imports

The instruction file selected by normal directory discovery can include another
UTF-8 instruction file with a standalone, column-one line:

```text
Use the project conventions.
@docs/conventions.md
@../shared/testing.md
```

The referenced file replaces that line, with a source-path heading. Expansion is
depth-first and preserves written order, including repeated imports. References
are relative to the **resolved containing file's directory**, including when a
source is a symlink. Imported files can themselves contain imports. This works
for ambient instruction files, directory instructions attached to reads, and
matching rule bodies.

Paths must use forward slashes and contain no whitespace, NUL, colon, absolute
prefix or `~` expansion. There is no URL fetching, environment expansion, glob
expansion, quoted-path syntax or `@import path` command. Inline `@mentions`,
indented code and fenced backtick/tilde examples are not imports. A column-one
line starting with `@` outside a fence is reserved: malformed syntax is an error.

Imports do not grant broader read authority:

- Ambient project imports, including `..` and symlink targets, must remain in
  the existing resolved project boundary.
- Instructions attached to file reads and their imports must remain in the
  resolved session/workspace read boundary.
- A global instruction file may already be a symlink into a dotfiles tree. Its
  imports are restricted to that resolved file's parent directory and subtree;
  this does not authorize arbitrary home-directory or absolute-path reads.

Each expansion permits at most 16 active files, 64 source occurrences (including
its root), and 1 MiB of source input. Every source and its expanded result is
limited to 64 KiB. Existing ambient 1 MiB and scoped-per-read 64 KiB aggregate
limits still apply. Canonical-path cycles, missing imports, invalid UTF-8,
unreadable/oversized files, malformed directives and escaping links are errors,
not truncated or silently skipped instructions. An ambient failure keeps the
existing `core/instructions` last-known value and unavailable notice; without a
previous value it fails preparation. Scoped failures appear as unavailable
instructions on the read, not as new ambient policy.

Instruction sources, imports and rule bodies are opened once through the same
guarded acquisition as skill resources. Containment, regular-file type and size
are checked on that acquired file, which stays open through the bounded read;
instruction bytes are not acquired through the shared pathname source cache.
Nonblocking opens refuse substituted FIFOs, and Windows retains link-free
directory pins and denies delete sharing until the read completes. Expansion
keeps the acquired source's resolved path and fixed scope rather than resolving
its authority again after reading.

## Path-scoped Markdown rules

Put direct Markdown children in `.grapt/rules/` under the session directory or
any descendant directory. Nested folders inside `rules/` are not recursively
scanned. No vendor-home, external configured directory or ancestor above the
session read boundary is newly searched. Rules do not need an AGENTS file to
be discovered and do not participate in its filename fallback selection.

Each rule requires exactly one YAML pattern key: standard `paths`, or the
compatible `globs` alias, followed by its instruction body:

```markdown
---
paths:
  - "src/**/*.rs"
  - "tests/**/*.rs"
---
Keep fallible APIs explicit.
@../../docs/rust-conventions.md
```

This example lives at `<session>/.grapt/rules/rust.md`, so its import points to
`<session>/docs/rust-conventions.md`. Either key accepts a single quoted string
or a nonempty list of strings. Supplying both `paths` and `globs`, even if one
value is null, is ambiguous and reported as unavailable rather than selecting
one key.
Patterns are case-sensitive, relative to the directory **containing `.grapt`**,
and use `/` separators: `*` does not cross a directory separator, `**` can, `?`
selects one non-separator character, and glob character classes and alternatives
are supported. A file matching any pattern receives the rule. There are no
negative patterns, absolute paths, backslashes, parent components, home or URL
expansion. Use 1–64 patterns, each at most 1024 bytes. Invalid YAML, duplicate
keys, YAML aliases, missing pattern keys, empty/non-string patterns and malformed
patterns are reported as unavailable, not treated as match-all rules. The
existing bounded frontmatter parser also caps nesting, parser events and
frontmatter bytes.
Both spellings retain the same guarded reads, containment and import limits:
16 active files, 64 source occurrences, 64 KiB per source and expanded result,
and 1 MiB aggregate import input, with 64 KiB scoped output per read.

Rules attach only to the initial `read` page or initial `read_lines` page for a
matching file, using the existing scoped instruction output. A nonmatching
rule's imports are not opened. Rules never enter the ambient instruction epoch,
so editing one does not change the unrelated ambient provider-cache prefix.
Malformed rule metadata is reported on reads in its scope because matching
cannot safely be determined, but does not change ambient instructions.

Discovery keeps the existing most-specific-directory-first ordering, including
rules at the session root; within a directory, rules are sorted by filename and
appear before its selected instruction file. The session-root selected file
remains ambient and is not repeated. Existing directory/conversation precedence
is unchanged. Each read scans at most 64 descendant directories plus the session
root, 256 rule-directory entries and 64 Markdown files, with at most 1 MiB of raw
rule text. Rule directories, rule files and import targets must resolve inside
the same read boundary. Discovery failures explicitly mark the remaining rule
walk incomplete instead of claiming that no rules exist.
