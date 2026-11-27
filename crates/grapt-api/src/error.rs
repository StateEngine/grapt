//! The closed refusal vocabulary and the redaction that makes it safe to render.
//!
//! Every way a request can stop at this boundary is one variant here. The
//! vocabulary is closed on purpose: adding a way to refuse breaks every
//! exhaustive match, which is the review pressure that keeps a new refusal from
//! quietly rendering something the old ones would not.
//!
//! ## Why observations are wrapped rather than omitted
//!
//! An error that says `recipient too long: 8192 bytes` is a probe oracle: it
//! tells a caller exactly how much of its payload the boundary accepted before
//! refusing, and repeated probes recover the bound and then the contents. The
//! rule this crate follows is that **a refusal names the BOUND, never the
//! observation**.
//!
//! Dropping the observation entirely would satisfy that rule but leave nothing
//! to witness and nothing to diagnose. Instead every observed value is carried
//! inside [`Redacted`], which has no accessor, no `Display` of its contents and
//! a `Debug` that prints a fixed sentinel. So the observation is present as
//! structure — a reviewer can see that a variant observed a length — and absent
//! as text, in `Display`, in `Debug`, and in any format string a downstream
//! renderer could reach for.

use core::fmt;

use grapt_types::{MailboxIdentityError, MAX_SEAT_RECIPIENT_ID_BYTES};
use thiserror::Error;

/// A value the boundary observed and must never render.
///
/// There is deliberately **no accessor**. A getter would be the whole bypass:
/// the redaction would then be a convention that every future error renderer
/// has to remember, rather than a property of the type. Both `Debug` and
/// `Display` print a fixed sentinel, so neither `{:?}` nor `{}` nor a
/// `format!` in a later lane can recover the value.
///
/// The inner value is retained rather than dropped so the refusal keeps its
/// shape: `DtoTooLarge` observed *a* length, and that fact is part of what the
/// variant means even though the number is unspeakable.
///
/// # No equality, deliberately (R1 finding H2)
///
/// This type derived `PartialEq`/`Eq` in the first candidate, and that was an
/// **offline guess oracle**: `Redacted::new` is public, so a holder of a
/// refusal could construct a candidate value and ask whether it equalled the
/// hidden one, one bit at a time, until the material was recovered. Equality on
/// a type whose entire purpose is to make a value unreadable hands the value
/// back through a side door.
///
/// The derives are gone and nothing replaces them. Callers that need to know
/// WHICH refusal occurred pattern-match the variant — `matches!(refusal,
/// ApiRefusal::DtoTooLarge { max: 8, .. })` — which reads the bound the refusal
/// legitimately names and cannot reach the observation at all.
#[derive(Clone)]
pub struct Redacted<T>(T);

impl<T> Redacted<T> {
    /// Wrap an observation. This is the only constructor, and the value is
    /// unreachable afterwards.
    pub fn new(value: T) -> Self {
        Self(value)
    }
}

/// Redacted in `Debug`. **This is the load-bearing one**, and the mutation
/// battery is what established that rather than the other way round.
///
/// `ApiRefusal` derives `Debug`, so a refusal formatted with `{:?}` — a panic
/// message, a structured log line, an assertion diagnostic — renders each field
/// through the field's own `Debug`. This impl is therefore the ONLY thing
/// standing between an observed credential and that output, and `QA23` mutates
/// exactly here.
///
/// The first version of `QA23` mutated the `Display` impl below and stayed
/// GREEN, which was a correct result about the wrong site: see that impl's note.
impl<T> fmt::Debug for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Redacted in `Display` too — but as belt-and-braces, not as the active guard.
///
/// Nothing reaches this today: every `#[error(...)]` format string on
/// `ApiRefusal` names only bounds and closed codes, and none interpolates an
/// `observed` field, so the outward rendering never asks a `Redacted` to display
/// itself. Mutating this impl to print the inner value is consequently
/// invisible — which is precisely what the battery reported when `QA23` was
/// pinned here.
///
/// It stays because it is the guard for the format string somebody adds later:
/// a future `#[error("... {observed}")]` renders `<redacted>` instead of the
/// material, so that edit is a harmless mistake rather than a leak. The
/// `T: Display` bound keeps such an edit compiling.
impl<T: fmt::Display> fmt::Display for Redacted<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<redacted>")
    }
}

