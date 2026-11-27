//! The closed failure taxonomy.
//!
//! No failure is represented by a control-flow string. A bounded redacted
//! diagnostic may ACCOMPANY a variant, but it may never select one — that is
//! the whole difference between evidence and authority.
//!
//! # What ruling J2 removed
//!
//! The admission packet's taxonomy contained `LeaseConflict` and `LeaseRevoked`.
//! Both are gone. Under J2 an agent id owns its inbox exactly as it does today
//! and there are no leases or fences, so a second opener is not an error to be
//! refused — it is a TAKEOVER to be made visible. Keeping a conflict variant
//! would have preserved the fortress shape after the ruling cut it, and a
//! reviewer would rightly ask which door still produced it.

use crate::identity::{MailTransportClientInstanceId, SourcePrincipalId};
use crate::limits::{SettledTransportLimits, GRAPT_MAX_DIAGNOSTIC_BYTES};
use core::fmt;

/// A bounded, redacted diagnostic.
///
/// Truncation is explicit and recorded, unlike a message body, which is always
/// refused instead: a diagnostic is evidence for a human, so a shortened one is
/// still useful, whereas a shortened instruction is dangerous.
///
/// # Which bound applies, and why there are two constructors
///
/// A diagnostic can arise before a binding exists — an `open` that never got far
/// enough to settle anything still needs to say why. There is therefore no
/// single ambient bound, and a constructor that quietly picked one would be
/// wrong half the time. So the phase is named at the call site:
///
/// * [`Self::local`] — PRE-OPEN, no binding, no negotiated profile. Bounded by
///   [`GRAPT_MAX_DIAGNOSTIC_BYTES`], which is this workspace's own ceiling and
///   the only bound available;
/// * [`Self::within`] — POST-OPEN, bounded by the binding's SETTLED
///   `diagnostic_bytes`, which is the smaller of the provider's measurement and
///   the Grapt ceiling.
///
/// The old single `new` is deliberately gone. It read the Grapt ceiling
/// unconditionally, so a provider that advertised a SMALLER diagnostic bound was
/// negotiated with and then ignored — the settled value existed and nothing
/// consulted it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BoundedDiagnostic {
    text: String,
    truncated_from: Option<usize>,
}

impl BoundedDiagnostic {
    /// A diagnostic raised where no binding exists yet.
    pub fn local(raw: impl Into<String>) -> Self {
        Self::bound(raw.into(), GRAPT_MAX_DIAGNOSTIC_BYTES)
    }

    /// A diagnostic raised under an open binding, bounded by what that binding
    /// actually settled.
    pub fn within(raw: impl Into<String>, limits: &SettledTransportLimits) -> Self {
        Self::bound(raw.into(), limits.diagnostic_bytes())
    }

    /// Truncates on a UTF-8 character boundary at or below `max`.
    ///
    /// Walking DOWN from `max` is what keeps the result valid UTF-8: cutting at
    /// `max` itself can land mid-sequence, and a `String` cannot hold that.
    fn bound(raw: String, max: usize) -> Self {
        if raw.len() <= max {
            return BoundedDiagnostic {
                text: raw,
                truncated_from: None,
            };
        }
        let original = raw.len();
        let mut end = max;
        while end > 0 && !raw.is_char_boundary(end) {
            end -= 1;
        }
        let mut text = raw;
        text.truncate(end);
        BoundedDiagnostic {
            text,
            truncated_from: Some(original),
        }
    }

    pub fn as_str(&self) -> &str {
        &self.text
    }

    /// The original byte length when this diagnostic was shortened.
    pub fn truncated_from(&self) -> Option<usize> {
        self.truncated_from
    }
}

/// Why a binding could not be opened.
///
/// Every variant here is fail-closed for the mailbox path: none of them may be
/// retried into success by the transport itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenFailure {
    Unauthenticated,
    /// The source's capability floor changed mid-read and asks for a retry.
    /// Still closed for the transport; only the consumer's recovery may retry.
    UnauthenticatedRetry,
    ForeignProjectMismatch,
    ExactPrincipalMismatch,
    RecipientMappingMissing,
    RecipientMismatch,
    MappingVersionMismatch,
    ProtocolVersionUnsupported,
    RequiredCapabilityMissing {
        field: &'static str,
    },
    /// A mandatory negotiated limit was absent or zero.
    LimitProfileIncomplete {
        field: &'static str,
    },
}

/// Why a subscribe, fetch, decode or dismiss call failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TransportFailure {
    TransportUnavailable,
    Disconnected,
    SubscriptionRefused,
    SnapshotExpired,
    CursorInvalid(crate::cursor::CursorInvalidation),
    CursorRegressed,
    PageCountExceeded,
    PageBodyBytesExceeded,
    PageDecodedBytesExceeded,
    MessageBodyExceeded,
    EmptyBody,
    ReferenceCountExceeded,
    ReferenceBytesExceeded,
    CursorBytesExceeded,
    MalformedEnvelope,
    UnknownRequiredField { field: &'static str },
    UnknownKind,
    UnknownArchiveReason,
    SupersessionConflict,
    ProjectProvenanceMissing,
}

