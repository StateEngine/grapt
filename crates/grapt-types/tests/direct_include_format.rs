//! Repository-wide formatting gate for Rust source assembled with `include!`.
//!
//! Cargo/rustfmt do not format fragments that are visible only through a macro.
//! This test inventories the complete bounded `crates/**/*.rs` tree, recognizes
//! literal include sites with a byte lexer, reconstructs each logical carrier,
//! and requires pinned rustfmt stdout to be byte-identical to canonical-LF input.

use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// These caps bound this harness's memory and walk, not repository size;
// no_god_files is the size policy.
const MAX_DEPTH: usize = 12;
const MAX_DIRECTORIES: usize = 2_048;
const MAX_ENTRIES: usize = 16_384;
/// Bound on the ignored-path listing this harness will read from git's STDOUT.
///
/// Bounded like every other read here: an unbounded listing is an unbounded
/// allocation driven by whatever build artifacts happen to exist on the machine.
const MAX_IGNORED_LISTING_BYTES: usize = 256 * 1024;
/// Bound on git's STDERR while producing that listing.
///
/// A separate cap, because it bounds a separate pipe. The first version of this
/// code capped stdout and left stderr entirely unbounded — and did even that
/// after the fact. See [`capture_bounded`].
const MAX_IGNORED_STDERR_BYTES: usize = 64 * 1024;
const MAX_FILES: usize = 8_192;
const MAX_FILE_BYTES: usize = 1_048_576;
const MAX_TOTAL_FILE_BYTES: usize = 134_217_728;
const MAX_CARRIERS: usize = 256;
const MAX_SITES: usize = 1_024;
const MAX_FRAGMENT_BYTES: usize = 16_777_216;
const MAX_EXPANDED_BYTES: usize = 4_194_304;
const MAX_TOTAL_EXPANDED_BYTES: usize = 33_554_432;
const MAX_DIAGNOSTIC_BYTES: usize = 65_536;
const MAX_SUBPROCESSES: usize = 257;
const FORMATTER_BYTE_LIMIT: usize = MAX_EXPANDED_BYTES + MAX_DIAGNOSTIC_BYTES;
// Review observed an 11.17s precheck failure followed by a 4.10s clean rerun
// under gate load. The original 10s sleep
// budget dates to e4428865; allow 60s without weakening format comparison.
// Each sleep lasts at least 25ms; 2400 probes reserve at least 60s under load
//. Keep one wait mechanism.
const FORMATTER_WAIT_SECONDS: usize = 60;
const FORMATTER_POLLS: usize = FORMATTER_WAIT_SECONDS * 40;
const FORMATTER_POLL_DELAY: Duration = Duration::from_millis(25);
const EXPECTED_VERSION: &[u8] = b"rustfmt 1.9.0-stable (8bab26f4f6 2026-07-14)\n";

#[derive(Debug, Clone, PartialEq, Eq)]
struct Site {
    carrier: String,
    target: String,
    start: usize,
    end: usize,
}

#[derive(Debug)]
struct Inventory {
    files: BTreeMap<String, Vec<u8>>,
    directories: usize,
    entries: usize,
    total_bytes: usize,
    /// Filesystem entries excluded because git ignores them.
    ///
    /// VISIBLE, not silent. An exclusion nobody can count is an exclusion
    /// nobody can witness — and the poisoned-sequence witness needs to prove
    /// the artifact was actually present and actually skipped, or it would pass
    /// just as happily against a tree where nothing was created.
    ignored_skipped: usize,
}

#[derive(Debug, Clone, Copy)]
struct EntryFacts {
    symlink: bool,
    reparse: bool,
    contained: bool,
}

