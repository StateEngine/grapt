// Copyright 2025 OpenAI. Licensed under the Apache License, Version 2.0.
// Best-effort entry handling and explicit partial-result reporting. Grapt retains its bounded,
// deterministic traversal and authorized workspace effect path.
//! Deterministic, bounded traversal shared by glob and literal grep.
//!
//! The application resolves and authorizes the workspace and search scope before
//! calling these effects; traversal rechecks links while executing the approved scope.

#[cfg(test)]
mod bounded_read_tests;
mod ignore_rules;
mod list;

pub use list::list;

use crate::filesystem::{is_link, read_bounded, reject_links};
use crate::FILE_EFFECT_MAX_BYTES;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use self::ignore_rules::Rules;
use self::ignore_rules::{ignored, load as load_ignore};

pub const RESULT_LIMIT: u64 = 1_000;
pub const OUTPUT_BYTES: usize = 256 * 1024;
pub const PATH_BYTES: usize = 4096;

const ENTRIES: usize = 10_000;
const DEPTH: usize = 32;
const SCAN_BYTES: u64 = 16 * 1024 * 1024;
const PATTERN_BYTES: usize = 256;
/// Long lines (minified or generated code) return a prefix; read_lines has the rest.
const MATCH_TEXT_BYTES: usize = 1024;
/// Grep output outside its two row arrays: keys, counts and the truncation hint.
const ENVELOPE_BYTES: usize = 1024;

/// Most alternatives one pattern's `{a,b}` braces may expand to.
const ALTERNATIVES: usize = 32;
/// Deepest brace nesting a pattern may use.
const BRACE_DEPTH: usize = 4;

pub fn validate_glob(pattern: &str) -> Result<(), String> {
    Glob::new(pattern).map(drop)
}

/// A validated, compiled glob, built once per search: `*` and `?` within one
/// path component, whole-component `**`, `[...]` classes (`!` or `^`
/// negates, `a-z` ranges, a leading `]` is literal) and nested `{a,b}`
/// alternatives. Matching is by character, so `?` takes one non-ASCII
/// character whole.
pub(super) struct Glob {
    alternatives: Vec<Vec<Part>>,
}

#[derive(Debug, Clone, PartialEq)]
enum Part {
    Recursive,
    Name(Vec<Token>),
}

#[derive(Debug, Clone, PartialEq)]
enum Token {
    Any,
    Star,
    Char(char),
    Class {
        negated: bool,
        ranges: Vec<(char, char)>,
    },
}

impl Glob {
    pub(super) fn new(pattern: &str) -> Result<Self, String> {
        let refused = || {
            format!(
                "glob {pattern:?} must be a relative pattern of at most {PATTERN_BYTES} bytes \
                 using *, ?, [...] classes, {{a,b}} alternatives (at most {ALTERNATIVES}) or \
                 whole-component **"
            )
        };
        if pattern.is_empty()
            || pattern.len() > PATTERN_BYTES
            || pattern.contains(['\\', ':', '\0'])
        {
            return Err(refused());
        }
        let alternatives = expand(pattern)
            .ok_or_else(refused)?
            .iter()
            .map(|alternative| {
                if alternative.starts_with('/') {
                    return None;
                }
                alternative
                    .split('/')
                    .map(|part| match part {
                        "" | "." | ".." => None,
                        "**" => Some(Ok(Part::Recursive)),
                        _ if part.contains("**") => None,
                        _ => Some(tokens(part).map(Part::Name)),
                    })
                    .collect::<Option<Result<Vec<_>, String>>>()
            })
            .collect::<Option<Result<Vec<_>, String>>>()
            .ok_or_else(refused)?
            .map_err(|reason| format!("{}: {reason}", refused()))?;
        Ok(Self { alternatives })
    }

    pub(super) fn matches(&self, path: &str) -> bool {
        let names: Vec<&str> = path.split('/').collect();
        self.alternatives
            .iter()
            .any(|parts| matches_parts(parts, &names))
    }

