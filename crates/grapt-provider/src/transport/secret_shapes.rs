//! Shapes in provider text that must not reach a reader whatever the current
//! credential: labelled values, authorization schemes, URL credentials,
//! published key prefixes, long encoded runs, and local home directories.
//! Each finder adds the byte ranges to mask.

/// A field whose name ends with one of these, case-insensitively, has its
/// `:` or `=` value masked. Names ending in `_key`, `-key` or `Key`, and a
/// bare `key`, are masked too.
const LABELS: [&str; 9] = [
    "authorization",
    "x-api-key",
    "api_key",
    "api-key",
    "apikey",
    "token",
    "secret",
    "password",
    "cookie",
];
/// Published key prefixes, matched at a word start.
const KEY_PREFIXES: [&str; 12] = [
    "sk-",
    "sk_",
    "gsk_",
    "xai-",
    "aiza",
    "akia",
    "eyj",
    "ghp_",
    "gho_",
    "ghs_",
    "github_pat_",
    "xoxb-",
];
/// Token characters a key prefix needs after it, so that a short word such as
/// `SK-1234` or a name such as `Aizawa` is kept.
const MIN_KEY_BODY: usize = 8;
/// A run of base64 or hex characters this long, mixing letters and digits.
const MIN_ENCODED_RUN: usize = 32;

pub(super) fn mark(text: &str, ranges: &mut Vec<(usize, usize)>) {
    labelled(text, ranges);
    schemes(text, ranges);
    url_credentials(text, ranges);
    url_query_and_fragment(text, ranges);
    key_prefixes(text, ranges);
    encoded_runs(text, ranges);
    home_paths(text, ranges);
}

/// Everything a URL's query or fragment can carry, as one masked range.
///
/// A query value has no closed alphabet: `,` `)` `'` `;` `%2C` and every other
/// sub-delimiter is legal inside one, so listing the characters that end a value
/// is an open-ended guess, and each guess has so far leaked the next character.
/// This does not guess. For a URL-looking token, everything from its first `?`
/// or `#` to the next whitespace (or the end of the text) is masked, so the whole
/// query string and fragment go together whatever delimiter a value contains.
///
/// Masking to the next whitespace hides text a reader might have wanted, and
/// that is the intended trade: an over-long mask never leaks a credential. The
/// `?` or `#` itself is kept, so the reader still sees that a query was redacted
/// rather than removed.
fn url_query_and_fragment(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let mut from = 0;
    for token in text.split_whitespace() {
        let Some(offset) = text[from..].find(token) else {
            break;
        };
        let start = from + offset;
        let end = start + token.len();
        from = end;
        let Some(marker) = token.find(['?', '#']) else {
            continue;
        };
        // Prose can carry a question mark; only a URL-looking token counts.
        if !url_like(&token[..marker]) {
            continue;
        }
        let masked_from = start + marker + 1;
        if end > masked_from {
            ranges.push((masked_from, end));
        }
    }
}

/// Whether the text before a `?` or `#` reads as a URL, so its tail is masked.
///
/// A scheme settles it. Otherwise the prefix must begin with a path separator or
/// carry both one and a host dot, which tells `example.test/path` and
/// `/api/v1/items` from prose such as `yes/no` or `what?`.
fn url_like(prefix: &str) -> bool {
    prefix.contains("://")
        || prefix.starts_with('/')
        || (prefix.contains('/') && prefix.contains('.'))
}

/// The account segment of a home directory: `/home/example/…`,
/// `/Users/example/…` and `C:\Users\example\…` (either separator). Masking the
/// segment rather than a whole rooted prefix also catches a provider that
/// echoes the directory without the path after it. It is shape-based on
/// purpose: comparing against the running process's own answer would miss a
/// body written for another spelling of the same profile, and would tie this
/// masking to ambient environment state.
const HOME_ROOTS: [&str; 3] = ["/home/", "/users/", "\\users\\"];

fn home_paths(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let lower = text.to_ascii_lowercase();
    for root in HOME_ROOTS {
        for (start, _) in lower.match_indices(root) {
            let at = start + root.len();
            // A drive-letter root (`c:\users\…`) only counts at a path start;
            // `/users/` may appear after any path segment.
            if root == "\\users\\" && start != 0 && !is_path_start(&text[..start]) {
                continue;
            }
            let end = skip(text, at, |c| {
                !matches!(c, '/' | '\\' | '"' | ' ' | '\t' | ',' | ';' | '}' | ']')
            });
            // A bare root with no account after it names nobody.
            if end > at {
                ranges.push((at, end));
            }
        }
    }
}

/// Whether the text before a root ends where a new path may begin, so a
/// backslash root is not matched inside an unrelated word.
fn is_path_start(before: &str) -> bool {
    before
        .chars()
        .next_back()
        .is_none_or(|c| matches!(c, ' ' | '\t' | '"' | '\'' | '(' | '[' | '{' | '=' | ':'))
}

pub(super) fn sensitive(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    LABELS.iter().any(|label| lower.ends_with(label))
        || lower == "key"
        || lower.ends_with("_key")
        || lower.ends_with("-key")
        || (name.len() > 3 && name.ends_with("Key"))
}

/// `name: value` and `name=value`, quoted or not. An object or array value is
/// masked whole.
fn labelled(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let bytes = text.as_bytes();
    for (separator, _) in text.match_indices([':', '=']) {
        let mut end = separator;
        while end > 0 && matches!(bytes[end - 1], b' ' | b'\t' | b'"' | b'\\') {
            end -= 1;
        }
        let mut start = end;
        while start > 0
            && (bytes[start - 1].is_ascii_alphanumeric() || matches!(bytes[start - 1], b'_' | b'-'))
        {
            start -= 1;
        }
        if start == end || !sensitive(&text[start..end]) {
            continue;
        }
        let at = skip(text, separator + 1, |c| {
            c.is_ascii_whitespace() || c == '"' || c == '\\'
        });
        let end = match bytes.get(at) {
            Some(b'{' | b'[') => balanced_end(text, at),
            _ => skip(text, at, |c| {
                !matches!(c, '"' | '\\' | '\n' | '\r' | ',' | '}' | ']')
            }),
        };
        if end > at {
            ranges.push((at, end));
        }
    }
}

