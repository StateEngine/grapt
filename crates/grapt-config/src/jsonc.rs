//! A strict, bounded JSONC decoder with exact byte spans and structured paths.
//!
//! # What it accepts
//!
//! JSON plus line comments, block comments and one trailing comma, because that
//! is `jsonc-parser` under `{ allowTrailingComma: true }`
//! (the oracle's `config.ts:229`). A PRESERVE decision measured from the
//! oracle, not a taste.
//!
//! # What it refuses that the oracle accepts: FOUR deliberate departures
//!
//! 1. **A duplicate object key.** `jsonc-parser` keeps the last silently, so
//!    `{"model":"a","model":"b"}` resolves to `b` and the author is never told
//!    that the first line does nothing. A document that says two different
//!    things about one key has no single meaning, and guessing one is the defect
//!    rather than the fix.
//! 2. **The empty key.** JSON permits `""`. A configuration key that cannot be
//!    named cannot be resolved by name.
//! 3. **Trailing content after the root value.** A second document appended to
//!    the first is not a longer document.
//! 4. **An isolated UTF-16 surrogate code unit** — unpaired `"\uD800"`,
//!    `"\uD800\u0041"`, or a bare `"\uDC00"`. Previously filed under ordinary
//!    strictness, which was WRONG: `jsonc-parser` 3.3.1 accepts every valid
//!    `\uXXXX` and appends `String.fromCharCode` with no pairing check. An
//!    OPERATIONAL NON-PARITY: Rust `str` is UTF-8 and cannot represent an
//!    isolated surrogate, so the choice was lossy U+FFFD substitution or a typed
//!    refusal. Substitution would let a truncated credential-bearing value
//!    decode into something that looks fine.
//!
//! Each refusal becomes a typed [`JsoncError`]. None silently drops a file —
//! which is what the oracle does for every parse failure
//! (`if (errors.length) return`, `config.ts:231`).
//!
//! # Public failures carry no authored bytes
//!
//! A `Debug` or `Display` over a decode failure is a read channel. Every
//! authored fragment a refusal carries — literal, key, escape, unexpected byte —
//! is held as [`Redacted`], and a key path segment renders as its ORDINAL. The
//! typed kind, the structured path and the byte offset survive; the bytes do
//! not.
//!
//! # No panics, and what "bounded" does and does not mean here
//!
//! No panicking path. The depth bound is checked BEFORE descending, so a hostile
//! `[[[[…` refuses instead of exhausting the stack, and the node bound is
//! charged BEFORE each scalar or container is constructed, so a document cannot
//! build past the limit and then be rejected.
//!
//! The byte cap is **defense in depth, not an I/O capture bound.** [`decode`]
//! receives an ALREADY MATERIALIZED slice and `from_utf8` is zero-copy, so
//! nothing here can enforce master §3.3's "before unbounded allocation" — that
//! allocation happened in whoever read the file. That bound belongs to U1b's
//! bounded-read contract. Re-checking the length here still refuses an oversized
//! document before it is parsed, and that is all this crate can claim.

use crate::bounds::ConfigBounds;
use crate::document::{
    ByteSpan, ConfigKey, ConfigNumber, ConfigValue, Redacted, Spanned, ValuePath,
};
use std::collections::BTreeSet;

/// What went wrong, where in the document, and at which byte.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{kind} at {path} (byte {offset})")]
pub struct JsoncError {
    kind: JsoncErrorKind,
    path: ValuePath,
    offset: usize,
}

impl JsoncError {
    pub fn kind(&self) -> &JsoncErrorKind {
        &self.kind
    }

    pub fn path(&self) -> &ValuePath {
        &self.path
    }

    pub fn offset(&self) -> usize {
        self.offset
    }
}