    /// A directory must match a nonterminal prefix, or a terminal `**`.
    /// Thus *.rs never walks target/ just to discover nothing matches.
    fn may_descend(&self, directory: &str) -> bool {
        let names: Vec<&str> = directory.split('/').collect();
        self.alternatives.iter().any(|parts| {
            (1..parts.len()).any(|end| matches_parts(&parts[..end], &names))
                || (parts.last() == Some(&Part::Recursive) && matches_parts(parts, &names))
        })
    }
}

/// The tokens of one path component; an unclosed class or a descending
/// range is refused by name rather than read as matching nothing.
fn tokens(part: &str) -> Result<Vec<Token>, String> {
    let unclosed = || format!("class in {part:?} is not closed");
    let mut characters = part.chars().peekable();
    let mut tokens = Vec::new();
    while let Some(character) = characters.next() {
        tokens.push(match character {
            '*' if tokens.last() == Some(&Token::Star) => continue,
            '*' => Token::Star,
            '?' => Token::Any,
            '[' => {
                let negated = characters
                    .next_if(|next| matches!(next, '!' | '^'))
                    .is_some();
                let mut ranges = Vec::new();
                let mut first = true;
                loop {
                    let start = characters.next().ok_or_else(unclosed)?;
                    if start == ']' && !first {
                        break;
                    }
                    first = false;
                    let end = match characters.peek() {
                        Some('-') => {
                            characters.next();
                            match characters.next().ok_or_else(unclosed)? {
                                ']' => {
                                    ranges.push((start, start));
                                    ranges.push(('-', '-'));
                                    break;
                                }
                                end => end,
                            }
                        }
                        _ => start,
                    };
                    if end < start {
                        return Err(format!("class range {start}-{end} is descending"));
                    }
                    ranges.push((start, end));
                }
                Token::Class { negated, ranges }
            }
            literal => Token::Char(literal),
        });
    }
    Ok(tokens)
}

fn matches_name(tokens: &[Token], name: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let mut previous = vec![false; name.len() + 1];
    previous[0] = true;
    for token in tokens {
        let mut next = vec![false; name.len() + 1];
        if *token == Token::Star {
            next[0] = previous[0];
        }
        for (index, character) in name.iter().enumerate() {
            next[index + 1] = match token {
                Token::Star => previous[index + 1] || next[index],
                Token::Any => previous[index],
                Token::Char(expected) => previous[index] && expected == character,
                Token::Class { negated, ranges } => {
                    previous[index]
                        && (ranges
                            .iter()
                            .any(|(low, high)| (low..=high).contains(&character))
                            != *negated)
                }
            };
        }
        previous = next;
    }
    previous[name.len()]
}

fn matches_parts(parts: &[Part], names: &[&str]) -> bool {
    let mut previous = vec![false; names.len() + 1];
    previous[0] = true;
    for part in parts {
        let mut next = vec![false; names.len() + 1];
        if *part == Part::Recursive {
            next[0] = previous[0];
        }
        for (index, name) in names.iter().enumerate() {
            next[index + 1] = match part {
                Part::Recursive => previous[index + 1] || next[index],
                Part::Name(tokens) => previous[index] && matches_name(tokens, name),
            };
        }
        previous = next;
    }
    previous[names.len()]
}

/// Every brace-free alternative of `pattern`, or `None` when its braces or
/// classes are unbalanced, a class names `/`, or the expansion is too large.
fn expand(pattern: &str) -> Option<Vec<String>> {
    let characters: Vec<char> = pattern.chars().collect();
    let mut position = 0;
    let alternatives = sequence(&characters, &mut position, 0)?;
    (position == characters.len()).then_some(alternatives)
}