/// The end of the object or array opening at `at`, or the text's end.
fn balanced_end(text: &str, at: usize) -> usize {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for (offset, c) in text[at..].char_indices() {
        if quoted {
            match (escaped, c) {
                (true, _) => escaped = false,
                (false, '\\') => escaped = true,
                (false, '"') => quoted = false,
                _ => (),
            }
            continue;
        }
        match c {
            '"' => quoted = true,
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return at + offset + 1;
                }
            }
            _ => (),
        }
    }
    text.len()
}

/// `Bearer <token>` and `Basic <base64>` in any case; a Basic token is masked
/// when it looks encoded rather than like a word. The shortest credential,
/// `a:b`, encodes to four characters.
fn schemes(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let lower = text.to_ascii_lowercase();
    for (start, _) in lower.match_indices("bearer ") {
        let at = skip(text, start + 7, char::is_whitespace);
        let end = skip(text, at, |c| {
            !c.is_whitespace() && !matches!(c, '"' | '\\' | ',' | '}' | ']')
        });
        if end > at {
            ranges.push((at, end));
        }
    }
    for (start, _) in lower.match_indices("basic ") {
        let at = skip(text, start + 6, char::is_whitespace);
        let end = skip(text, at, |c| {
            c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=')
        });
        let token = &text[at..end];
        let encoded = token
            .bytes()
            .any(|b| b.is_ascii_digit() || matches!(b, b'+' | b'/' | b'='))
            || token.bytes().skip(1).any(|b| b.is_ascii_uppercase())
            || decodes_to_credential(token);
        if token.len() >= 4 && encoded {
            ranges.push((at, end));
        }
    }
}

/// Whether `token` is standard base64 whose decoded bytes hold the `:` that
/// separates a Basic user from its password. This catches a credential whose
/// encoding happens to read like a lowercase word.
fn decodes_to_credential(token: &str) -> bool {
    let bytes = token.as_bytes();
    if bytes.is_empty() || !bytes.len().is_multiple_of(4) {
        return false;
    }
    let padding = bytes.iter().rev().take_while(|&&b| b == b'=').count();
    if padding > 2 || bytes[..bytes.len() - padding].contains(&b'=') {
        return false;
    }
    let mut decoded = Vec::with_capacity(bytes.len() / 4 * 3);
    for chunk in bytes.chunks(4) {
        let mut word = 0u32;
        for &b in chunk {
            let value = match b {
                b'A'..=b'Z' => b - b'A',
                b'a'..=b'z' => b - b'a' + 26,
                b'0'..=b'9' => b - b'0' + 52,
                b'+' => 62,
                b'/' => 63,
                b'=' => 0,
                _ => return false,
            };
            word = (word << 6) | u32::from(value);
        }
        decoded.extend_from_slice(&word.to_be_bytes()[1..]);
    }
    decoded.truncate(decoded.len() - padding);
    decoded.contains(&b':')
}

/// The user information of `scheme://user:pass@host`.
/// The user information of `scheme://user:pass@host`.
///
/// The user-info runs to the `@` that ends it, so no delimiter list is needed:
/// an `@` may itself appear encoded inside it, and the last one in the authority
/// is the one that ends the credentials.
fn url_credentials(text: &str, ranges: &mut Vec<(usize, usize)>) {
    for (start, _) in text.match_indices("://") {
        let at = start + 3;
        let end = skip(text, at, |c| {
            !c.is_whitespace() && !matches!(c, '/' | '"' | '\\' | '?' | '#')
        });
        if let Some(user) = text[at..end].rfind('@').filter(|user| *user > 0) {
            ranges.push((at, at + user));
        }
    }
}

fn key_prefixes(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let lower = text.to_ascii_lowercase();
    for label in KEY_PREFIXES {
        for (start, _) in lower.match_indices(label) {
            if text[..start]
                .chars()
                .next_back()
                .is_some_and(|c| c.is_ascii_alphanumeric())
            {
                continue;
            }
            // A JSON Web Token joins its three parts with dots.
            let dots = label == "eyj";
            let end = skip(text, start, |c| {
                c.is_ascii_alphanumeric() || matches!(c, '-' | '_') || (dots && c == '.')
            });
            if end.saturating_sub(start + label.len()) >= MIN_KEY_BODY {
                ranges.push((start, end));
            }
        }
    }
}

fn encoded_runs(text: &str, ranges: &mut Vec<(usize, usize)>) {
    let encoded =
        |b: u8| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'/' | b'=' | b'_' | b'-');
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < bytes.len() {
        if !encoded(bytes[at]) {
            at += 1;
            continue;
        }
        let start = at;
        while at < bytes.len() && encoded(bytes[at]) {
            at += 1;
        }
        let run = &bytes[start..at];
        if run.len() >= MIN_ENCODED_RUN
            && run.iter().any(u8::is_ascii_digit)
            && run.iter().any(u8::is_ascii_alphabetic)
        {
            ranges.push((start, at));
        }
    }
}

fn skip(text: &str, at: usize, accepts: impl Fn(char) -> bool) -> usize {
    at + text[at..]
        .chars()
        .take_while(|c| accepts(*c))
        .map(char::len_utf8)
        .sum::<usize>()
}

#[cfg(test)]
#[path = "secret_shapes_tests.rs"]
mod tests;
