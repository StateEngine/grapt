use serde::de::DeserializeOwned;
use serde_path_to_error::Segment;
use std::fmt;

/// Total bound applied to a rendered target path, in characters.
///
/// The bound is applied once, when the error is constructed, so every public
/// surface — [`DecodeError::path`], `Display` and `Debug` — shares it. No
/// accessor can return an unbounded path.
const MAX_PATH_CHARS: usize = 256;

const TRUNCATION_MARKER: &str = "...";
const REDACTED_SEGMENT: &str = "<redacted>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// What a rendered path may say about a NAMED segment.
///
/// # Why the default redacts, and why it cannot be smarter
///
/// A named segment reaches the path because the DOCUMENT contained that key. It
/// may be a struct field name, in which case echoing it is harmless schema
/// structure; it may be a map key, in which case it is decoded content and can
/// carry a tenant id, an account name or a secret. `serde` reports both through
/// one channel — `deserialize_struct` and `deserialize_map` both surface the key
/// the input supplied — so the boundary genuinely cannot tell them apart.
///
/// An undecidable safety question is answered closed. What is preserved is the
/// part that is unambiguously schema rather than content: the path's depth, its
/// ordering, and its sequence indices, which come from the target's own shape.
///
/// The one name that survives redaction is the field named in serde's
/// `missing field` report, and only because that name provably comes from the
/// TARGET rather than from the input — an absent field cannot have been supplied
/// by the document. That recovery is crate-private (`missing_field`) and is
/// admitted only when the reported name is a plain identifier.
pub enum NamedSegmentPolicy {
    /// Every named segment renders as a fixed marker. The default.
    Redact,
    /// The boundary asserts that its object keys are schema-derived, and accepts
    /// up to `max_chars` of each being echoed on failure.
    Reveal { max_chars: usize },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Caller-owned resource limits applied before serde deserialization.
///
/// Byte and nesting limits must be positive. Keeping construction fallible and
/// fields private prevents an accidental zero limit or a hidden universal
/// default.
pub struct DecodeLimits {
    max_input_bytes: usize,
    max_nesting_depth: usize,
    named_segments: NamedSegmentPolicy,
}

impl DecodeLimits {
    /// Creates byte and structural-nesting limits for one decode boundary.
    ///
    /// Named path segments are REDACTED by default. This inverts the previous
    /// default, which echoed up to 64 characters of every key unless a caller
    /// opted out — so a boundary that had not thought about its key space leaked
    /// by omission, which is the wrong direction for a decision nobody makes
    /// explicitly.
    pub fn new(max_input_bytes: usize, max_nesting_depth: usize) -> Result<Self, LimitError> {
        if max_input_bytes == 0 {
            return Err(LimitError::InputBytes);
        }
        if max_nesting_depth == 0 {
            return Err(LimitError::NestingDepth);
        }
        Ok(Self {
            max_input_bytes,
            max_nesting_depth,
            named_segments: NamedSegmentPolicy::Redact,
        })
    }

    /// OPT IN to echoing named path segments, bounded at `max_chars`.
    ///
    /// The caller is asserting that this boundary's object keys are schema
    /// names, not decoded content. That assertion is not checkable here, which
    /// is exactly why it is a named call at the call site rather than a number
    /// with a safe-looking default.
    ///
    /// `0` is not a degenerate reveal: it resolves to
    /// [`NamedSegmentPolicy::Redact`], so a computed budget that reaches zero
    /// cannot produce a policy that claims to reveal and does not.
    pub fn revealing_named_path_segments(mut self, max_chars: usize) -> Self {
        self.named_segments = if max_chars == 0 {
            NamedSegmentPolicy::Redact
        } else {
            NamedSegmentPolicy::Reveal { max_chars }
        };
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Identifies an invalid zero-valued decode limit.
pub enum LimitError {
    /// The maximum input byte count was zero.
    InputBytes,
    /// The maximum structural nesting depth was zero.
    NestingDepth,
}

impl fmt::Display for LimitError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LimitError::InputBytes => f.write_str("maximum input bytes must be positive"),
            LimitError::NestingDepth => f.write_str("maximum nesting depth must be positive"),
        }
    }
}