fn sequence(characters: &[char], position: &mut usize, depth: usize) -> Option<Vec<String>> {
    let mut alternatives = vec![String::new()];
    while let Some(&character) = characters.get(*position) {
        match character {
            ',' | '}' if depth > 0 => break,
            '}' => return None,
            '{' => {
                if depth >= BRACE_DEPTH {
                    return None;
                }
                *position += 1;
                let mut options = Vec::new();
                loop {
                    options.extend(sequence(characters, position, depth + 1)?);
                    match characters.get(*position) {
                        Some(',') => *position += 1,
                        Some('}') => {
                            *position += 1;
                            break;
                        }
                        _ => return None,
                    }
                }
                if alternatives.len().checked_mul(options.len())? > ALTERNATIVES {
                    return None;
                }
                alternatives = alternatives
                    .iter()
                    .flat_map(|prefix| {
                        options
                            .iter()
                            .map(move |option| format!("{prefix}{option}"))
                    })
                    .collect();
                continue;
            }
            '[' => {
                // A class is copied whole; `]` first (after any negation) is literal.
                let start = *position;
                *position += 1;
                if matches!(characters.get(*position), Some('!' | '^')) {
                    *position += 1;
                }
                if characters.get(*position) == Some(&']') {
                    *position += 1;
                }
                while characters.get(*position)? != &']' {
                    if characters[*position] == '/' {
                        return None;
                    }
                    *position += 1;
                }
                let class: String = characters[start..=*position].iter().collect();
                for alternative in &mut alternatives {
                    alternative.push_str(&class);
                }
            }
            ']' => return None,
            _ => {
                for alternative in &mut alternatives {
                    alternative.push(character);
                }
            }
        }
        *position += 1;
    }
    Some(alternatives)
}

/// The project directory name from before the product rename. Repositories
/// may still hold one, and it is excluded whichever name is in use.
const LEGACY_PROJECT_DIRECTORY: &str = ".secode"; // grapt-rename: keep

pub fn excluded(name: &str) -> bool {
    [
        ".git",
        ".grapt",
        LEGACY_PROJECT_DIRECTORY,
        ".stateengine",
        ".codex",
    ]
    .iter()
    .any(|protected| {
        if cfg!(windows) {
            name.eq_ignore_ascii_case(protected)
        } else {
            name == *protected
        }
    })
}

#[derive(Default)]
struct Scan {
    files: Vec<(PathBuf, String)>,
    skipped: Vec<serde_json::Value>,
    skipped_bytes: usize,
    ignored: usize,
}

impl Scan {
    fn skip(&mut self, path: &str, reason: &str) -> Result<(), String> {
        let row = serde_json::json!({"path":path,"reason":reason});
        // The row plus its array separator.
        self.skipped_bytes += serde_json::to_vec(&row)
            .map_err(|error| error.to_string())?
            .len()
            + 1;
        if self.skipped.len() >= RESULT_LIMIT as usize || self.skipped_bytes > OUTPUT_BYTES / 2 {
            return Err(format!(
                "search skipped-report bound exceeded at {path}; narrow scope or exclusions"
            ));
        }
        self.skipped.push(row);
        Ok(())
    }
}

fn display_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(relative) if relative.as_os_str().is_empty() => ".".to_owned(),
        Ok(relative) => relative
            .to_str()
            .map_or_else(|| format!("{relative:?}"), |text| text.replace('\\', "/")),
        Err(_) => "<outside workspace>".to_owned(),
    }
}

fn io_reason(error: &std::io::Error) -> &'static str {
    if error.kind() == std::io::ErrorKind::NotFound {
        "absent"
    } else {
        "unreadable"
    }
}