#[derive(Debug)]
struct ProcessResult {
    status: i32,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Serializes every test that WALKS the source tree against every test that
/// MUTATES it.
///
/// Cargo runs the tests in one binary on parallel threads. The ignored-artifact
/// witness necessarily creates a real directory under `crates/`, and
/// [`inventory`] snapshots the ignored set ONCE at the start of a walk — so a
/// directory created after that snapshot but before the walk reaches it is
/// counted, and the pinned census moves under a test that changed nothing.
///
/// That is not hypothetical: it was measured at 219 directories against a pinned
/// 218, roughly one run in a dozen, and it appeared only when the suite was run
/// repeatedly. One clean run would have shipped it.
///
/// The alternative — making the walk tolerate entries that appear mid-walk —
/// was rejected. It would mask a genuine race behind a retry and would weaken
/// the very census this file exists to keep exact. Serializing says plainly that
/// these tests share one mutable resource.
static TREE: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Take the tree lock, ignoring poisoning.
///
/// A panicking test has already reported its own failure; letting its poison
/// turn every sibling red as well would bury the real one under a cascade.
fn lock_tree() -> std::sync::MutexGuard<'static, ()> {
    TREE.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn add_bounded(current: &mut usize, amount: usize, cap: usize, label: &str) -> Result<(), String> {
    let next = current
        .checked_add(amount)
        .ok_or_else(|| format!("{label} count overflow"))?;
    if next > cap {
        return Err(format!("{label} exceeds bound {cap}: {next}"));
    }
    *current = next;
    Ok(())
}

fn canonical_lf(bytes: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    out.try_reserve_exact(bytes.len())
        .map_err(|error| format!("cannot reserve {} canonical-LF bytes: {error}", bytes.len()))?;
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes[cursor] == b'\r' && bytes.get(cursor + 1) == Some(&b'\n') {
            out.push(b'\n');
            cursor += 2;
        } else {
            out.push(bytes[cursor]);
            cursor += 1;
        }
    }
    Ok(out)
}

fn read_bounded(path: &Path, cap: usize) -> Result<Vec<u8>, String> {
    let file = map_read_result(File::open(path), path, "open")?;
    let take = u64::try_from(cap)
        .map_err(|_| "read cap does not fit u64".to_owned())?
        .checked_add(1)
        .ok_or_else(|| "read cap overflow".to_owned())?;
    let mut bytes = Vec::new();
    map_read_result(file.take(take).read_to_end(&mut bytes), path, "read")?;
    if bytes.len() > cap {
        return Err(format!("{} exceeds byte bound {cap}", path.display()));
    }
    Ok(bytes)
}

fn map_read_result<T>(
    result: std::io::Result<T>,
    path: &Path,
    operation: &str,
) -> Result<T, String> {
    result.map_err(|error| format!("cannot {operation} {}: {error}", path.display()))
}

fn require_utf8<'a>(bytes: &'a [u8], path: &str) -> Result<&'a str, String> {
    std::str::from_utf8(bytes).map_err(|error| format!("{path} is not UTF-8: {error}"))
}

fn insert_case_folded(folded: &mut BTreeMap<String, String>, path: &str) -> Result<(), String> {
    let fold = path.to_ascii_lowercase();
    if let Some(previous) = folded.insert(fold, path.to_owned()) {
        return Err(format!("ASCII-case path collision: {previous} and {path}"));
    }
    Ok(())
}

fn is_link_or_reparse(path: &Path, kind: fs::FileType) -> Result<bool, String> {
    if kind.is_symlink() {
        return Ok(true);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        let metadata = fs::symlink_metadata(path)
            .map_err(|error| format!("cannot inspect {}: {error}", path.display()))?;
        Ok(metadata.file_attributes() & 0x400 != 0)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        Ok(false)
    }
}

fn normalized_relative(root: &Path, path: &Path) -> Result<String, String> {
    let relative = path
        .strip_prefix(root)
        .map_err(|error| format!("cannot relativize {}: {error}", path.display()))?;
    let text = relative.to_string_lossy().replace('\\', "/");
    if text.bytes().any(|byte| !byte.is_ascii()) {
        return Err(format!("non-ASCII repository path is refused: {text}"));
    }
    Ok(text)
}