/// Exactly what was wrong with a route path.
///
/// Closed. Each variant renders **its own** law or bound. R1 finding H3: the
/// first candidate funnelled every one of these through `ConflictingRoute`, so
/// an empty path, a missing leading slash, an over-long path and a duplicate
/// parameter name all rendered "the registered route patterns conflict" — a
/// false sentence from a boundary whose entire error thesis is precision. A
/// refusal must name the law it actually applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RoutePathDefect {
    /// The path was empty.
    Empty,
    /// The path did not begin with `/`.
    NoLeadingSlash,
    /// The path exceeded the byte bound.
    TooLong { max: usize },
    /// The path carried more `/`-separated segments than the bound admits.
    TooManySegments { max: usize },
    /// A `//` or a trailing `/` produced a segment with no content.
    EmptySegment,
    /// A parameter shared its segment with static text, e.g. `{id}-suffix`.
    PartialSegmentParameter,
    /// A `{...}` capture was unbalanced, unnamed, or not an ASCII identifier.
    MalformedParameter,
    /// A `{*rest}` catch-all appeared anywhere but the final segment.
    CatchAllNotFinal,
    /// One parameter name occurred twice in a single path.
    DuplicateParameterName,
    /// A v0.7 `:param` capture, which the router panics on.
    LegacyColonCapture,
    /// A v0.7 `*wildcard` capture, which the router panics on.
    LegacyStarCapture,
}

impl fmt::Display for RoutePathDefect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RoutePathDefect::Empty => f.write_str("a route path must not be empty"),
            RoutePathDefect::NoLeadingSlash => f.write_str("a route path must begin with `/`"),
            RoutePathDefect::TooLong { max } => {
                write!(f, "a route path must be at most {max} bytes")
            }
            RoutePathDefect::TooManySegments { max } => {
                write!(f, "a route path must carry at most {max} segments")
            }
            RoutePathDefect::EmptySegment => {
                f.write_str("a route path must not contain an empty segment")
            }
            RoutePathDefect::PartialSegmentParameter => {
                f.write_str("a route parameter must occupy its whole segment")
            }
            RoutePathDefect::MalformedParameter => f.write_str(
                "a route parameter must be a balanced `{name}` naming ASCII alphanumerics or `_`",
            ),
            RoutePathDefect::CatchAllNotFinal => f.write_str(
                "a `{*catch_all}` route parameter is admitted only as the final segment",
            ),
            RoutePathDefect::DuplicateParameterName => {
                f.write_str("a route path must not repeat a parameter name")
            }
            RoutePathDefect::LegacyColonCapture => {
                f.write_str("a route segment must not begin with `:`; use `{name}`")
            }
            RoutePathDefect::LegacyStarCapture => {
                f.write_str("a route segment must not begin with `*`; use `{*name}`")
            }
        }
    }
}

/// Why the boundary refused.
///
/// Closed (law C3). Note what is NOT here: there is no `Other`, no
/// `Message(String)` and no variant carrying caller text, because any of those
/// would be an open channel for exactly the material the rest of the vocabulary
/// is redacting.
///
/// # No equality, deliberately (R1 finding H2)
///
/// `PartialEq`/`Eq` are gone for the reason given on [`Redacted`]: a derived
/// equality over a variant carrying a `Redacted` field compares the hidden
/// value, so a caller could brute-force a refusal's contents by constructing
/// twins. Match on the variant instead; every bound a caller may legitimately
/// read is a plain field it can bind by pattern.
#[derive(Debug, Clone, Error)]
pub enum ApiRefusal {
    /// The presented credential did not verify.
    ///
    /// One variant for every failure mode — unknown subject, malformed
    /// material, wrong shape. Distinguishing them outward would tell a caller
    /// whether a subject exists, which is the enumeration oracle.
    #[error("the presented credential did not verify")]
    CredentialRejected { observed: Redacted<String> },

    /// A caller-authored [`grapt_types::AuthenticatedClientId`] was presented
    /// as if it were proof.
    ///
    /// `AuthenticatedClientId::new` is public, so a caller can author a
    /// syntactically perfect id. It is refused because it did not come from
    /// this boundary's own verification, not because it looks wrong.
    #[error("client identity must originate from this boundary's verification")]
    ClientIdentityNotIssuedHere { observed: Redacted<String> },

    /// The injected identity authority produced a value that is not a usable
    /// client identity.
    ///
    /// `AuthenticatedClientId::new` refuses an empty string, and this crate
    /// refuses rather than unwraps: the workspace denies `unwrap_used` and
    /// `expect_used`, and a boundary that panics on a misconfigured adapter has
    /// turned a configuration defect into a process kill.
    #[error("the injected identity authority produced no usable client identity")]
    ClientIdentityUnavailable,

    /// The request carried no verified subject at all.
    #[error("the request is unauthenticated")]
    Unauthenticated,

    /// A DTO named a recipient that disagrees with the server-derived one.
    ///
    /// Refused rather than overridden: a silent override would make the two
    /// values look interchangeable, and the next lane would read the DTO one.
    #[error("the request names a recipient other than the authenticated one")]
    RecipientDisagreement { observed: Redacted<String> },