struct ScopePolicy {
    rules: Vec<Arc<Rules>>,
    exclusion: Option<(PathBuf, &'static str)>,
}

fn scope_policy(
    root: &Path,
    scope: &Path,
    include_ignored: bool,
    count: &mut usize,
    bytes: &mut usize,
) -> Result<ScopePolicy, String> {
    let relative = scope
        .strip_prefix(root)
        .map_err(|_| "scope escaped workspace")?;
    let mut policy = ScopePolicy {
        rules: Vec::new(),
        exclusion: None,
    };
    let mut ancestor = root.to_path_buf();
    if !include_ignored {
        if let Some(rule) = load_ignore(&ancestor, count, bytes)? {
            policy.rules.push(Arc::new(rule));
        }
    }
    for component in relative.components() {
        ancestor.push(component);
        let name = component
            .as_os_str()
            .to_str()
            .ok_or("scope path is not UTF-8")?;
        if excluded(name) {
            policy.exclusion = Some((ancestor, "protected-control-scope"));
            return Ok(policy);
        }
        if !include_ignored {
            if let Some(rule) = load_ignore(&ancestor, count, bytes)? {
                policy.rules.push(Arc::new(rule));
            }
        }
    }
    Ok(policy)
}

#[expect(clippy::too_many_lines, reason = "legacy")]
fn files(
    root: &Path,
    scope: &Path,
    pattern: &str,
    exclusions: &[String],
    tolerant: bool,
    include_ignored: bool,
) -> Result<Scan, String> {
    let glob = Glob::new(pattern)?;
    let exclusions = exclusions
        .iter()
        .map(|pattern| Glob::new(pattern))
        .collect::<Result<Vec<_>, _>>()?;
    let mut visited = 0;
    let mut scan = Scan::default();
    let mut ignore_files = 0_usize;
    let mut ignore_bytes = 0_usize;
    let policy = scope_policy(
        root,
        scope,
        include_ignored,
        &mut ignore_files,
        &mut ignore_bytes,
    )?;
    if let Some((path, reason)) = policy.exclusion {
        scan.ignored = 1;
        scan.skip(&display_path(root, &path), reason)?;
        return Ok(scan);
    }
    let mut pending = vec![(scope.to_path_buf(), 0, policy.rules)];
    while let Some((directory, depth, mut rules)) = pending.pop() {
        let directory_relative = directory
            .strip_prefix(root)
            .map_err(|_| "search directory escaped workspace: <outside workspace>".to_owned())?;
        if let Err(error) = reject_links(root, directory_relative) {
            if tolerant {
                scan.skip(&display_path(root, &directory), &error)?;
                continue;
            }
            return Err(error);
        }
        if !include_ignored && directory != scope {
            if let Some(rule) = load_ignore(&directory, &mut ignore_files, &mut ignore_bytes)? {
                rules.push(Arc::new(rule));
            }
        }
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if tolerant => {
                scan.skip(&display_path(root, &directory), io_reason(&error))?;
                continue;
            }
            Err(error) => {
                return Err(format!(
                    "search directory {} ({}): {error}",
                    io_reason(&error),
                    display_path(root, &directory)
                ))
            }
        };
        for entry in entries {
            visited += 1;
            if visited > ENTRIES {
                return Err(format!("search traversal entry bound exceeded in {}; narrow path or exclude build output", display_path(root, &directory)));
            }
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) if tolerant => {
                    scan.skip(&display_path(root, &directory), io_reason(&error))?;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "search entry unreadable in {}: {error}",
                        display_path(root, &directory)
                    ))
                }
            };
            let path = entry.path();
            let display = display_path(root, &path);
            let name = match entry.file_name().into_string() {
                Ok(name) => name,
                Err(_) if tolerant => {
                    scan.skip(&display, "filename-not-utf8")?;
                    continue;
                }
                Err(_) => return Err(format!("search filename is not UTF-8: {display}")),
            };
            if excluded(&name) {
                continue;
            }
            if name.contains(['\\', ':']) {
                return Err(format!(
                    "search filename cannot be represented as a portable tool path: {display}"
                ));
            }
            let relative = path
                .strip_prefix(scope)
                .map_err(|_| format!("search entry escaped scope: {display}"))?
                .to_str()
                .ok_or_else(|| format!("search path is not UTF-8: {display}"))?
                .replace('\\', "/");
            if relative.len() > PATH_BYTES {
                return Err(format!("search path byte bound exceeded: {display}"));
            }
            if exclusions
                .iter()
                .any(|exclusion| exclusion.matches(&relative))
            {
                scan.skip(&display, "excluded")?;
                continue;
            }
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(error) if tolerant => {
                    scan.skip(&display, io_reason(&error))?;
                    continue;
                }
                Err(error) => {
                    return Err(format!(
                        "search metadata {} ({display}): {error}",
                        io_reason(&error)
                    ))
                }
            };
            if (metadata.is_dir() && ignore_rules::build_directory(&name))
                || (!include_ignored && ignored(&rules, &path, &name, metadata.is_dir()))
            {
                scan.ignored += 1;
                continue;
            }
            if is_link(&metadata) {
                if tolerant {
                    scan.skip(&display, "symlink-or-reparse-point")?;
                    continue;
                }
                return Err(format!(
                    "search refuses symlinks and reparse points: {display}"
                ));
            }
            if metadata.is_dir() {
                if !glob.may_descend(&relative) {
                    continue;
                }
                if depth >= DEPTH {
                    return Err(format!("search traversal depth bound exceeded: {display}"));
                }
                pending.push((path, depth + 1, rules.clone()));
            } else if metadata.is_file() && glob.matches(&relative) {
                let workspace_path = path
                    .strip_prefix(root)
                    .map_err(|_| format!("search file escaped workspace: {display}"))?
                    .to_str()
                    .ok_or_else(|| format!("search path is not UTF-8: {display}"))?
                    .replace('\\', "/");
                if workspace_path.len() > PATH_BYTES {
                    return Err(format!("search path byte bound exceeded: {display}"));
                }
                scan.files.push((path, workspace_path));
            } else if !metadata.is_file() {
                if tolerant {
                    scan.skip(&display, "non-regular")?;
                    continue;
                }
                return Err(format!("search encountered a non-regular file: {display}"));
            }
        }
    }
    scan.files.sort_by(|left, right| left.1.cmp(&right.1));
    Ok(scan)
}