fn inventory(root: &Path) -> Result<Inventory, String> {
    let crates = root.join("crates");
    let canonical_crates = fs::canonicalize(&crates)
        .map_err(|error| format!("cannot canonicalize {}: {error}", crates.display()))?;
    let ignored = ignored_relatives(root)?;
    let mut stack = vec![(crates, 0_usize)];
    let mut directories = 1;
    let mut entries = 0;
    let mut total_bytes = 0;
    let mut ignored_skipped = 0;
    let mut files = BTreeMap::new();
    let mut folded = BTreeMap::new();
    while let Some((directory, depth)) = stack.pop() {
        let mut children = Vec::new();
        for entry in fs::read_dir(&directory)
            .map_err(|error| format!("cannot read {}: {error}", directory.display()))?
        {
            let entry = entry.map_err(|error| format!("cannot read directory entry: {error}"))?;
            // THE POLICY, applied BEFORE the entry is counted. Counting first and
            // filtering later would leave `entries` carrying the artifact, which
            // is the whole defect.
            let relative = normalized_relative(root, &entry.path())?;
            if is_ignored(&relative, &ignored) {
                add_bounded(&mut ignored_skipped, 1, MAX_ENTRIES, "ignored entries")?;
                continue;
            }
            add_bounded(&mut entries, 1, MAX_ENTRIES, "entries")?;
            children.push(entry);
        }
        children.sort_by_key(fs::DirEntry::file_name);
        for entry in children.into_iter().rev() {
            let path = entry.path();
            let kind = entry
                .file_type()
                .map_err(|error| format!("cannot stat {}: {error}", path.display()))?;
            if is_link_or_reparse(&path, kind)? {
                return Err(format!(
                    "source inventory refuses link or reparse point {}",
                    path.display()
                ));
            }
            let resolved = fs::canonicalize(&path)
                .map_err(|error| format!("cannot resolve {}: {error}", path.display()))?;
            if !resolved.starts_with(&canonical_crates) {
                return Err(format!(
                    "source inventory escapes crates: {}",
                    path.display()
                ));
            }
            if kind.is_dir() {
                let child_depth = depth
                    .checked_add(1)
                    .ok_or_else(|| "directory depth overflow".to_owned())?;
                if child_depth > MAX_DEPTH {
                    return Err(format!(
                        "directory depth exceeds {MAX_DEPTH}: {}",
                        path.display()
                    ));
                }
                add_bounded(&mut directories, 1, MAX_DIRECTORIES, "directories")?;
                stack.push((path, child_depth));
            } else if kind.is_file() && path.extension().is_some_and(|extension| extension == "rs")
            {
                if files.len() >= MAX_FILES {
                    return Err(format!("files exceeds bound {MAX_FILES}"));
                }
                let relative = normalized_relative(root, &path)?;
                insert_case_folded(&mut folded, &relative)?;
                let remaining_total = MAX_TOTAL_FILE_BYTES
                    .checked_sub(total_bytes)
                    .ok_or_else(|| "total file bytes already exceed bound".to_owned())?;
                let raw_bytes = read_bounded(&path, MAX_FILE_BYTES.min(remaining_total))?;
                add_bounded(
                    &mut total_bytes,
                    raw_bytes.len(),
                    MAX_TOTAL_FILE_BYTES,
                    "total file bytes",
                )?;
                let bytes = canonical_lf(&raw_bytes)?;
                require_utf8(&bytes, &relative)?;
                files.insert(relative, bytes);
            }
        }
    }
    Ok(Inventory {
        files,
        directories,
        entries,
        total_bytes,
        ignored_skipped,
    })
}

fn is_ident(byte: u8) -> bool {
    byte == b'_' || byte.is_ascii_alphanumeric()
}

fn quoted_end(bytes: &[u8], start: usize, quote: u8) -> Option<usize> {
    let mut cursor = start + 1;
    while cursor < bytes.len() {
        match bytes[cursor] {
            b'\\' => cursor = cursor.checked_add(2)?,
            byte if byte == quote => return cursor.checked_add(1),
            b'\n' if quote == b'\'' => return None,
            _ => cursor += 1,
        }
    }
    None
}

fn raw_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut cursor = start;
    if matches!(bytes.get(cursor), Some(b'b' | b'c')) {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'r') {
        return None;
    }
    cursor += 1;
    let hash_start = cursor;
    while bytes.get(cursor) == Some(&b'#') {
        cursor += 1;
    }
    if bytes.get(cursor) != Some(&b'"') {
        return None;
    }
    let hashes = cursor - hash_start;
    cursor += 1;
    while cursor < bytes.len() {
        if bytes[cursor] == b'"'
            && bytes.get(cursor + 1..cursor + 1 + hashes)
                == Some(&bytes[hash_start..hash_start + hashes])
        {
            return cursor.checked_add(1 + hashes);
        }
        cursor += 1;
    }
    None
}

