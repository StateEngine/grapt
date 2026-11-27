//! The bounded decode seam and the identity-free DTO law.
//!
//! A serves no request, so why does it own this? Because a decode bound and
//! "what a decoded value may be" are **vocabulary and policy**, not I/O. A host
//! lane enforces wire-level byte limits on the socket; this module enforces what
//! may exist as a value once those bytes are in memory, and what a DTO is
//! forbidden to carry no matter how the wire delivered it.
//!
//! ## Two rules, and they fail differently
//!
//! **The bound is checked before parsing.** Not after, and not during: a
//! hostile payload must never be fully materialized into a structured value
//! whose size is discovered on the way out. The check is on the encoded length
//! and it is the first thing [`decode`] does.
//!
//! **No DTO may be, carry, or select an identity.** There is no field here
//! typed as [`crate::auth::AuthenticatedSeatPrincipal`],
//! [`crate::auth::VerifiedSubject`], [`crate::takeover::RecipientLease`], or any
//! `grapt-types` grant or lease — and there is no accessor that returns one.
//! That absence is witnessed by a compile-fail probe rather than a runtime
//! test, because the property is "this does not typecheck", and a runtime test
//! can only ever observe the cases someone thought to write.
//!
//! ## The recipient field is here to be refused against
//!
//! A route may legitimately carry a recipient in its body. Three separate rules
//! govern it, and they are separate because they fail differently:
//!
//! 1. **Selection** — it is never read when choosing the principal. Not
//!    preferred-less-than, not a fallback: not read. The type system carries
//!    this one, because `SeatPrincipalDirectory::resolve` takes no such
//!    parameter.
//! 2. **Disagreement** — a value that disagrees with the server-derived
//!    recipient is a typed refusal before any application call.
//! 3. **Agreement is not evidence** — a DTO recipient that happens to match
//!    grants nothing, and appears in no receipt as though it did.

use grapt_types::SeatRecipientId;

use crate::error::{ApiRefusal, Redacted};

/// A checked positive decode bound, with no `Default`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MaxRequestDtoBytes(usize);

impl MaxRequestDtoBytes {
    pub fn try_new(value: usize) -> Result<Self, ApiRefusal> {
        if value == 0 {
            return Err(ApiRefusal::BoundNotPositive {
                bound: "max_request_dto_bytes",
            });
        }
        Ok(Self(value))
    }

    pub fn value(self) -> usize {
        self.0
    }
}

/// A decoded request body.
///
/// Note what it does not have: no principal, no verified subject, no lease, no
/// grant, no client identity, and no method returning any of those. A DTO is
/// caller-authored data, and a caller-authored value that could name authority
/// is the bypass every other seal in this crate exists to close.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestEnvelope {
    body: String,
    recipient: Option<SeatRecipientId>,
}

impl RequestEnvelope {
    /// The decoded body text.
    pub fn body(&self) -> &str {
        &self.body
    }

    /// The recipient this DTO named, if its route carries one.
    ///
    /// Returning it is safe precisely because the only consumer is the
    /// disagreement check in [`crate::authorization::authorize`], which can
    /// refuse on it and cannot select with it.
    pub fn recipient(&self) -> Option<&SeatRecipientId> {
        self.recipient.as_ref()
    }
}

/// Decode a request body, refusing oversize input **before** parsing it.
///
/// The order of the two steps is the law. Parsing first and measuring after
/// would mean a hostile payload is fully materialized before the refusal, which
/// is the resource exhaustion the bound exists to prevent.
pub fn decode(bound: MaxRequestDtoBytes, raw: &[u8]) -> Result<RequestEnvelope, ApiRefusal> {
    if raw.len() > bound.value() {
        return Err(ApiRefusal::DtoTooLarge {
            max: bound.value(),
            observed: Redacted::new(raw.len()),
        });
    }
    // Its own variant. R1 finding H3: this used to refuse with
    // `RecipientMalformed`, which rendered a sentence about seat recipients for
    // a body that simply was not UTF-8.
    let body = core::str::from_utf8(raw).map_err(|_| ApiRefusal::BodyNotUtf8)?;
    Ok(RequestEnvelope {
        body: body.to_owned(),
        recipient: None,
    })
}

/// Decode a body on a route that legitimately carries a recipient field.
///
/// The recipient text is validated through `SeatRecipientId::try_new`, so the
/// non-empty and 256-byte bounds are the owning type's rather than a second
/// copy of them here. A malformed value is refused naming the BOUND, never the
/// observed length.
pub fn decode_with_recipient(
    bound: MaxRequestDtoBytes,
    raw: &[u8],
    recipient_text: &str,
) -> Result<RequestEnvelope, ApiRefusal> {
    let envelope = decode(bound, raw)?;
    let recipient = SeatRecipientId::try_new(recipient_text)
        .map_err(|error| ApiRefusal::from_recipient_error(&error, recipient_text))?;
    Ok(RequestEnvelope {
        recipient: Some(recipient),
        ..envelope
    })
}