pub fn glob(
    root: &Path,
    scope: &Path,
    pattern: &str,
    limit: u64,
    exclusions: &[String],
    include_ignored: bool,
) -> Result<Vec<u8>, String> {
    let mut scan = files(root, scope, pattern, exclusions, false, include_ignored)?;
    let total = scan.files.len();
    // Return the first page with an explicit count instead of nothing.
    let paths: Vec<_> = scan
        .files
        .into_iter()
        .take(usize::try_from(limit).map_err(|_| "glob limit exceeds usize")?)
        .map(|(_, relative)| relative)
        .collect();
    let truncated = paths.len() < total;
    scan.skipped
        .sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let mut output = serde_json::json!({"paths":paths,"complete":scan.skipped.is_empty() && !truncated,"truncated":truncated,"skipped":scan.skipped,"ignoredCount":scan.ignored});
    if truncated {
        output["totalMatches"] = total.into();
        output["hint"] = "results truncated; narrow the pattern or path, or raise limit".into();
    }
    encode(&output).map_err(|error| format!("{error} in {}", display_path(root, scope)))
}

pub fn grep(
    root: &Path,
    scope: &Path,
    matcher: &regex::Regex,
    pattern: &str,
    limit: u64,
    exclusions: &[String],
    include_ignored: bool,
) -> Result<Vec<u8>, String> {
    let mut scan = if scope.is_file() {
        // An explicitly named file is searched as given; filters select within directories.
        let relative = display_path(root, scope);
        if relative.len() > PATH_BYTES {
            return Err(format!("search path byte bound exceeded: {relative}"));
        }
        Scan {
            files: vec![(scope.to_path_buf(), relative)],
            ..Scan::default()
        }
    } else {
        files(root, scope, pattern, exclusions, true, include_ignored)?
    };
    let mut matches = Vec::new();
    let mut scanned = 0_u64;
    let mut rendered_bytes = 0_usize;
    let mut truncated = false;
    let mut stopped_reason = None;
    'files: for (path, relative) in std::mem::take(&mut scan.files) {
        // Revalidate before open; never follow an entry that changed into a link.
        if let Err(error) = reject_links(
            root,
            path.strip_prefix(root)
                .map_err(|_| format!("grep path escaped workspace: {relative}"))?,
        ) {
            scan.skip(&relative, &error)?;
            continue;
        }
        let metadata = match std::fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) => {
                scan.skip(&relative, io_reason(&error))?;
                continue;
            }
        };
        if !metadata.is_file() || is_link(&metadata) {
            scan.skip(&relative, "non-regular")?;
            continue;
        }
        let text = match search_text(&path, metadata.len(), &mut scanned) {
            Ok(text) => text,
            Err(reason) => {
                // Preserve already collected matches when omission details or
                // the aggregate read budget fill up. Neither is a full search.
                if scan.skip(&relative, reason).is_err() {
                    stopped_reason = Some("skipped-report-bound");
                    truncated = true;
                    break;
                }
                if reason == "scan-byte-bound" {
                    stopped_reason = Some(reason);
                    truncated = true;
                    break;
                }
                continue;
            }
        };
        for (index, line) in text.lines().enumerate() {
            if matcher.is_match(line) {
                if matches.len() as u64 >= limit {
                    truncated = true;
                    break 'files;
                }
                let mut row = serde_json::json!({"path":relative,"line":index + 1,"text":line});
                if line.len() > MATCH_TEXT_BYTES {
                    let mut cut = MATCH_TEXT_BYTES;
                    while !line.is_char_boundary(cut) {
                        cut -= 1;
                    }
                    row["text"] = line[..cut].into();
                    row["textTruncated"] = true.into();
                }
                rendered_bytes += serde_json::to_vec(&row)
                    .map_err(|error| error.to_string())?
                    .len()
                    + 1;
                // Skipped rows hold at most half the output; matches get the
                // rest less the envelope's keys, counts and hint.
                if rendered_bytes > OUTPUT_BYTES / 2 - ENVELOPE_BYTES {
                    truncated = true;
                    break 'files;
                }
                matches.push(row);
            }
        }
    }
    scan.skipped
        .sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    let mut output = serde_json::json!({"matches":matches,"complete":scan.skipped.is_empty() && !truncated,"truncated":truncated,"skipped":scan.skipped,"ignoredCount":scan.ignored,"scannedBytes":scanned});
    if let Some(reason) = stopped_reason {
        output["stoppedReason"] = reason.into();
    }
    if truncated || !scan.skipped.is_empty() {
        output["hint"] =
            "partial search; inspect skipped files, narrow path/glob or exclusions, or raise the match limit".into();
    }
    encode(&output).map_err(|error| format!("{error} in {}", display_path(root, scope)))
}