/// Why a delivery/admission acknowledgement failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeliveryAckFailure {
    /// The acknowledged message was never enumerated to this binding.
    MessageNeverEnumerated,
    AcknowledgementCoordinateMismatch,
    AcknowledgementDigestMismatch,
    AcknowledgementConflict,
    DeliveryAckUnsupported,
    /// The provider offered its READ acknowledgement in place of a delivery
    /// acknowledgement. Refused loudly, because silently accepting it is
    /// precisely the corruption this contract exists to prevent.
    ReadAckSubstitutionRefused,
}

/// Why a bounded lifecycle budget ended a slice.
///
/// These are not fail-closed blockers. They preserve durable debt and the
/// settled checkpoint and return control; they never claim idle, replay an
/// external effect, or terminate independent work.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifecycleOutcome {
    Cancelled,
    ConnectBudgetExhausted,
    ReconnectBudgetExhausted,
    SliceBudgetExhausted { catch_up_owed: bool },
    ShutdownBudgetExhausted { tasks_remaining: u32 },
    RecoveryUnavailable,
}

/// Why an armed subscription ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubscriptionClosed {
    /// The connection dropped. Harmless on its own: every reconnect performs a
    /// fresh subscribe-and-snapshot and catches up before declaring Live.
    Disconnected,
    /// The local owner asked for shutdown.
    LocalShutdown,
    /// ANOTHER owner took this recipient's inbox over.
    ///
    /// Ruling J2's replacement for lease revocation. It is not an error and not
    /// a refusal: the newer owner is legitimate, and this notice is the
    /// displaced side of the two-way receipt. It is deliberately loud so a
    /// silently-stopped seat cannot look like a quiet inbox.
    DisplacedByTakeover {
        by_principal: SourcePrincipalId,
        by_client: MailTransportClientInstanceId,
    },
}

impl fmt::Display for OpenFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OpenFailure::Unauthenticated => f.write_str("the transport binding is unauthenticated"),
            OpenFailure::UnauthenticatedRetry => {
                f.write_str("the transport binding is unauthenticated; the source asks for a retry")
            }
            OpenFailure::ForeignProjectMismatch => {
                f.write_str("the authenticated foreign project is not the expected one")
            }
            OpenFailure::ExactPrincipalMismatch => {
                f.write_str("the authenticated principal is not the expected one")
            }
            OpenFailure::RecipientMappingMissing => {
                f.write_str("no recipient mapping exists for the authenticated principal")
            }
            OpenFailure::RecipientMismatch => {
                f.write_str("the mapped recipient is not the expected local recipient")
            }
            OpenFailure::MappingVersionMismatch => {
                f.write_str("the recipient mapping version is not the expected one")
            }
            OpenFailure::ProtocolVersionUnsupported => {
                f.write_str("the provider does not support this protocol version")
            }
            OpenFailure::RequiredCapabilityMissing { field } => {
                write!(
                    f,
                    "the provider does not advertise required capability {field}"
                )
            }
            OpenFailure::LimitProfileIncomplete { field } => write!(
                f,
                "the negotiated limit profile is incomplete at {field}; \
                 a missing or zero bound refuses open"
            ),
        }
    }
}

impl std::error::Error for OpenFailure {}

impl fmt::Display for DeliveryAckFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DeliveryAckFailure::MessageNeverEnumerated => {
                f.write_str("the acknowledged message was never enumerated to this binding")
            }
            DeliveryAckFailure::AcknowledgementCoordinateMismatch => {
                f.write_str("an acknowledgement coordinate does not match the enumerated message")
            }
            DeliveryAckFailure::AcknowledgementDigestMismatch => {
                f.write_str("the canonical message digest does not match")
            }
            DeliveryAckFailure::AcknowledgementConflict => {
                f.write_str("a conflicting acknowledgement already exists for this message")
            }
            DeliveryAckFailure::DeliveryAckUnsupported => {
                f.write_str("the provider offers no distinct delivery acknowledgement")
            }
            DeliveryAckFailure::ReadAckSubstitutionRefused => f.write_str(
                "the provider offered a READ acknowledgement in place of a delivery \
                 acknowledgement; delivery is not attention and the substitution is refused",
            ),
        }
    }
}

impl std::error::Error for DeliveryAckFailure {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A settled profile whose diagnostic bound the caller chooses.
    fn settled_with_diagnostic(diagnostic: usize) -> SettledTransportLimits {
        crate::test_support::provider_with(65_536, 64, 262_144, diagnostic).settle()
    }

    #[test]
    fn a_local_diagnostic_truncates_on_a_character_boundary_and_records_the_original() {
        let wide = "\u{1F600}".repeat(GRAPT_MAX_DIAGNOSTIC_BYTES); // far over the bound
        let diagnostic = BoundedDiagnostic::local(wide.clone());
        assert!(diagnostic.as_str().len() <= GRAPT_MAX_DIAGNOSTIC_BYTES);
        // Never split a character: the result must still be valid UTF-8 text.
        assert!(diagnostic.as_str().chars().all(|c| c == '\u{1F600}'));
        assert_eq!(diagnostic.truncated_from(), Some(wide.len()));
    }