fn block_comment_end(bytes: &[u8], start: usize) -> Result<usize, String> {
    let mut depth = 1_usize;
    let mut cursor = start + 2;
    while cursor + 1 < bytes.len() {
        if &bytes[cursor..cursor + 2] == b"/*" {
            depth = depth
                .checked_add(1)
                .ok_or_else(|| "block-comment depth overflow".to_owned())?;
            cursor += 2;
        } else if &bytes[cursor..cursor + 2] == b"*/" {
            depth -= 1;
            cursor += 2;
            if depth == 0 {
                return Ok(cursor);
            }
        } else {
            cursor += 1;
        }
    }
    Err("unterminated block comment".to_owned())
}

fn skip_trivia(bytes: &[u8], mut cursor: usize) -> Result<usize, String> {
    loop {
        while bytes.get(cursor).is_some_and(u8::is_ascii_whitespace) {
            cursor += 1;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            cursor = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| cursor + offset + 1);
        } else if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            cursor = block_comment_end(bytes, cursor)?;
        } else {
            return Ok(cursor);
        }
    }
}

fn validate_include_path(path: &str) -> Result<(), String> {
    if path.is_empty() || path.starts_with('/') || path.contains('\\') || path.contains('\0') {
        return Err(format!("invalid include path {path:?}"));
    }
    if !path.ends_with(".rs") {
        return Err(format!("include target is not .rs: {path:?}"));
    }
    for segment in path.split('/') {
        if segment.is_empty() || matches!(segment, "." | "..") || segment.contains(':') {
            return Err(format!("invalid include path segment in {path:?}"));
        }
    }
    Ok(())
}

fn parse_include_line(line: &[u8]) -> Result<String, String> {
    let trimmed = line.strip_suffix(b"\n").unwrap_or(line);
    let trimmed = trimmed.strip_suffix(b"\r").unwrap_or(trimmed);
    let start = trimmed
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(trimmed.len());
    let end = trimmed
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    let canonical = &trimmed[start..end];
    let prefix = b"include!(\"";
    let suffix = b"\");";
    if !canonical.starts_with(prefix) || !canonical.ends_with(suffix) {
        return Err(
            "include macro must occupy one canonical include!(\"path.rs\"); line".to_owned(),
        );
    }
    let path = &canonical[prefix.len()..canonical.len() - suffix.len()];
    let path = std::str::from_utf8(path).map_err(|_| "include path is not UTF-8".to_owned())?;
    validate_include_path(path)?;
    Ok(path.to_owned())
}

fn char_literal_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'\'') {
        return None;
    }
    let next = *bytes.get(start + 1)?;
    if next == b'\'' || next == b'\n' || next == b'\r' {
        return None;
    }
    if next == b'\\' {
        // Bounded Rust character escape sequence (`'\''`, `'\n'`, `'\x7f'`, `'\u{10ffff}'`).
        let mut cursor = start + 2;
        if bytes.get(cursor).is_none_or(|b| *b == b'\n' || *b == b'\r') {
            return None;
        }
        cursor += 1;
        let limit = (start + 16).min(bytes.len());
        while cursor < limit {
            match bytes[cursor] {
                b'\'' => return Some(cursor + 1),
                b'\n' | b'\r' => return None,
                _ => cursor += 1,
            }
        }
        return None;
    }
    // Unescaped single UTF-8 scalar: the byte immediately after the scalar must be `'`.
    let char_len = if next < 0x80 {
        1
    } else if next & 0xE0 == 0xC0 {
        2
    } else if next & 0xF0 == 0xE0 {
        3
    } else if next & 0xF8 == 0xF0 {
        4
    } else {
        return None;
    };
    let end_char = start + 1 + char_len;
    if std::str::from_utf8(bytes.get(start + 1..end_char)?).is_err() {
        return None;
    }
    (bytes.get(end_char) == Some(&b'\'')).then_some(end_char + 1)
}