/// Probe binary content before spending the aggregate budget on a whole file.
/// The probe is part of that budget, and no partial file is reported as text.
fn search_text(path: &Path, length: u64, scanned: &mut u64) -> Result<String, &'static str> {
    if length > FILE_EFFECT_MAX_BYTES {
        return Err("file-byte-bound");
    }
    let mut file = std::fs::File::open(path).map_err(|error| io_reason(&error))?;
    let probe = 4096.min(SCAN_BYTES - *scanned);
    if probe == 0 {
        return Err("scan-byte-bound");
    }
    let mut bytes = Vec::new();
    let read = Read::by_ref(&mut file).take(probe).read_to_end(&mut bytes);
    *scanned += bytes.len() as u64;
    read.map_err(|error| io_reason(&error))?;
    if bytes.contains(&0)
        || std::str::from_utf8(&bytes).is_err_and(|error| error.error_len().is_some())
    {
        return Err("binary-or-non-utf8");
    }
    let before = bytes.len();
    let remaining = (FILE_EFFECT_MAX_BYTES + 1 - before as u64).min(SCAN_BYTES - *scanned);
    let read = file.take(remaining).read_to_end(&mut bytes);
    let added = (bytes.len() - before) as u64;
    *scanned += added;
    read.map_err(|error| io_reason(&error))?;
    if bytes.len() as u64 > FILE_EFFECT_MAX_BYTES {
        return Err("file-byte-bound");
    }
    if *scanned == SCAN_BYTES && added == remaining {
        return Err("scan-byte-bound");
    }
    let text = String::from_utf8(bytes).map_err(|_| "binary-or-non-utf8")?;
    if text.contains('\0') {
        return Err("binary-or-non-utf8");
    }
    Ok(text)
}

