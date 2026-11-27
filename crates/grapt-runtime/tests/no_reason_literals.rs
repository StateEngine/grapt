//! Structural gate: no terminal-reason LITERAL survives in production code.
//!
//! The mutation rows attribute each individual routing law. This gate covers
//! what they cannot: a NEW string reason introduced later, at a site no
//! existing witness watches. That is exactly how the defect R2A closes arrived
//! — `const ABORTED: &str = "aborted"` was a reasonable-looking line at the
//! time, and nothing objected to it for the whole life of the carrier.
//!
//! Scope is the two crates that own the terminal vocabulary and its carriers.
//! Comments are stripped first: the oracle's own schema literals are quoted in
//! documentation on purpose, and quoting a vocabulary is not deciding on one.

use std::path::{Path, PathBuf};

/// The reason literals the string carrier used. Assembled from fragments so
/// this file does not trip a scanner pointed at it by mistake, and so a reader
/// cannot "fix" a failure by editing the needle.
fn banned() -> Vec<String> {
    [
        &["abort", "ed"][..],
        &["provider-", "failure"][..],
        &["process-", "loss"][..],
        &["attempts-", "exhausted"][..],
        &["retry-attempts-", "exhausted"][..],
        &["invalid-provider-", "output"][..],
        &["compaction-", "failed"][..],
        &["history-reload-", "failed"][..],
        &["history-generation-did-not-", "advance"][..],
        &["no-progress-", "timeout"][..],
        &["context-overflow-after-", "output"][..],
        &["context-overflow-rebuild-", "exhausted"][..],
        &["retryable-failure-after-", "output"][..],
        &["provider-stream-ended-without-", "terminal"][..],
    ]
    .iter()
    .map(|parts| format!("\"{}\"", parts.concat()))
    .collect()
}

/// Remove line comments and block comments, so a documented vocabulary is not
/// mistaken for a decision made on one.
fn strip_comments(source: &str) -> String {
    let mut out = String::with_capacity(source.len());
    let mut chars = source.chars().peekable();
    let mut in_line = false;
    let mut block_depth = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while let Some(c) = chars.next() {
        if in_line {
            if c == '\n' {
                in_line = false;
                out.push(c);
            }
            continue;
        }
        if block_depth > 0 {
            if c == '*' && chars.peek() == Some(&'/') {
                chars.next();
                block_depth -= 1;
            } else if c == '/' && chars.peek() == Some(&'*') {
                chars.next();
                block_depth += 1;
            } else if c == '\n' {
                out.push(c);
            }
            continue;
        }
        if in_string {
            out.push(c);
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
            continue;
        }
        if c == '"' {
            in_string = true;
            out.push(c);
            continue;
        }
        if c == '/' {
            match chars.peek() {
                Some('/') => {
                    chars.next();
                    in_line = true;
                    continue;
                }
                Some('*') => {
                    chars.next();
                    block_depth = 1;
                    continue;
                }
                _ => {}
            }
        }
        out.push(c);
    }
    out
}

fn rust_sources(dir: &Path, found: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => panic!("cannot read {}: {error}", dir.display()),
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_sources(&path, found);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            found.push(path);
        }
    }
}

fn scanned_roots() -> Vec<PathBuf> {
    let here = Path::new(env!("CARGO_MANIFEST_DIR"));
    let crates = match here.parent() {
        Some(crates) => crates,
        None => panic!("the crate directory must have a parent"),
    };
    vec![here.join("src"), crates.join("grapt-provider").join("src")]
}

#[test]
fn no_terminal_reason_literal_survives_in_production_code() {
    let needles = banned();
    let mut sources = Vec::new();
    for root in scanned_roots() {
        rust_sources(&root, &mut sources);
    }
    assert!(
        sources.len() >= 10,
        "the scan found only {} files, which means it is pointed at the wrong tree and would \
         pass vacuously",
        sources.len()
    );

    let mut hits = Vec::new();
    for path in &sources {
        let source = match std::fs::read_to_string(path) {
            Ok(source) => source,
            Err(error) => panic!("cannot read {}: {error}", path.display()),
        };
        let code = strip_comments(&source);
        for needle in &needles {
            if code.contains(needle.as_str()) {
                hits.push(format!("{}: {needle}", path.display()));
            }
        }
    }

    assert!(
        hits.is_empty(),
        "a terminal reason is being named by a string literal again:\n  {}",
        hits.join("\n  ")
    );
}

/// THE CONTROL. Proving absence needs a same-file positive, or a scanner that
/// reads nothing at all reports the same clean result. This shows the scanner
/// does find a needle when one is present, and that comment stripping is what
/// spares the documented occurrences rather than the scan simply missing them.
#[test]
fn the_scanner_finds_a_literal_when_one_is_there() {
    let needle = format!("\"{}\"", ["abort", "ed"].concat());

    let code = format!("fn main() {{ let reason = {needle}; }}");
    assert!(
        strip_comments(&code).contains(&needle),
        "the scanner must find a live literal"
    );

    let documented = format!("/// the oracle spelled this {needle}\nfn main() {{}}");
    assert!(
        !strip_comments(&documented).contains(&needle),
        "a documented vocabulary is not a decision made on one"
    );

    let blocked = format!("/* {needle} */ fn main() {{}}");
    assert!(
        !strip_comments(&blocked).contains(&needle),
        "block comments are stripped too"
    );

    // A literal inside a string must SURVIVE stripping: the stripper must not
    // treat a `//` inside a string as the start of a comment and swallow the
    // rest of the line.
    let in_string = format!("fn main() {{ let url = \"http://x\"; let r = {needle}; }}");
    assert!(
        strip_comments(&in_string).contains(&needle),
        "a `//` inside a string literal is not a comment"
    );
}