fn detect_sites(carrier: &str, bytes: &[u8]) -> Result<Vec<Site>, String> {
    let mut sites = Vec::new();
    let mut cursor = 0;
    while cursor < bytes.len() {
        if bytes.get(cursor..cursor + 2) == Some(b"//") {
            cursor = bytes[cursor..]
                .iter()
                .position(|byte| *byte == b'\n')
                .map_or(bytes.len(), |offset| cursor + offset + 1);
            continue;
        }
        if bytes.get(cursor..cursor + 2) == Some(b"/*") {
            cursor = block_comment_end(bytes, cursor)?;
            continue;
        }
        if let Some(end) = raw_end(bytes, cursor) {
            cursor = end;
            continue;
        }
        let quoted = if bytes[cursor] == b'"' {
            quoted_end(bytes, cursor, b'"')
        } else if matches!(bytes.get(cursor..cursor + 2), Some(b"b\"") | Some(b"c\"")) {
            quoted_end(bytes, cursor + 1, b'"')
        } else if bytes.get(cursor..cursor + 2) == Some(b"b'") {
            char_literal_end(bytes, cursor + 1)
        } else if bytes[cursor] == b'\'' {
            char_literal_end(bytes, cursor)
        } else {
            None
        };
        if let Some(end) = quoted {
            cursor = end;
            continue;
        }
        if bytes.get(cursor..cursor + 7) == Some(b"include")
            && (cursor == 0 || !is_ident(bytes[cursor - 1]))
            && bytes.get(cursor + 7).is_none_or(|byte| !is_ident(*byte))
        {
            let bang = skip_trivia(bytes, cursor + 7)?;
            if bytes.get(bang) == Some(&b'!') {
                let line_start = bytes[..cursor]
                    .iter()
                    .rposition(|byte| *byte == b'\n')
                    .map_or(0, |index| index + 1);
                let line_end = bytes[cursor..]
                    .iter()
                    .position(|byte| *byte == b'\n')
                    .map_or(bytes.len(), |offset| cursor + offset + 1);
                let relative = parse_include_line(&bytes[line_start..line_end])?;
                let parent = Path::new(carrier)
                    .parent()
                    .ok_or_else(|| format!("carrier has no parent: {carrier}"))?;
                let target = parent.join(relative).to_string_lossy().replace('\\', "/");
                if sites.len() >= MAX_SITES {
                    return Err(format!("include sites exceeds bound {MAX_SITES}"));
                }
                sites.push(Site {
                    carrier: carrier.to_owned(),
                    target,
                    start: line_start,
                    end: line_end,
                });
                cursor = line_end;
                continue;
            }
        }
        cursor += 1;
    }
    Ok(sites)
}

fn admit_entry(facts: EntryFacts, label: &str) -> Result<(), String> {
    if facts.symlink || facts.reparse {
        return Err(format!("{label} is a link or reparse point"));
    }
    if !facts.contained {
        return Err(format!("{label} resolves outside crates"));
    }
    Ok(())
}

fn resolve_target(
    root: &Path,
    carrier: &str,
    target: &str,
    files: &BTreeMap<String, Vec<u8>>,
) -> Result<(), String> {
    let relative = Path::new(target);
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(format!("non-normal target path {target}"));
    }
    let crates = fs::canonicalize(root.join("crates"))
        .map_err(|error| format!("cannot resolve crates: {error}"))?;
    let mut walked = root.to_path_buf();
    for component in relative.components() {
        if let Component::Normal(name) = component {
            walked.push(name);
            let metadata = fs::symlink_metadata(&walked)
                .map_err(|error| format!("{carrier} includes missing target {target}: {error}"))?;
            admit_entry(
                EntryFacts {
                    symlink: metadata.file_type().is_symlink(),
                    reparse: is_link_or_reparse(&walked, metadata.file_type())?,
                    contained: fs::canonicalize(&walked)
                        .is_ok_and(|path| path.starts_with(&crates)),
                },
                target,
            )?;
        }
    }
    require_inventory_target(files, target).map_err(|error| format!("{carrier}: {error}"))?;
    Ok(())
}

fn require_inventory_target(files: &BTreeMap<String, Vec<u8>>, target: &str) -> Result<(), String> {
    if !files.contains_key(target) {
        return Err(format!("includes target outside Rust inventory: {target}"));
    }
    Ok(())
}

fn reject_graph_cycles(sites: &[Site]) -> Result<(), String> {
    let carriers: BTreeSet<&str> = sites.iter().map(|site| site.carrier.as_str()).collect();
    for site in sites {
        if carriers.contains(site.target.as_str()) {
            return Err(format!(
                "include target/carrier intersection: {} -> {}",
                site.carrier, site.target
            ));
        }
    }
    Ok(())
}