impl std::error::Error for LimitError {}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Stable error categories that never contain a rejected input value.
pub enum DecodeErrorKind {
    /// Input length exceeded the caller's byte budget before serde ran.
    ///
    /// It carries the CONFIGURED limit and not the observed length. The observed
    /// length is a measurement of the rejected input: against a boundary that
    /// wraps one secret-bearing value, the exact byte count is a length oracle,
    /// and repeated probing narrows the value without the decoder ever accepting
    /// anything. [`NestingTooDeep`](DecodeErrorKind::NestingTooDeep) already had
    /// the correct shape; this variant now matches it.
    InputTooLarge { limit: usize },
    /// Object/array nesting exceeded the caller's depth budget before serde ran.
    NestingTooDeep { limit: usize },
    /// The input was not syntactically valid JSON.
    Syntax,
    /// A valid JSON value did not satisfy the target serde schema.
    Data,
    /// JSON ended before one complete target value was available.
    EndOfInput,
    /// Bytes remained after one complete target value.
    TrailingData,
}

#[derive(Clone, PartialEq, Eq)]
/// A redacted JSON boundary failure with a structured target path.
///
/// The underlying serde error and the input are intentionally not retained. The
/// path is bounded per segment and in total when the error is constructed, so
/// no public accessor can reveal an unbounded key.
///
/// # No line or column
///
/// The error used to carry serde's one-based line and column and render them in
/// `Display`. A column is an offset into the rejected document, so it measures
/// the exact encoded length of everything preceding the failure on that line —
/// including a secret earlier in the same object. That is a length oracle
/// reported on every failure, and unlike the path it cannot be bounded without
/// destroying the only thing it was for.
///
/// What replaces it is the structural path, which says WHERE in the target
/// schema the failure was without saying where in the input.
pub struct DecodeError {
    kind: DecodeErrorKind,
    path: String,
}

impl DecodeError {
    /// Returns the stable failure category.
    pub fn kind(&self) -> &DecodeErrorKind {
        &self.kind
    }

    /// Returns the bounded, redacted serde target path, rooted at `$`.
    ///
    /// The value is already bounded by the [`DecodeLimits`] in force at the
    /// failing decode; it is identical to the path rendered by `Display`.
    pub fn path(&self) -> &str {
        &self.path
    }
}

impl fmt::Debug for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DecodeError")
            .field("kind", &self.kind)
            .field("path", &self.path)
            .finish()
    }
}

impl fmt::Display for DecodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let category = match &self.kind {
            DecodeErrorKind::InputTooLarge { .. } => "input exceeds byte limit",
            DecodeErrorKind::NestingTooDeep { .. } => "input exceeds nesting limit",
            DecodeErrorKind::Syntax => "invalid JSON syntax",
            DecodeErrorKind::Data => "JSON value violates schema",
            DecodeErrorKind::EndOfInput => "unexpected end of JSON input",
            DecodeErrorKind::TrailingData => "trailing JSON data",
        };
        write!(f, "{category} at {}", self.path)
    }
}

impl std::error::Error for DecodeError {}

/// Strictly decodes exactly one JSON value under caller-provided limits.
///
/// The target type owns unknown-field, enum, required-field, and newtype rules.
/// This boundary adds no coercion or defaults. Input length and JSON structural
/// depth are checked without allocation before target deserialization begins.
pub fn decode_json<T>(input: &[u8], limits: DecodeLimits) -> Result<T, DecodeError>
where
    T: DeserializeOwned,
{
    if input.len() > limits.max_input_bytes {
        return Err(DecodeError {
            kind: DecodeErrorKind::InputTooLarge {
                limit: limits.max_input_bytes,
            },
            path: "$".to_owned(),
        });
    }
    preflight_depth(input, limits.max_nesting_depth)?;

    let mut deserializer = serde_json::Deserializer::from_slice(input);
    let value = serde_path_to_error::deserialize(&mut deserializer).map_err(|error| {
        let path = render_path(error.path(), limits.named_segments);
        decode_error(path, error.into_inner(), false)
    })?;
    deserializer
        .end()
        .map_err(|error| decode_error("$".to_owned(), error, true))?;
    Ok(value)
}

