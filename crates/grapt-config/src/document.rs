//! The decoded shape of one configuration document, and the span that proves
//! where every value came from.
//!
//! # Three deliberate representation choices
//!
//! **Objects are an ordered `Vec`, not a map**, because the oracle decodes with
//! `propertyOrder: "original"` so order is observable — and because keeping the
//! vector is what makes a duplicate key representable at all.
//!
//! **Numbers keep their literal text**, because `f64` loses `9007199254740993`
//! and the constitution forbids "permissive coercions". [`ConfigNumber`] offers
//! fallible readers and no lossy accessor to reach for by accident.
//!
//! **`Null` is a value, not an absence.** Master's presence categories make
//! missing and explicit `null` distinct, and `Option<T>` is "never used to guess
//! a wire category".
//!
//! # What `Debug` may say
//!
//! A config blob is secret-bearing by default, so a derived `Debug` over this
//! tree would print every configured value into any trace that formats a
//! document. The formatters below report shape and never content; values are
//! reachable only through named accessors.

use core::fmt;

/// A half-open byte range inside one document's raw bytes.
///
/// Byte offsets, not line/column: exact, cheap and stable under any newline
/// convention. A renderer that wants a line number computes it from the bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSpan {
    start: usize,
    end: usize,
}

impl ByteSpan {
    /// `None` when the range is inverted. A span whose end precedes its start
    /// is not a small span, it is a wrong one, and returning it would let a
    /// provenance claim point at nothing.
    pub fn new(start: usize, end: usize) -> Option<Self> {
        if end < start {
            None
        } else {
            Some(Self { start, end })
        }
    }

    pub fn start(self) -> usize {
        self.start
    }

    pub fn end(self) -> usize {
        self.end
    }

    pub fn len(self) -> usize {
        self.end - self.start
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

/// A value paired with the exact bytes it occupied.
#[derive(Clone, PartialEq, Eq)]
pub struct Spanned<T> {
    value: T,
    span: ByteSpan,
}

impl<T> Spanned<T> {
    pub fn new(value: T, span: ByteSpan) -> Self {
        Self { value, span }
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn span(&self) -> ByteSpan {
        self.span
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

/// Shape and position only — never the wrapped value. See the module note.
impl<T> fmt::Debug for Spanned<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Spanned")
            .field("start", &self.span.start)
            .field("end", &self.span.end)
            .finish_non_exhaustive()
    }
}

/// One object key.
///
/// A newtype rather than a bare `String` so a key and a string VALUE cannot be
/// interchanged by position in a tuple, and so the empty key is refused once
/// here instead of at every reader.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ConfigKey(String);

impl ConfigKey {
    /// `None` for the empty key. JSON permits `""`; a configuration key that
    /// cannot be named cannot be resolved by name, so this crate does not admit
    /// one and the document carrying it becomes an invalid contributor.
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// An authored key may itself be secret, so the shape is public and the bytes
/// are not: no length, no prefix, nothing that narrows the material.
impl fmt::Debug for ConfigKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigKey(<redacted>)")
    }
}

/// A JSON number, retained exactly as written.
#[derive(Clone, PartialEq, Eq)]
pub struct ConfigNumber(String);

impl ConfigNumber {
    /// The literal is produced by the decoder, which has already proved it is
    /// well-formed JSON. This constructor exists for that decoder and for
    /// witnesses; it re-checks nothing and promises nothing beyond retention.
    pub(crate) fn from_literal(literal: String) -> Self {
        Self(literal)
    }

    /// The exact bytes the document contained.
    pub fn literal(&self) -> &str {
        &self.0
    }

    /// `None` when the literal is not an exact `i64` — including every literal
    /// with a fraction or an exponent. It does not round and it does not
    /// truncate.
    pub fn as_i64(&self) -> Option<i64> {
        self.0.parse::<i64>().ok()
    }

    /// `None` when the literal is not an exact `u64`.
    pub fn as_u64(&self) -> Option<u64> {
        self.0.parse::<u64>().ok()
    }

    /// The nearest `f64`, and `None` when the parse fails or the result is not
    /// finite. Named `lossy` because that is what it is: a caller that needs the
    /// exact value asks for the literal.
    pub fn as_f64_lossy(&self) -> Option<f64> {
        match self.0.parse::<f64>() {
            Ok(value) if value.is_finite() => Some(value),
            _ => None,
        }
    }
}

/// The literal is a number, not secret-bearing content in the sense the module
/// note protects — but it is still a configured value, so it is not printed.
impl fmt::Debug for ConfigNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("ConfigNumber(<value>)")
    }
}

/// One decoded configuration value.
///
/// Closed, and exhaustively matched by every reader. Master: "Event consumers
/// exhaustively match closed variants"; the same rule is why there is no
/// `Other`/`Unknown` arm here to absorb a future shape silently.
#[derive(Clone, PartialEq, Eq)]
pub enum ConfigValue {
    Null,
    Bool(bool),
    Number(ConfigNumber),
    String(String),
    Array(Vec<Spanned<ConfigValue>>),
    /// Ordered, and ordered on purpose — see the module note.
    Object(Vec<(Spanned<ConfigKey>, Spanned<ConfigValue>)>),
}

