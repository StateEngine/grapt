//! The structural gate over review blocker 4, ingress half.
//!
//! # The defect this exists to make impossible
//!
//! `Bounds::max_response_bytes` was enforced in `Client::raw`. `Client::json`
//! was a second path that called `Connection::call` itself and handed the body
//! straight to the decoder — and permission list and permission reply used it.
//! So the bound was real, was tested, and simply was not on the route half the
//! operations took.
//!
//! Nothing caught it because every test that exercised the bound went through
//! the path that had it. That is the shape worth naming: a bound with two
//! ingress paths is a bound on ONE of them, and the tests will agree with you
//! that it works.
//!
//! The cure is structural rather than another check. There is now exactly one
//! function that touches `Connection::call`, and this gate counts it. A second
//! call site is a red test naming the reason, which is the only thing that
//! would have caught the first one.

use std::path::PathBuf;

const MAX_SOURCE_BYTES: u64 = 1_048_576;

/// The Windows path separator, named so this file carries no escape that a
/// rewriting tool can mangle.
const BACKSLASH: char = '\u{5c}';

fn read_normalized(relative: &str) -> String {
    // Normalized to LF: this repository carries no `.gitattributes`, so a
    // CRLF checkout would otherwise make this gate's result depend on the
    // machine rather than on the source.
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative);
    let metadata = std::fs::metadata(&path)
        .unwrap_or_else(|error| panic!("source must be stattable at {path:?}: {error}"));
    assert!(
        metadata.len() <= MAX_SOURCE_BYTES,
        "{path:?} exceeds the {MAX_SOURCE_BYTES}-byte read bound"
    );
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|error| panic!("source must be readable at {path:?}: {error}"));
    String::from_utf8(bytes)
        .unwrap_or_else(|error| panic!("source must be UTF-8 at {path:?}: {error}"))
        .replace("\r\n", "\n")
}

/// The source of `client.rs` with its `#[cfg(test)]` module removed.
///
/// Witnesses legitimately drive a transport directly, so counting their call
/// sites would make this gate grow an exception list until it meant nothing.
/// `client.rs` declares its tests as `mod tests;` in a separate file, so there
/// is nothing to strip today — this function exists so that stays true by
/// assertion rather than by luck.
/// The whole client MODULE, carrier and subject submodules together.
///
/// It used to read `client.rs` alone. That was the same thing until the file
/// passed the workspace module ceiling and was split by subject: the ingress
/// rule is about the client, not about one of its files, and reading only the
/// carrier would let a second transport call appear in a sibling and pass.
fn client_source() -> String {
    let mut text = read_normalized("src/client.rs");
    let module = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("client");
    if module.is_dir() {
        // RECURSIVE. A review finding: `read_dir` sees one level, so a second
        // transport call in `client/<sub>/deeper.rs` would pass a gate whose
        // entire subject is that there is exactly one ingress — and the module
        // ceiling makes deeper splits likely, not hypothetical.
        let mut parts: Vec<std::path::PathBuf> = Vec::new();
        let mut stack = vec![module.clone()];
        while let Some(directory) = stack.pop() {
            for entry in std::fs::read_dir(&directory)
                .unwrap_or_else(|error| panic!("client module must be readable: {error}"))
            {
                let path = entry
                    .unwrap_or_else(|error| panic!("client entry must be readable: {error}"))
                    .path();
                if path.is_dir() {
                    stack.push(path);
                } else if path.extension().is_some_and(|extension| extension == "rs") {
                    parts.push(path);
                }
            }
        }
        parts.sort();
        for part in parts {
            // Witnesses are not ingress. `tests.rs` and the `tests/` directory
            // beside it hold this module's own witnesses, whose transport calls
            // would otherwise be counted as a second ingress.
            // A `tests` DIRECTORY is skipped for the same reason, now that the
            // walk goes deeper than one level.
            if part.file_name().is_some_and(|name| name == "tests.rs")
                || part
                    .components()
                    .any(|component| component.as_os_str() == "tests")
            {
                continue;
            }
            text.push('\n');
            let relative: String = part
                .strip_prefix(PathBuf::from(env!("CARGO_MANIFEST_DIR")))
                .unwrap_or(&part)
                .to_string_lossy()
                .chars()
                .map(|character| {
                    if character == BACKSLASH {
                        '/'
                    } else {
                        character
                    }
                })
                .collect();
            text.push_str(&read_normalized(&relative));
        }
    }
    assert!(
        !text.contains("#[cfg(test)]\nmod tests {"),
        "client.rs has grown an inline test module; this gate must strip it before \
         counting call sites, or a witness's transport call will be counted as an ingress"
    );
    text
}

#[test]
fn exactly_one_function_calls_the_connection() {
    let source = client_source();
    let sites = source.matches(".connection.call(").count();
    assert_eq!(
        sites, 1,
        "found {sites} call sites for `Connection::call` in client.rs. There must be \
         exactly ONE, because the response byte bound is enforced there. A second call \
         site is a second ingress, and a bound with two ingress paths is a bound on one \
         of them — which is exactly how the permission routes came to be unbounded while \
         the bound's own tests stayed green."
    );
}

#[test]
fn synchronous_and_incremental_reads_share_the_response_bound() {
    // Both acquisition modes must enter the same response validator before
    // DTO decoding. New readers may share the validator without changing a
    // global call-site count; the sole Connection::call is checked separately.
    for path in [
        "src/client.rs",
        "src/client/recovery.rs",
        "src/client/presets.rs",
        "src/client/catalog_refresh.rs",
        "src/client/gateway.rs",
        "src/client/worktree.rs",
        "src/client/worktree_attachment.rs",
    ] {
        assert!(
            read_normalized(path).contains("self.accept_response("),
            "{path} must enter the common response validator"
        );
    }
    let source = read_normalized("src/client/response.rs");
    let validator_at = source
        .find("fn accept_response(")
        .unwrap_or_else(|| panic!("client/response.rs must define `fn accept_response`"));
    let after_validator = &source[validator_at..];
    let next_fn = after_validator[1..]
        .find("\n    fn ")
        .or_else(|| after_validator[1..].find("\n    pub fn "))
        .map_or(after_validator.len(), |at| at + 1);
    let body = &after_validator[..next_fn];
    assert!(
        body.contains("self.bounds.max_response_bytes"),
        "the shared response validator must enforce `max_response_bytes`"
    );
    assert!(
        body.contains("ResponseTooLarge"),
        "the shared response validator must refuse oversized responses"
    );
}

#[test]
fn there_is_no_parallel_json_ingress() {
    // Permission payloads now use the real DTO codecs directly after `ingest`.
    // Reintroducing the old wrapper would recreate the second-ingress shape.
    let source = client_source();
    assert!(
        !source.contains("fn json("),
        "client.rs must not restore the old parallel JSON ingress"
    );
    assert!(
        source.contains("decode_permissions(&bytes)"),
        "permission list must decode the bounded bytes returned by `ingest`"
    );
}

#[test]
fn this_gate_can_actually_find_a_call_site() {
    // ANTI-VACUITY for a counting assertion. `assert_eq!(sites, 1)` is equally
    // happy when the scanner reads an empty string and finds zero — no, it would
    // fail at zero, but `json_holds_no_bound_of_its_own` and the pattern
    // assertions above are absences that WOULD pass vacuously. So prove the
    // instrument reads real source.
    let source = client_source();
    assert!(
        source.len() > 4_000,
        "client.rs read back as {} bytes; the scanner is not reading the real file",
        source.len()
    );
    assert!(
        source.contains("impl<C: Connection> Client<C>"),
        "the scanner is not reading client.rs's real contents"
    );
}