fn expand_carrier(
    source: &[u8],
    sites: &[Site],
    files: &BTreeMap<String, Vec<u8>>,
    cap: usize,
    total_expanded: &mut usize,
    total_cap: usize,
) -> Result<Vec<u8>, String> {
    let expanded_len = plan_expanded_len(source, sites, files, cap)?;
    let next_total = admit_expanded_append(*total_expanded, expanded_len, total_cap)?;
    let mut expanded = Vec::new();
    expanded.try_reserve_exact(expanded_len).map_err(|error| {
        format!("cannot reserve {expanded_len} expanded carrier bytes: {error}")
    })?;
    let mut cursor = 0;
    for site in sites {
        expanded.extend_from_slice(&source[cursor..site.start]);
        let fragment = files
            .get(&site.target)
            .ok_or_else(|| format!("missing target {}", site.target))?;
        expanded.extend_from_slice(fragment);
        cursor = site.end;
    }
    expanded.extend_from_slice(&source[cursor..]);
    *total_expanded = next_total;
    Ok(expanded)
}

fn plan_expanded_len(
    source: &[u8],
    sites: &[Site],
    files: &BTreeMap<String, Vec<u8>>,
    cap: usize,
) -> Result<usize, String> {
    let mut cursor = 0;
    let mut expanded_len = 0;
    for site in sites {
        if site.start < cursor || site.start > site.end || site.end > source.len() {
            return Err(format!(
                "overlapping or invalid include span in {}",
                site.carrier
            ));
        }
        expanded_len = admit_expanded_append(expanded_len, site.start - cursor, cap)?;
        let fragment = files
            .get(&site.target)
            .ok_or_else(|| format!("missing target {}", site.target))?;
        expanded_len = admit_expanded_append(expanded_len, fragment.len(), cap)?;
        cursor = site.end;
    }
    admit_expanded_append(expanded_len, source.len() - cursor, cap)
}

fn admit_expanded_append(current: usize, amount: usize, cap: usize) -> Result<usize, String> {
    let next = current
        .checked_add(amount)
        .ok_or_else(|| "expanded carrier byte count overflow".to_owned())?;
    if next > cap {
        return Err(format!("expanded carrier exceeds {cap} bytes: {next}"));
    }
    Ok(next)
}

fn insert_bounded_carrier<'a>(
    carriers: &mut BTreeSet<&'a str>,
    carrier: &'a str,
) -> Result<(), String> {
    if !carriers.contains(carrier) && carriers.len() >= MAX_CARRIERS {
        return Err(format!("carriers exceeds bound {MAX_CARRIERS}"));
    }
    carriers.insert(carrier);
    Ok(())
}

/// Inject observation and sleeping so poll-bound cases need no real delay.
fn poll_formatter<T>(
    mut probe: impl FnMut() -> Result<Option<T>, String>,
    mut pause: impl FnMut(),
) -> Result<T, String> {
    for _ in 0..FORMATTER_POLLS {
        if let Some(status) = probe()? {
            return Ok(status);
        }
        pause();
    }
    // A child that exited during the last sleep must not be called timed out.
    probe()?.ok_or_else(|| {
        format!("rustfmt timed out after its {FORMATTER_WAIT_SECONDS}s poll sleep budget")
    })
}

fn require_carrier_format(
    carrier: &str,
    fragments: &[&str],
    expanded: &[u8],
    formatted: Result<Vec<u8>, String>,
) -> Result<(), String> {
    let formatted = formatted.map_err(|error| format!("{carrier}: {error}"))?;
    if formatted != expanded {
        return Err(bounded_diagnostic(
            &format!("logical carrier {carrier} is not pinned-rustfmt clean; fragments: "),
            fragments,
        )?);
    }
    Ok(())
}