    /// The principal was minted under a directory generation that is no longer
    /// current.
    #[error("the authenticated principal was minted under a superseded directory generation")]
    StaleDirectoryGeneration,

    /// The principal is authentic but no capability proof authorizes this
    /// action on this resource.
    #[error("no capability proof authorizes this action for this recipient")]
    NoCapabilityProof,

    /// A capability proof was supplied but does not cover the requested
    /// capability at the requested scope.
    #[error("the supplied capability proof does not authorize this action")]
    CapabilityProofDoesNotAuthorize,

    /// The encoded request exceeds the decode bound.
    ///
    /// Refused **before** parsing, so a hostile payload is never fully
    /// materialized. The message names `max` and nothing else.
    #[error("request body exceeds the {max}-byte decode bound")]
    DtoTooLarge {
        max: usize,
        observed: Redacted<usize>,
    },

    /// A recipient in configuration or a DTO was empty or over the bound.
    ///
    /// Names the type's own constant rather than the observed length.
    ///
    /// This variant is now used for **recipients only**. R1 finding H3 found it
    /// standing in for a non-UTF-8 body, a malformed subject and an empty policy
    /// version, each of which then rendered a sentence about seat recipients.
    #[error("a seat recipient must be non-empty and at most {MAX_SEAT_RECIPIENT_ID_BYTES} bytes")]
    RecipientMalformed { observed: Redacted<String> },

    /// The request body was not valid UTF-8.
    ///
    /// Its own variant (H3): this is a property of the encoded body and has
    /// nothing to do with a seat recipient.
    #[error("a request body must be valid UTF-8")]
    BodyNotUtf8,

    /// An authentication subject was empty or over the bound.
    #[error("an authentication subject must be non-empty and at most {max} bytes")]
    SubjectMalformed {
        max: usize,
        observed: Redacted<String>,
    },

    /// An authentication policy version was empty.
    #[error("an authentication policy version must be non-empty")]
    PolicyVersionMalformed,

    /// A route path did not satisfy the admitted grammar.
    ///
    /// Distinct from [`ApiRefusal::ConflictingRoute`], which means two
    /// well-formed paths cannot coexist. `defect` names the exact rule.
    #[error("{defect}")]
    RouteMalformed {
        defect: RoutePathDefect,
        observed: Redacted<String>,
    },

    /// Directory configuration declared more mappings than the bound admits.
    #[error("the directory admits at most {max} principal mappings")]
    DirectoryBoundExceeded { max: usize },

    /// Two configuration entries claimed the same verified subject.
    ///
    /// One subject resolves to exactly one recipient (R4HA-03), so a duplicate
    /// is a configuration defect, not a last-one-wins merge.
    #[error("a verified subject may appear in the directory exactly once")]
    DuplicateSubject { observed: Redacted<String> },

    /// The registry is full and the acquisition names a recipient it is not
    /// already holding.
    #[error("the connection registry admits at most {max} recipients")]
    TakeoverBoundExceeded { max: usize },

    /// A lease that has been displaced tried to act as the current owner.
    #[error("this lease has been displaced by a newer acquisition")]
    LeaseDisplaced,

    /// Route registration past the module capacity.
    #[error("the router admits at most {max} modules")]
    RouterBoundExceeded { max: usize },

    /// The same (method, path) was registered twice.
    #[error("a route may be registered exactly once")]
    DuplicateRoute { observed: Redacted<String> },

    /// Two registered path patterns conflict.
    #[error("the registered route patterns conflict")]
    ConflictingRoute { observed: Redacted<String> },

    /// A bound was declared as zero.
    ///
    /// Every capacity in this crate is a checked positive value with no
    /// `Default`: a zero bound is a configuration mistake that would otherwise
    /// present as "everything is refused" and read like a working deny-all.
    #[error("{bound} must be a positive capacity")]
    BoundNotPositive { bound: &'static str },
}

impl ApiRefusal {
    /// Build the malformed-recipient refusal from the owning type's own error.
    ///
    /// The bound lives in `grapt-types` and is enforced by
    /// `SeatRecipientId::try_new`; re-implementing it here would be a second
    /// carrier for one law, and the two would drift.
    pub fn from_recipient_error(error: &MailboxIdentityError, observed: impl Into<String>) -> Self {
        // Both arms refuse identically on purpose: which of "empty" and "too
        // long" occurred is an observation about the caller's input, and
        // telling them apart outward is the oracle this vocabulary avoids.
        let _ = error;
        ApiRefusal::RecipientMalformed {
            observed: Redacted::new(observed.into()),
        }
    }
}