    #[test]
    fn a_short_diagnostic_is_not_marked_truncated() {
        let diagnostic = BoundedDiagnostic::local("disconnected mid-page");
        assert_eq!(diagnostic.as_str(), "disconnected mid-page");
        assert_eq!(diagnostic.truncated_from(), None);
    }

    #[test]
    fn a_post_open_diagnostic_obeys_the_smaller_provider_bound() {
        // Advisory B, the defect's own shape: this text fits the Grapt ceiling
        // comfortably, so the OLD unconditional constructor kept all of it. Under
        // a binding that settled 64 bytes, keeping it would hand the provider a
        // diagnostic it cannot carry.
        let limits = settled_with_diagnostic(64);
        assert_eq!(limits.diagnostic_bytes(), 64);
        let raw = "d".repeat(200);
        let diagnostic = BoundedDiagnostic::within(raw.clone(), &limits);
        assert_eq!(diagnostic.as_str().len(), 64);
        assert_eq!(diagnostic.truncated_from(), Some(200));
        // The pre-open route is the contrast, and is NOT wrong — it simply has no
        // binding to consult, which is exactly why the two are separate.
        assert_eq!(BoundedDiagnostic::local(raw).as_str().len(), 200);
    }

    #[test]
    fn a_post_open_diagnostic_obeys_the_se_code_ceiling_when_it_is_smaller() {
        // The provider would allow a megabyte; this workspace still will not
        // retain more than its own ceiling.
        let limits = settled_with_diagnostic(1_048_576);
        assert_eq!(limits.diagnostic_bytes(), GRAPT_MAX_DIAGNOSTIC_BYTES);
        let raw = "d".repeat(GRAPT_MAX_DIAGNOSTIC_BYTES * 2);
        let diagnostic = BoundedDiagnostic::within(raw, &limits);
        assert_eq!(diagnostic.as_str().len(), GRAPT_MAX_DIAGNOSTIC_BYTES);
    }

    #[test]
    fn a_settled_bound_that_splits_a_character_truncates_below_it() {
        // The UTF-8 boundary case at a NEGOTIATED bound rather than the ceiling.
        // Each emoji is 4 bytes, so a 10-byte bound lands mid-character: the
        // result must be 8 bytes, never 10, and never invalid UTF-8.
        let limits = settled_with_diagnostic(10);
        let diagnostic = BoundedDiagnostic::within("\u{1F600}".repeat(4), &limits);
        assert_eq!(diagnostic.as_str().len(), 8);
        assert_eq!(diagnostic.as_str().chars().count(), 2);
        assert_eq!(diagnostic.truncated_from(), Some(16));
    }

    #[test]
    fn the_taxonomy_has_no_lease_variant_left_after_j2() {
        // A guard against re-growing the fortress shape by habit. The closed
        // sets below are matched exhaustively, so a re-added lease variant
        // fails this test to compile rather than sliding back in as data.
        fn open_is_closed(failure: &OpenFailure) -> &'static str {
            match failure {
                OpenFailure::Unauthenticated => "unauthenticated",
                OpenFailure::UnauthenticatedRetry => "unauthenticated-retry",
                OpenFailure::ForeignProjectMismatch => "foreign-project-mismatch",
                OpenFailure::ExactPrincipalMismatch => "exact-principal-mismatch",
                OpenFailure::RecipientMappingMissing => "recipient-mapping-missing",
                OpenFailure::RecipientMismatch => "recipient-mismatch",
                OpenFailure::MappingVersionMismatch => "mapping-version-mismatch",
                OpenFailure::ProtocolVersionUnsupported => "protocol-version-unsupported",
                OpenFailure::RequiredCapabilityMissing { .. } => "required-capability-missing",
                OpenFailure::LimitProfileIncomplete { .. } => "limit-profile-incomplete",
            }
        }
        assert_eq!(
            open_is_closed(&OpenFailure::Unauthenticated),
            "unauthenticated"
        );
    }

    #[test]
    fn a_read_ack_substitution_is_refused_with_its_own_reason() {
        // The named resurrection: accepting msg_ack as a delivery receipt.
        let failure = DeliveryAckFailure::ReadAckSubstitutionRefused;
        assert_ne!(failure, DeliveryAckFailure::DeliveryAckUnsupported);
        assert!(failure.to_string().contains("delivery is not attention"));
    }

    #[test]
    fn a_takeover_closes_a_subscription_loudly_and_names_the_new_owner(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let closed = SubscriptionClosed::DisplacedByTakeover {
            by_principal: SourcePrincipalId::new("agt-other").ok_or("principal")?,
            by_client: MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
        };
        // Distinct from an ordinary disconnect: a displaced owner must not
        // reconnect in a loop as though the link merely dropped.
        assert_ne!(closed, SubscriptionClosed::Disconnected);
        assert_ne!(closed, SubscriptionClosed::LocalShutdown);
        Ok(())
    }
}