fn preflight_depth(input: &[u8], limit: usize) -> Result<(), DecodeError> {
    let mut depth = 0;
    let mut in_string = false;
    let mut escaped = false;
    for byte in input {
        if in_string {
            if escaped {
                escaped = false;
                continue;
            }
            if *byte == b'\\' {
                escaped = true;
                continue;
            }
            if *byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if *byte == b'"' {
            in_string = true;
            continue;
        }
        if matches!(*byte, b'{' | b'[') {
            depth += 1;
            if depth > limit {
                return Err(DecodeError {
                    kind: DecodeErrorKind::NestingTooDeep { limit },
                    path: "$".to_owned(),
                });
            }
            continue;
        }
        if matches!(*byte, b'}' | b']') {
            depth = depth.saturating_sub(1);
        }
    }
    Ok(())
}

/// Renders a serde target path under the boundary's named-segment policy, with a
/// total character bound.
///
/// Segment *shape* — depth, ordering and sequence indices — is preserved
/// exactly, because it comes from the target's own structure rather than from
/// decoded content. A sequence index says which element of a schema-declared
/// list failed; it is not a key the document chose. Named segments follow
/// [`NamedSegmentPolicy`], which defaults to redaction.
fn render_path(path: &serde_path_to_error::Path, policy: NamedSegmentPolicy) -> String {
    let mut rendered = String::from("$");
    for segment in path.iter() {
        match segment {
            Segment::Seq { index } => {
                rendered.push('[');
                rendered.push_str(&index.to_string());
                rendered.push(']');
            }
            Segment::Map { key } => {
                rendered.push('.');
                rendered.push_str(&bound_segment(key, policy));
            }
            Segment::Enum { variant } => {
                rendered.push('.');
                rendered.push_str(&bound_segment(variant, policy));
            }
            Segment::Unknown => rendered.push_str(".?"),
        }
        if rendered.chars().count() >= MAX_PATH_CHARS {
            return bound_total(rendered);
        }
    }
    bound_total(rendered)
}

fn bound_segment(segment: &str, policy: NamedSegmentPolicy) -> String {
    let max_segment_chars = match policy {
        NamedSegmentPolicy::Redact => return REDACTED_SEGMENT.to_owned(),
        NamedSegmentPolicy::Reveal { max_chars } => max_chars,
    };
    if segment.chars().count() <= max_segment_chars {
        return segment.to_owned();
    }
    // The marker counts against the caller's budget, so a bounded segment never
    // renders more characters than the limit it advertises.
    let kept = max_segment_chars.saturating_sub(TRUNCATION_MARKER.chars().count());
    let mut bounded: String = segment.chars().take(kept).collect();
    bounded.push_str(TRUNCATION_MARKER);
    bounded
}

/// Bounds a rendered path so its total length, *including* the truncation
/// marker, never exceeds [`MAX_PATH_CHARS`].
fn bound_total(rendered: String) -> String {
    if rendered.chars().count() <= MAX_PATH_CHARS {
        return rendered;
    }
    let kept = MAX_PATH_CHARS.saturating_sub(TRUNCATION_MARKER.chars().count());
    let mut bounded: String = rendered.chars().take(kept).collect();
    bounded.push_str(TRUNCATION_MARKER);
    bounded
}

fn decode_error(mut path: String, error: serde_json::Error, trailing: bool) -> DecodeError {
    if let Some(field) = missing_field(&error) {
        path.push('.');
        path.push_str(&field);
        path = bound_total(path);
    }
    let kind = if trailing {
        DecodeErrorKind::TrailingData
    } else {
        match error.classify() {
            serde_json::error::Category::Io | serde_json::error::Category::Syntax => {
                DecodeErrorKind::Syntax
            }
            serde_json::error::Category::Data => DecodeErrorKind::Data,
            serde_json::error::Category::Eof => DecodeErrorKind::EndOfInput,
        }
    };
    DecodeError { kind, path }
}

/// Recovers the name of an absent required field so the path can point at it.
///
/// `serde` does not expose the missing field structurally, and the name it
/// reports comes from the target schema rather than from the rejected input.
/// The recovered name is therefore diagnostic only: it never selects behaviour,
/// and it is admitted solely when it is a plain Rust-style identifier, so no
/// arbitrary custom error text can reach a public path.
///
/// This is the ONE name that survives the default redaction, and the reason is
/// specific rather than convenient: the field is missing, so the document did
/// not supply it, so the name cannot be document-derived. Every other named
/// segment reached the path because the input contained that key.
fn missing_field(error: &serde_json::Error) -> Option<String> {
    if error.classify() != serde_json::error::Category::Data {
        return None;
    }
    let message = error.to_string();
    let field = message.strip_prefix("missing field `")?.split('`').next()?;
    let identifier = !field.is_empty()
        && field.len() <= 64
        && field
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_');
    identifier.then(|| field.to_owned())
}