/// Closed. A reader matches every arm; there is no catch-all string variant to
/// absorb a new failure mode without anyone noticing.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum JsoncErrorKind {
    #[error("document bytes {actual} exceed the bound {bound}")]
    DocumentTooLarge { actual: usize, bound: usize },
    #[error("document is not valid UTF-8")]
    NotUtf8,
    #[error("document is empty")]
    Empty,
    #[error("unexpected end of document")]
    UnexpectedEnd,
    #[error("expected {expected}, found {found}")]
    Expected {
        expected: &'static str,
        found: Redacted,
    },
    #[error("unterminated block comment")]
    UnterminatedComment,
    #[error("unterminated string")]
    UnterminatedString,
    #[error("invalid escape {escape}")]
    InvalidEscape { escape: Redacted },
    #[error("invalid unicode escape")]
    InvalidUnicodeEscape,
    #[error("lone surrogate in unicode escape")]
    LoneSurrogate,
    #[error("control character U+{code:04X} must be escaped inside a string")]
    UnescapedControl { code: u32 },
    #[error("invalid number literal {literal}")]
    InvalidNumber { literal: Redacted },
    #[error("duplicate object key {key}")]
    DuplicateKey { key: Redacted },
    #[error("an object key may not be empty")]
    EmptyKey,
    #[error("value nesting exceeds the depth bound {bound}")]
    DepthExceeded { bound: usize },
    #[error("document node count exceeds the bound {bound}")]
    NodeCountExceeded { bound: usize },
    #[error("trailing content after the root value")]
    TrailingContent,
}

/// Decode one document's raw bytes.
///
/// The byte bound is checked first and against the RAW bytes, before UTF-8
/// validation allocates anything — master §3.3 places the hard bound at the I/O
/// boundary "before unbounded allocation".
pub fn decode(raw: &[u8], bounds: ConfigBounds) -> Result<Spanned<ConfigValue>, JsoncError> {
    let byte_bound = bounds.document_bytes().value();
    if raw.len() > byte_bound {
        return Err(JsoncError {
            kind: JsoncErrorKind::DocumentTooLarge {
                actual: raw.len(),
                bound: byte_bound,
            },
            path: ValuePath::root(),
            offset: byte_bound,
        });
    }
    let text = match core::str::from_utf8(raw) {
        Ok(text) => text,
        Err(error) => {
            return Err(JsoncError {
                kind: JsoncErrorKind::NotUtf8,
                path: ValuePath::root(),
                offset: error.valid_up_to(),
            })
        }
    };

    let mut decoder = Decoder {
        bytes: text.as_bytes(),
        text,
        pos: 0,
        nodes: 0,
        bounds,
    };

    decoder.skip_trivia(&ValuePath::root())?;
    if decoder.pos >= decoder.bytes.len() {
        return Err(JsoncError {
            kind: JsoncErrorKind::Empty,
            path: ValuePath::root(),
            offset: decoder.pos,
        });
    }

    let value = decoder.value(&ValuePath::root(), 1)?;
    decoder.skip_trivia(&ValuePath::root())?;
    if decoder.pos < decoder.bytes.len() {
        return Err(JsoncError {
            kind: JsoncErrorKind::TrailingContent,
            path: ValuePath::root(),
            offset: decoder.pos,
        });
    }
    Ok(value)
}

struct Decoder<'a> {
    bytes: &'a [u8],
    text: &'a str,
    pos: usize,
    nodes: usize,
    bounds: ConfigBounds,
}

impl<'a> Decoder<'a> {
    fn error(&self, kind: JsoncErrorKind, path: &ValuePath) -> JsoncError {
        JsoncError {
            kind,
            path: path.clone(),
            offset: self.pos,
        }
    }

    fn peek(&self) -> Option<u8> {
        self.bytes.get(self.pos).copied()
    }

    /// One node produced. Checked as nodes appear so the bound is a bound on
    /// allocation, not a post-hoc audit of it.
    fn count_node(&mut self, path: &ValuePath) -> Result<(), JsoncError> {
        self.nodes += 1;
        if self.nodes > self.bounds.document_nodes().value() {
            return Err(self.error(
                JsoncErrorKind::NodeCountExceeded {
                    bound: self.bounds.document_nodes().value(),
                },
                path,
            ));
        }
        Ok(())
    }

    /// Whitespace and comments. Both comment forms are the oracle's; a `/`
    /// that starts neither is a hard refusal rather than a literal slash.
    fn skip_trivia(&mut self, path: &ValuePath) -> Result<(), JsoncError> {
        loop {
            match self.peek() {
                Some(b' ' | b'\t' | b'\r' | b'\n') => self.pos += 1,
                Some(b'/') => match self.bytes.get(self.pos + 1).copied() {
                    Some(b'/') => {
                        self.pos += 2;
                        while let Some(byte) = self.peek() {
                            if matches!(byte, b'\r' | b'\n') {
                                break;
                            }
                            self.pos += 1;
                        }
                    }
                    Some(b'*') => {
                        let opened = self.pos;
                        self.pos += 2;
                        loop {
                            match self.peek() {
                                Some(b'*') if self.bytes.get(self.pos + 1) == Some(&b'/') => {
                                    self.pos += 2;
                                    break;
                                }
                                Some(_) => self.pos += 1,
                                None => {
                                    self.pos = opened;
                                    return Err(
                                        self.error(JsoncErrorKind::UnterminatedComment, path)
                                    );
                                }
                            }
                        }
                    }
                    _ => {
                        return Err(self.error(
                            JsoncErrorKind::Expected {
                                expected: "a comment",
                                found: Redacted::of("/"),
                            },
                            path,
                        ))
                    }
                },
                _ => return Ok(()),
            }
        }
    }