fn encode(value: &serde_json::Value) -> Result<Vec<u8>, String> {
    let bytes = serde_json::to_vec(value).map_err(|error| error.to_string())?;
    if bytes.len() > OUTPUT_BYTES {
        return Err("coding output byte bound exceeded".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glob(pattern: &str) -> Glob {
        Glob::new(pattern).unwrap_or_else(|error| panic!("{pattern}: {error}"))
    }

    #[test]
    fn glob_component_and_recursive_semantics() {
        for (pattern, path, expected) in [
            ("**/*.rs", "lib.rs", true),
            ("**/*.rs", "src/lib.rs", true),
            ("*.rs", "src/lib.rs", false),
            ("src/?.rs", "src/é.rs", true),
            ("src/**/x.rs", "src/a/b/x.rs", true),
            ("src/**/x.rs", "src/x.rs", true),
            ("src/**/x.rs", "other/x.rs", false),
            // Nested and path-spanning brace alternatives.
            ("**/*.{ts,tsx}", "src/app.tsx", true),
            ("**/*.{ts,tsx}", "src/app.ts", true),
            ("**/*.{ts,tsx}", "src/app.js", false),
            ("{src,lib}/**/*.rs", "lib/a/b.rs", true),
            ("{src,lib}/**/*.rs", "tests/b.rs", false),
            ("*.{r{s,on},toml}", "a.ron", true),
            ("*.{r{s,on},toml}", "a.rs", true),
            ("*.{r{s,on},toml}", "a.toml", true),
            ("*.{r{s,on},toml}", "a.r", false),
            ("{a/b,c}.rs", "a/b.rs", true),
            ("x{,.bak}", "x", true),
            // Classes, negated classes and ranges.
            ("src/[A-Z]*.rs", "src/Main.rs", true),
            ("src/[A-Z]*.rs", "src/main.rs", false),
            ("[!.]*", "visible", true),
            ("[!.]*", ".hidden", false),
            ("[^.]*", ".hidden", false),
            ("[]x].md", "].md", true),
            ("[{]x", "{x", true),
            // A class never spans a separator.
            ("a?b", "a/b", false),
        ] {
            assert_eq!(
                glob(pattern).matches(path),
                expected,
                "{pattern} against {path}"
            );
        }
    }

    #[test]
    fn malformed_or_unbounded_globs_are_refused_by_name() {
        let many = "{a,b}{c,d}{e,f}{g,h}{i,j}{k,l}";
        for pattern in [
            "",
            "/abs",
            "a\\b",
            "c:/x",
            "a//b",
            "./a",
            "../a",
            "a**",
            "{a,b",
            "a,b}",
            "[ab",
            "a]",
            "[a/b]",
            "{{{{{a}}}}}",
            many,
            "[z-a]",
            "[!z-a]",
            "x[^9-0]",
        ] {
            assert!(Glob::new(pattern).is_err(), "{pattern:?} must be refused");
        }
        for pattern in ["[z-a]", "[!z-a]"] {
            let refused = Glob::new(pattern).err().unwrap_or_default();
            assert!(refused.contains("range z-a is descending"), "{refused}");
        }
        for (pattern, path) in [
            ("[a-z]", "m"),
            ("[!a-z]", "M"),
            ("[α-ω]", "β"),
            ("[a-]", "-"),
        ] {
            assert!(glob(pattern).matches(path), "{pattern} against {path}");
        }
        assert!(Glob::new("a,b").is_ok(), "a top-level comma is a literal");
        assert!(
            Glob::new("{{{{a}}}}").is_ok(),
            "four nested levels are allowed"
        );
    }

    #[test]
    fn descent_follows_every_alternative_and_skips_the_rest() {
        let braced = glob("{src,lib}/**/*.rs");
        assert!(braced.may_descend("src") && braced.may_descend("lib"));
        assert!(!braced.may_descend("target"));
        assert!(!glob("*.rs").may_descend("target"));
        assert!(glob("**").may_descend("target"));
    }
}