impl ConfigValue {
    /// The variant's label, for diagnostics that must say what was found
    /// without saying what it contained.
    pub fn kind(&self) -> &'static str {
        match self {
            ConfigValue::Null => "null",
            ConfigValue::Bool(_) => "bool",
            ConfigValue::Number(_) => "number",
            ConfigValue::String(_) => "string",
            ConfigValue::Array(_) => "array",
            ConfigValue::Object(_) => "object",
        }
    }

    /// The value bound to `key`, or `None` when the key is ABSENT.
    ///
    /// A key bound to `null` returns `Some(&ConfigValue::Null)`. That
    /// distinction is the point: master's presence categories make missing and
    /// explicit null different states, and a reader that wants to treat them
    /// alike must say so itself.
    pub fn get(&self, key: &str) -> Option<&Spanned<ConfigValue>> {
        match self {
            ConfigValue::Object(entries) => entries
                .iter()
                .find(|(k, _)| k.value().as_str() == key)
                .map(|(_, v)| v),
            _ => None,
        }
    }

    /// Whether this is an object that DEFINES `key`, null included.
    pub fn defines(&self, key: &str) -> bool {
        self.get(key).is_some()
    }

    pub fn as_bool(&self) -> Option<bool> {
        match self {
            ConfigValue::Bool(value) => Some(*value),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<&str> {
        match self {
            ConfigValue::String(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_number(&self) -> Option<&ConfigNumber> {
        match self {
            ConfigValue::Number(value) => Some(value),
            _ => None,
        }
    }

    pub fn as_array(&self) -> Option<&[Spanned<ConfigValue>]> {
        match self {
            ConfigValue::Array(items) => Some(items),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&[(Spanned<ConfigKey>, Spanned<ConfigValue>)]> {
        match self {
            ConfigValue::Object(entries) => Some(entries),
            _ => None,
        }
    }
}

/// Kind only. A derived formatter here would print every configured string,
/// which is exactly what the module note forbids.
impl fmt::Debug for ConfigValue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConfigValue::Array(items) => write!(f, "ConfigValue::Array(len {})", items.len()),
            ConfigValue::Object(entries) => {
                write!(f, "ConfigValue::Object(keys {})", entries.len())
            }
            other => write!(f, "ConfigValue::{}", capitalise(other.kind())),
        }
    }
}

fn capitalise(label: &str) -> String {
    let mut chars = label.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Authored bytes that a public diagnostic must never reveal.
///
/// The decoder CARRIES the offending fragment but cannot let it cross a `Debug`
/// or a `Display`. No LENGTH either, following `grapt_policy::Secret`'s
/// precedent ("no length, no prefix — nothing that narrows the material"): a
/// secret-dependent length is a weak oracle, and the byte offset already
/// localizes the failure exactly.
#[derive(Clone, PartialEq, Eq)]
pub struct Redacted(String);

impl Redacted {
    pub(crate) fn of(raw: impl Into<String>) -> Self {
        Self(raw.into())
    }
}

impl fmt::Debug for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

impl fmt::Display for Redacted {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// A path to one value inside a document, for structured decode failures.
///
/// Master: "User-authored configuration and public request decoding preserve
/// structured paths through a `serde_path_to_error`-class boundary." This is
/// that path. It is built as the decoder descends, so a failure names where it
/// happened rather than only what happened.
///
/// # Why a key segment renders as an ORDINAL and not as its name
///
/// The first version rendered `$.providers.anthropic.apiKey` — a key can itself
/// be secret-bearing and a public `Display` was emitting it verbatim. Redacting
/// the name would keep the path honest but useless, since every sibling would
/// render identically. The ordinal locates the value exactly, leaks nothing, and
/// is stable because objects retain document order. The name is still CARRIED,
/// as [`Redacted`], for a future trusted consumer.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct ValuePath(Vec<PathSegment>);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PathSegment {
    Key { ordinal: usize, name: Redacted },
    Index(usize),
}

impl ValuePath {
    pub fn root() -> Self {
        Self(Vec::new())
    }

    pub fn segments(&self) -> &[PathSegment] {
        &self.0
    }

    pub(crate) fn pushed_key(&self, ordinal: usize, name: impl Into<String>) -> Self {
        let mut next = self.0.clone();
        next.push(PathSegment::Key {
            ordinal,
            name: Redacted::of(name),
        });
        Self(next)
    }

    pub(crate) fn pushed_index(&self, index: usize) -> Self {
        let mut next = self.0.clone();
        next.push(PathSegment::Index(index));
        Self(next)
    }

    /// A stable rendering: `$`, `$.#0`, `$[0]`, `$.#1[2].#0`.
    pub fn render(&self) -> String {
        let mut out = String::from("$");
        for segment in &self.0 {
            match segment {
                PathSegment::Key { ordinal, .. } => {
                    out.push_str(".#");
                    out.push_str(&ordinal.to_string());
                }
                PathSegment::Index(index) => {
                    out.push('[');
                    out.push_str(&index.to_string());
                    out.push(']');
                }
            }
        }
        out
    }
}

impl fmt::Debug for ValuePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ValuePath({})", self.render())
    }
}

impl fmt::Display for ValuePath {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.render())
    }
}