    /// The character at the cursor, for a diagnostic. Falls back to U+FFFD only
    /// when the cursor is past the end, which every caller has already excluded.
    fn current_char(&self) -> Redacted {
        Redacted::of(
            self.text[self.pos..]
                .chars()
                .next()
                .map(String::from)
                .unwrap_or_default(),
        )
    }

    fn value(
        &mut self,
        path: &ValuePath,
        depth: usize,
    ) -> Result<Spanned<ConfigValue>, JsoncError> {
        if depth > self.bounds.value_depth().value() {
            return Err(self.error(
                JsoncErrorKind::DepthExceeded {
                    bound: self.bounds.value_depth().value(),
                },
                path,
            ));
        }
        self.skip_trivia(path)?;
        let start = self.pos;
        let Some(byte) = self.peek() else {
            return Err(self.error(JsoncErrorKind::UnexpectedEnd, path));
        };
        // Charged BEFORE the scalar is built. Counting afterwards let a
        // document allocate past the limit and only then be rejected, which is
        // what the crate doc used to claim it did not do.
        if !matches!(byte, b'{' | b'[') {
            self.count_node(path)?;
        }
        let value = match byte {
            b'{' => return self.object(path, depth),
            b'[' => return self.array(path, depth),
            b'"' => ConfigValue::String(self.string(path)?),
            b't' => {
                self.literal("true", path)?;
                ConfigValue::Bool(true)
            }
            b'f' => {
                self.literal("false", path)?;
                ConfigValue::Bool(false)
            }
            b'n' => {
                self.literal("null", path)?;
                ConfigValue::Null
            }
            b'-' | b'0'..=b'9' => ConfigValue::Number(self.number(path)?),
            _ => {
                return Err(self.error(
                    JsoncErrorKind::Expected {
                        expected: "a value",
                        found: self.current_char(),
                    },
                    path,
                ))
            }
        };
        self.spanned(value, start, path)
    }

    fn spanned(
        &self,
        value: ConfigValue,
        start: usize,
        path: &ValuePath,
    ) -> Result<Spanned<ConfigValue>, JsoncError> {
        match ByteSpan::new(start, self.pos) {
            Some(span) => Ok(Spanned::new(value, span)),
            // Unreachable by construction — the cursor only advances — but this
            // crate has no panicking path, so it is an error and not an
            // `expect`.
            None => Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
        }
    }

    fn literal(&mut self, word: &'static str, path: &ValuePath) -> Result<(), JsoncError> {
        if self.text[self.pos..].starts_with(word) {
            self.pos += word.len();
            Ok(())
        } else {
            Err(self.error(
                JsoncErrorKind::Expected {
                    expected: word,
                    found: self.current_char(),
                },
                path,
            ))
        }
    }

    fn number(&mut self, path: &ValuePath) -> Result<ConfigNumber, JsoncError> {
        let start = self.pos;
        if self.peek() == Some(b'-') {
            self.pos += 1;
        }
        // Integer part: `0` alone, or a non-zero digit run. `01` is not JSON.
        match self.peek() {
            Some(b'0') => self.pos += 1,
            Some(b'1'..=b'9') => {
                while matches!(self.peek(), Some(b'0'..=b'9')) {
                    self.pos += 1;
                }
            }
            _ => return Err(self.invalid_number(start, path)),
        }
        if self.peek() == Some(b'.') {
            self.pos += 1;
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.invalid_number(start, path));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        if matches!(self.peek(), Some(b'e' | b'E')) {
            self.pos += 1;
            if matches!(self.peek(), Some(b'+' | b'-')) {
                self.pos += 1;
            }
            if !matches!(self.peek(), Some(b'0'..=b'9')) {
                return Err(self.invalid_number(start, path));
            }
            while matches!(self.peek(), Some(b'0'..=b'9')) {
                self.pos += 1;
            }
        }
        // A digit-run that ends against `0123` would otherwise split into two
        // values and the second would surface as trailing content, which names
        // the wrong defect.
        if matches!(self.peek(), Some(b'0'..=b'9') | Some(b'.')) {
            return Err(self.invalid_number(start, path));
        }
        Ok(ConfigNumber::from_literal(
            self.text[start..self.pos].to_owned(),
        ))
    }