fn run_bounded(args: &[&str], input: Option<&[u8]>) -> Result<ProcessResult, String> {
    let mut child = Command::new("rustup")
        .args(["run", "1.97.1", "rustfmt"])
        .args(args)
        .stdin(if input.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| format!("rustfmt unavailable: {error}"))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| "rustfmt stdout pipe absent".to_owned())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| "rustfmt stderr pipe absent".to_owned())?;
    let oversized = Arc::new(AtomicBool::new(false));
    let out_flag = Arc::clone(&oversized);
    let err_flag = Arc::clone(&oversized);
    let out_thread = thread::spawn(move || drain_pipe(stdout, FORMATTER_BYTE_LIMIT, out_flag));
    let err_thread = thread::spawn(move || drain_pipe(stderr, MAX_DIAGNOSTIC_BYTES, err_flag));
    let input_bytes = input.map(ToOwned::to_owned);
    let stdin = child.stdin.take();
    let input_thread = thread::spawn(move || -> Result<(), String> {
        if let (Some(mut pipe), Some(bytes)) = (stdin, input_bytes) {
            pipe.write_all(&bytes)
                .map_err(|error| format!("rustfmt stdin write failed: {error}"))?;
        }
        Ok(())
    });
    let completion = poll_formatter(
        || {
            if oversized.load(Ordering::SeqCst) {
                return Err("rustfmt stdout/stderr exceeded its bounded pipe".to_owned());
            }
            child
                .try_wait()
                .map_err(|error| format!("cannot poll rustfmt: {error}"))
        },
        || thread::sleep(FORMATTER_POLL_DELAY),
    );
    let mut forced_error = None;
    let status = match completion {
        Ok(status) => status,
        Err(error) => {
            // Reap before joining pipes, including on poll and overflow errors.
            let _ = child.kill();
            let status = child
                .wait()
                .map_err(|error| format!("cannot wait for stopped rustfmt: {error}"))?;
            forced_error = Some(error);
            status
        }
    };
    let input_result = input_thread
        .join()
        .map_err(|_| "rustfmt stdin thread panicked".to_owned())?;
    let stdout = out_thread
        .join()
        .map_err(|_| "rustfmt stdout thread panicked".to_owned())??;
    let stderr = err_thread
        .join()
        .map_err(|_| "rustfmt stderr thread panicked".to_owned())??;
    if let Some(error) = forced_error {
        return Err(error);
    }
    input_result?;
    if oversized.load(Ordering::SeqCst) {
        return Err("rustfmt stdout/stderr exceeded its bounded pipe".to_owned());
    }
    Ok(ProcessResult {
        status: status.code().unwrap_or(-1),
        stdout,
        stderr,
    })
}

fn validate_formatter_result(result: ProcessResult) -> Result<Vec<u8>, String> {
    if result.status != 0 {
        return Err(format!(
            "rustfmt exited {}: {}",
            result.status,
            String::from_utf8_lossy(&result.stderr)
        ));
    }
    Ok(result.stdout)
}

fn format_bytes(input: &[u8]) -> Result<Vec<u8>, String> {
    validate_formatter_result(run_bounded(
        &[
            "--emit",
            "stdout",
            "--edition",
            "2021",
            "--style-edition",
            "2021",
            "--config",
            "newline_style=Unix,skip_children=true",
            "--color",
            "never",
        ],
        Some(input),
    )?)
}

fn physical_lines(bytes: &[u8]) -> usize {
    bytes.iter().filter(|byte| **byte == b'\n').count()
        + usize::from(!bytes.is_empty() && !bytes.ends_with(b"\n"))
}

fn bounded_diagnostic(prefix: &str, details: &[&str]) -> Result<String, String> {
    let mut used = 0;
    add_bounded(
        &mut used,
        prefix.len(),
        MAX_DIAGNOSTIC_BYTES,
        "diagnostic bytes",
    )?;
    let mut message = prefix.to_owned();
    for detail in details {
        let detail_bytes = detail
            .len()
            .checked_add(2)
            .ok_or_else(|| "diagnostic detail length overflow".to_owned())?;
        add_bounded(
            &mut used,
            detail_bytes,
            MAX_DIAGNOSTIC_BYTES,
            "diagnostic bytes",
        )?;
        if message.len() > prefix.len() {
            message.push_str(", ");
        }
        message.push_str(detail);
    }
    Ok(message)
}

/// The bounded child-capture seam, split out when this file crossed the
/// god-file ceiling. The ratchet refused the growth and it was right to:
/// bounded acquisition is its own subsystem — a failure vocabulary, a capture
/// routine and the ignored-path policy built on it — not a paragraph of a file
/// about include formatting.
#[path = "direct_include_format/capture.rs"]
mod capture;
pub(crate) use capture::*;

#[path = "direct_include_format/cases.rs"]
mod cases;

#[path = "direct_include_format/ratchet.rs"]
mod ratchet;