    fn invalid_number(&mut self, start: usize, path: &ValuePath) -> JsoncError {
        // Report the whole run the author wrote, bounded by the next structural
        // byte, so the message names the literal rather than one character of it.
        let mut end = self.pos;
        while end < self.bytes.len()
            && !matches!(
                self.bytes[end],
                b' ' | b'\t' | b'\r' | b'\n' | b',' | b'}' | b']' | b':'
            )
        {
            end += 1;
        }
        let literal = Redacted::of(&self.text[start..end]);
        self.pos = start;
        self.error(JsoncErrorKind::InvalidNumber { literal }, path)
    }

    fn string(&mut self, path: &ValuePath) -> Result<String, JsoncError> {
        // Caller has already seen the opening quote.
        self.pos += 1;
        let mut out = String::new();
        let mut run_start = self.pos;
        loop {
            let Some(byte) = self.peek() else {
                return Err(self.error(JsoncErrorKind::UnterminatedString, path));
            };
            match byte {
                b'"' => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    return Ok(out);
                }
                b'\\' => {
                    out.push_str(&self.text[run_start..self.pos]);
                    self.pos += 1;
                    let decoded = self.escape(path)?;
                    out.push(decoded);
                    run_start = self.pos;
                }
                // JSON forbids a raw control character inside a string. The
                // oracle's parser reports it as an error too; keeping it typed
                // means an author is told which one.
                0x00..=0x1F => {
                    return Err(
                        self.error(JsoncErrorKind::UnescapedControl { code: byte as u32 }, path)
                    )
                }
                _ => self.pos += 1,
            }
        }
    }

    fn escape(&mut self, path: &ValuePath) -> Result<char, JsoncError> {
        let Some(byte) = self.peek() else {
            return Err(self.error(JsoncErrorKind::UnterminatedString, path));
        };
        self.pos += 1;
        let decoded = match byte {
            b'"' => '"',
            b'\\' => '\\',
            b'/' => '/',
            b'b' => '\u{0008}',
            b'f' => '\u{000C}',
            b'n' => '\n',
            b'r' => '\r',
            b't' => '\t',
            b'u' => return self.unicode_escape(path),
            _ => {
                self.pos -= 1;
                let escape = self.current_char();
                return Err(self.error(JsoncErrorKind::InvalidEscape { escape }, path));
            }
        };
        Ok(decoded)
    }

    fn unicode_escape(&mut self, path: &ValuePath) -> Result<char, JsoncError> {
        let first = self.hex4(path)?;
        // A high surrogate must be followed by its low surrogate. A lone one is
        // not a character and cannot become U+FFFD silently: that would let a
        // truncated credential-bearing value decode into something that looks
        // fine.
        if (0xD800..0xDC00).contains(&first) {
            if self.peek() != Some(b'\\') || self.bytes.get(self.pos + 1) != Some(&b'u') {
                return Err(self.error(JsoncErrorKind::LoneSurrogate, path));
            }
            self.pos += 2;
            let second = self.hex4(path)?;
            if !(0xDC00..0xE000).contains(&second) {
                return Err(self.error(JsoncErrorKind::LoneSurrogate, path));
            }
            let combined = 0x10000 + ((first - 0xD800) << 10) + (second - 0xDC00);
            return match char::from_u32(combined) {
                Some(value) => Ok(value),
                None => Err(self.error(JsoncErrorKind::InvalidUnicodeEscape, path)),
            };
        }
        if (0xDC00..0xE000).contains(&first) {
            return Err(self.error(JsoncErrorKind::LoneSurrogate, path));
        }
        match char::from_u32(first) {
            Some(value) => Ok(value),
            None => Err(self.error(JsoncErrorKind::InvalidUnicodeEscape, path)),
        }
    }

    fn hex4(&mut self, path: &ValuePath) -> Result<u32, JsoncError> {
        let mut value = 0u32;
        for _ in 0..4 {
            let Some(byte) = self.peek() else {
                return Err(self.error(JsoncErrorKind::InvalidUnicodeEscape, path));
            };
            let digit = match byte {
                b'0'..=b'9' => u32::from(byte - b'0'),
                b'a'..=b'f' => u32::from(byte - b'a') + 10,
                b'A'..=b'F' => u32::from(byte - b'A') + 10,
                _ => return Err(self.error(JsoncErrorKind::InvalidUnicodeEscape, path)),
            };
            value = value * 16 + digit;
            self.pos += 1;
        }
        Ok(value)
    }

    fn array(
        &mut self,
        path: &ValuePath,
        depth: usize,
    ) -> Result<Spanned<ConfigValue>, JsoncError> {
        let start = self.pos;
        self.count_node(path)?;
        self.pos += 1; // `[`
        let mut items: Vec<Spanned<ConfigValue>> = Vec::new();
        loop {
            self.skip_trivia(path)?;
            match self.peek() {
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
                // Closes an empty array, and closes one after the trailing comma
                // the oracle permits.
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                Some(_) => {}
            }
            let child_path = path.pushed_index(items.len());
            let item = self.value(&child_path, depth + 1)?;
            items.push(item);
            self.skip_trivia(path)?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b']') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
                Some(_) => {
                    return Err(self.error(
                        JsoncErrorKind::Expected {
                            expected: "',' or ']'",
                            found: self.current_char(),
                        },
                        path,
                    ))
                }
            }
        }
        self.spanned(ConfigValue::Array(items), start, path)
    }

    fn object(
        &mut self,
        path: &ValuePath,
        depth: usize,
    ) -> Result<Spanned<ConfigValue>, JsoncError> {
        let start = self.pos;
        self.count_node(path)?;
        self.pos += 1; // `{`
        let mut entries: Vec<(Spanned<ConfigKey>, Spanned<ConfigValue>)> = Vec::new();
        // Duplicate detection is a SET, not a rescan of `entries`. The linear
        // rescan was quadratic: 99,999 distinct short keys stays inside both the
        // node ceiling and the byte cap while performing 4,999,850,001 prior-key
        // comparisons, which is a CPU denial path inside the declared bounds.
        // `BTreeSet` and not `HashSet` because `RandomState` is ambient
        // randomness, which this workspace forbids; ordering here is a
        // deterministic consequence of the keys, never of a per-process seed.
        let mut seen: BTreeSet<String> = BTreeSet::new();
        loop {
            self.skip_trivia(path)?;
            match self.peek() {
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                Some(b'"') => {}
                Some(_) => {
                    return Err(self.error(
                        JsoncErrorKind::Expected {
                            expected: "a quoted key or '}'",
                            found: self.current_char(),
                        },
                        path,
                    ))
                }
            }
            let key_start = self.pos;
            let raw_key = self.string(path)?;
            let Some(key) = ConfigKey::new(raw_key.clone()) else {
                self.pos = key_start;
                return Err(self.error(JsoncErrorKind::EmptyKey, path));
            };
            if !seen.insert(raw_key.clone()) {
                self.pos = key_start;
                return Err(self.error(
                    JsoncErrorKind::DuplicateKey {
                        key: Redacted::of(raw_key),
                    },
                    path,
                ));
            }
            let key_span = match ByteSpan::new(key_start, self.pos) {
                Some(span) => span,
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
            };
            self.skip_trivia(path)?;
            match self.peek() {
                Some(b':') => self.pos += 1,
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
                Some(_) => {
                    return Err(self.error(
                        JsoncErrorKind::Expected {
                            expected: "':'",
                            found: self.current_char(),
                        },
                        path,
                    ))
                }
            }
            let child_path = path.pushed_key(entries.len(), raw_key);
            let value = self.value(&child_path, depth + 1)?;
            entries.push((Spanned::new(key, key_span), value));
            self.skip_trivia(path)?;
            match self.peek() {
                Some(b',') => self.pos += 1,
                Some(b'}') => {
                    self.pos += 1;
                    break;
                }
                None => return Err(self.error(JsoncErrorKind::UnexpectedEnd, path)),
                Some(_) => {
                    return Err(self.error(
                        JsoncErrorKind::Expected {
                            expected: "',' or '}'",
                            found: self.current_char(),
                        },
                        path,
                    ))
                }
            }
        }
        self.spanned(ConfigValue::Object(entries), start, path)
    }
}
