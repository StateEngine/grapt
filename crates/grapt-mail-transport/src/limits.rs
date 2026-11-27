//! Negotiated transport bounds.
//!
//! Ruling J6: every count, UTF-8 byte, page, token, diagnostic, in-flight,
//! reconnect and shutdown limit is positive, exact, receipt-bound, measured and
//! fail-closed. The values below that carry a literal are SE CODE CEILINGS, not
//! provider minima — the reconciliation addendum's amendment 5 is explicit that
//! a provider may advertise a smaller measured profile, and the settled value is
//! the SMALLER of the two.
//!
//! # The inventory, counted mechanically
//!
//! [`ProviderLimitsParts`] declares **24** mandatory fields. Of those, **four**
//! are ceiling-settled here — body bytes, messages per page, aggregate body
//! bytes per page, and diagnostic bytes — and the remaining **20** are
//! provider-owned residual: this crate records them, binds them to the receipt,
//! and consults none of them, because the consumers that would are R3b-2B and
//! R3b-2C.
//!
//! An earlier revision of this module said "twenty values" and a review counted
//! 24 against the struct body. The count is now stated once, next to the struct
//! it describes, and a test counts the ceiling-settled side against the settling
//! code rather than against this sentence.
//!
//! # Why the residual fields carry no literal here
//!
//! Seat 5D measured StateEngine at `bb466976` and found character caps (8,000
//! JavaScript characters per body, 512 per ref) and an MCP framing bound — none
//! of which is a UTF-8 byte bound on a decoded message or page. Adopting one as
//! if it were would be exactly the "character caps are not byte caps" defect the
//! addendum records. They stay provider-owned until the ratified provider
//! contract measures them.
//!
//! # Absence and zero cannot reach this module
//!
//! A previous revision claimed absence "is modelled by [`Option`] at the
//! negotiation edge, in [`ProviderLimits::settle`]". That was false in both
//! halves: `settle` has no `Option` and no failure mode, and the `LimitError`
//! that named the refusal had no producer anywhere in the workspace — a kind
//! without a producer is not a boundary.
//!
//! What actually holds is structural. Every field of [`ProviderLimitsParts`] is
//! a `NonZero` type, so a zero bound cannot be written down; the struct has no
//! `Default` and no `Option`, so a partial profile cannot be completed by
//! omission. A [`ProviderLimits`] therefore CANNOT exist unless all 24 values
//! are present and positive, which is why `settle` is infallible rather than
//! optimistic.
//!
//! Missing or zero input is a DECODE concern one layer earlier: a future
//! crate-owned adapter reading a provider DTO maps it to
//! [`crate::failure::OpenFailure::LimitProfileIncomplete`] naming the offending
//! field, and that refusal happens BEFORE a `ProviderLimits` exists.

use core::num::{NonZeroU32, NonZeroUsize};

/// The largest single UTF-8 body this workspace will ever accept.
///
/// Deliberately equal to `grapt_types::mailbox::MAX_MAILBOX_BODY_BYTES`, and
/// asserted equal in this module's tests: a transport that accepted a body the
/// durable vocabulary refuses could fetch mail that can never be admitted, which
/// is a stall disguised as progress.
pub const GRAPT_MAX_BODY_BYTES: usize = 65_536;

/// The largest number of whole messages one page may emit.
pub const GRAPT_MAX_MESSAGES_PER_PAGE: u32 = 64;

/// The largest aggregate decoded body bytes one page may emit.
pub const GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE: usize = 262_144;

/// The largest bounded diagnostic this workspace will retain, in UTF-8 bytes.
///
/// A ceiling like the others, and settled by `min` against the provider's own
/// `max_diagnostic_bytes`: a provider that can only carry a shorter diagnostic
/// must not be handed a longer one, and this workspace will not retain a longer
/// one even if the provider would allow it.
///
/// This is the ONLY definition of the diagnostic ceiling. The failure taxonomy
/// used to carry a second `MAX_DIAGNOSTIC_BYTES` literal of its own; two names
/// for one law is a redundant carrier that can drift, and an equality test
/// between them would have been self-derived once either was defined from the
/// other. [`crate::failure::BoundedDiagnostic::local`] reads this constant
/// directly for the PRE-OPEN case, where no binding and therefore no negotiated
/// bound exists yet.
pub const GRAPT_MAX_DIAGNOSTIC_BYTES: usize = 4_096;

/// An advisory change signal carries no mail. Zero is the contract, not a
/// default: a signal that could carry a message would become a second delivery
/// path with no durability behind it.
pub const GRAPT_MAX_ADVISORY_SIGNAL_PAYLOAD_BYTES: usize = 0;

/// Local pending signals coalesce into exactly one bit, never a queue.
pub const GRAPT_MAX_COALESCED_PENDING_SIGNALS: u32 = 1;

/// The 24 values the provider must measure and advertise, all at once.
///
/// This is the ONLY way to obtain a [`ProviderLimits`]. It is a plain struct
/// with public fields and deliberately no `Default` and no `#[non_exhaustive]`,
/// so an external caller can build one — the port takes a profile from its
/// caller — but only by writing every field down. Rust's struct-literal rule is
/// the completeness check: omit one field and the code does not compile.
///
/// Every field is `NonZero`, so "present but zero" has no representation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderLimitsParts {
    pub max_refs_per_message: NonZeroU32,
    pub max_ref_utf8_bytes: NonZeroUsize,
    pub max_aggregate_ref_bytes_per_message: NonZeroUsize,
    pub max_decoded_message_bytes_including_metadata: NonZeroUsize,
    pub max_decoded_page_bytes_including_metadata: NonZeroUsize,
    pub max_cursor_bytes: NonZeroUsize,
    pub max_snapshot_token_bytes: NonZeroUsize,
    pub max_pages_per_runtime_slice: NonZeroU32,
    pub max_messages_per_runtime_slice: NonZeroU32,
    pub max_decoded_bytes_per_runtime_slice: NonZeroUsize,
    pub max_ack_debt_items_per_recovery_page: NonZeroU32,
    pub max_connect_attempts_per_activation: NonZeroU32,
    pub max_reconnect_attempts_per_runtime_slice: NonZeroU32,
    pub max_connect_duration_ticks: NonZeroU32,
    pub max_single_reconnect_delay_ticks: NonZeroU32,
    pub max_total_reconnect_delay_ticks_per_slice: NonZeroU32,
    pub max_fetch_duration_ticks: NonZeroU32,
    pub max_ack_duration_ticks: NonZeroU32,
    pub max_shutdown_duration_ticks: NonZeroU32,
    pub max_joined_transport_tasks_at_shutdown: NonZeroU32,
    /// The provider's own diagnostic cap, settled against
    /// [`GRAPT_MAX_DIAGNOSTIC_BYTES`].
    pub max_diagnostic_bytes: NonZeroUsize,
    /// The provider's own single-body cap, settled against
    /// [`GRAPT_MAX_BODY_BYTES`].
    pub max_body_utf8_bytes: NonZeroUsize,
    /// The provider's own page-count cap, settled against
    /// [`GRAPT_MAX_MESSAGES_PER_PAGE`].
    pub max_messages_per_page: NonZeroU32,
    /// The provider's own aggregate-body cap, settled against
    /// [`GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE`].
    pub max_aggregate_body_bytes_per_page: NonZeroUsize,
}

/// A complete, positive provider profile.
///
/// The value fields are PRIVATE. Construction goes through
/// [`ProviderLimits::new`] and therefore through [`ProviderLimitsParts`], so
/// there is no struct-literal route that could grow an `Option`, a sentinel or a
/// defaulted field later without the compiler naming every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderLimits {
    parts: ProviderLimitsParts,
}

impl ProviderLimits {
    /// The only constructor. Infallible BECAUSE the parts are already complete
    /// and positive by type.
    pub fn new(parts: ProviderLimitsParts) -> Self {
        ProviderLimits { parts }
    }

    pub fn max_refs_per_message(&self) -> NonZeroU32 {
        self.parts.max_refs_per_message
    }

    pub fn max_ref_utf8_bytes(&self) -> NonZeroUsize {
        self.parts.max_ref_utf8_bytes
    }

    pub fn max_aggregate_ref_bytes_per_message(&self) -> NonZeroUsize {
        self.parts.max_aggregate_ref_bytes_per_message
    }

    pub fn max_decoded_message_bytes_including_metadata(&self) -> NonZeroUsize {
        self.parts.max_decoded_message_bytes_including_metadata
    }

    pub fn max_decoded_page_bytes_including_metadata(&self) -> NonZeroUsize {
        self.parts.max_decoded_page_bytes_including_metadata
    }

    pub fn max_cursor_bytes(&self) -> NonZeroUsize {
        self.parts.max_cursor_bytes
    }

    pub fn max_snapshot_token_bytes(&self) -> NonZeroUsize {
        self.parts.max_snapshot_token_bytes
    }

    pub fn max_pages_per_runtime_slice(&self) -> NonZeroU32 {
        self.parts.max_pages_per_runtime_slice
    }

    pub fn max_messages_per_runtime_slice(&self) -> NonZeroU32 {
        self.parts.max_messages_per_runtime_slice
    }

    pub fn max_decoded_bytes_per_runtime_slice(&self) -> NonZeroUsize {
        self.parts.max_decoded_bytes_per_runtime_slice
    }

    pub fn max_ack_debt_items_per_recovery_page(&self) -> NonZeroU32 {
        self.parts.max_ack_debt_items_per_recovery_page
    }

    pub fn max_connect_attempts_per_activation(&self) -> NonZeroU32 {
        self.parts.max_connect_attempts_per_activation
    }

    pub fn max_reconnect_attempts_per_runtime_slice(&self) -> NonZeroU32 {
        self.parts.max_reconnect_attempts_per_runtime_slice
    }

    pub fn max_connect_duration_ticks(&self) -> NonZeroU32 {
        self.parts.max_connect_duration_ticks
    }

    pub fn max_single_reconnect_delay_ticks(&self) -> NonZeroU32 {
        self.parts.max_single_reconnect_delay_ticks
    }

    pub fn max_total_reconnect_delay_ticks_per_slice(&self) -> NonZeroU32 {
        self.parts.max_total_reconnect_delay_ticks_per_slice
    }

    pub fn max_fetch_duration_ticks(&self) -> NonZeroU32 {
        self.parts.max_fetch_duration_ticks
    }

    pub fn max_ack_duration_ticks(&self) -> NonZeroU32 {
        self.parts.max_ack_duration_ticks
    }

    pub fn max_shutdown_duration_ticks(&self) -> NonZeroU32 {
        self.parts.max_shutdown_duration_ticks
    }

    pub fn max_joined_transport_tasks_at_shutdown(&self) -> NonZeroU32 {
        self.parts.max_joined_transport_tasks_at_shutdown
    }

    pub fn max_diagnostic_bytes(&self) -> NonZeroUsize {
        self.parts.max_diagnostic_bytes
    }

    pub fn max_body_utf8_bytes(&self) -> NonZeroUsize {
        self.parts.max_body_utf8_bytes
    }

    pub fn max_messages_per_page(&self) -> NonZeroU32 {
        self.parts.max_messages_per_page
    }

    pub fn max_aggregate_body_bytes_per_page(&self) -> NonZeroUsize {
        self.parts.max_aggregate_body_bytes_per_page
    }

    /// Settles this profile against the Grapt ceilings.
    ///
    /// Each capped field takes the SMALLER of the provider measurement and the
    /// local ceiling. There is no path that raises a bound above either side.
    pub fn settle(self) -> SettledTransportLimits {
        let body = self.parts.max_body_utf8_bytes.get();
        let msgs = self.parts.max_messages_per_page.get();
        let agg = self.parts.max_aggregate_body_bytes_per_page.get();
        let diag = self.parts.max_diagnostic_bytes.get();
        SettledTransportLimits {
            provider: self,
            body_utf8_bytes: body.min(GRAPT_MAX_BODY_BYTES),
            messages_per_page: msgs.min(GRAPT_MAX_MESSAGES_PER_PAGE),
            aggregate_body_bytes_per_page: agg.min(GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE),
            diagnostic_bytes: diag.min(GRAPT_MAX_DIAGNOSTIC_BYTES),
            clamped_fields: ClampedFields {
                body_utf8_bytes: body > GRAPT_MAX_BODY_BYTES,
                messages_per_page: msgs > GRAPT_MAX_MESSAGES_PER_PAGE,
                aggregate_body_bytes_per_page: agg > GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE,
                diagnostic_bytes: diag > GRAPT_MAX_DIAGNOSTIC_BYTES,
            },
        }
    }
}

/// The immutable settled profile for one binding.
///
/// Recorded in the binding receipt and fixed for that binding's lifetime: a
/// limit that could change under a live subscription would make an in-flight
/// page's bound unprovable.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SettledTransportLimits {
    provider: ProviderLimits,
    body_utf8_bytes: usize,
    messages_per_page: u32,
    aggregate_body_bytes_per_page: usize,
    diagnostic_bytes: usize,
    clamped_fields: ClampedFields,
}

/// Which fields the Grapt ceiling, rather than the provider, decided.
///
/// Kept as evidence: a receipt that did not say which side won could not later
/// explain why a page stopped where it did.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[expect(clippy::struct_excessive_bools, reason = "legacy")]
pub struct ClampedFields {
    pub body_utf8_bytes: bool,
    pub messages_per_page: bool,
    pub aggregate_body_bytes_per_page: bool,
    pub diagnostic_bytes: bool,
}

impl SettledTransportLimits {
    pub fn body_utf8_bytes(&self) -> usize {
        self.body_utf8_bytes
    }

    pub fn messages_per_page(&self) -> u32 {
        self.messages_per_page
    }

    pub fn aggregate_body_bytes_per_page(&self) -> usize {
        self.aggregate_body_bytes_per_page
    }

    /// The settled diagnostic bound for this binding.
    ///
    /// Post-open diagnostics are bounded by THIS, not by the pre-open ceiling —
    /// see [`crate::failure::BoundedDiagnostic::within`].
    pub fn diagnostic_bytes(&self) -> usize {
        self.diagnostic_bytes
    }

    pub fn clamped_fields(&self) -> ClampedFields {
        self.clamped_fields
    }

    pub fn provider(&self) -> &ProviderLimits {
        &self.provider
    }

    /// The advisory payload bound. Not negotiable: zero on both sides.
    pub const fn advisory_signal_payload_bytes(&self) -> usize {
        GRAPT_MAX_ADVISORY_SIGNAL_PAYLOAD_BYTES
    }

    /// The coalesced pending-signal bound. Not negotiable: one bit.
    pub const fn coalesced_pending_signals(&self) -> u32 {
        GRAPT_MAX_COALESCED_PENDING_SIGNALS
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::provider_with;
    use grapt_types::mailbox::MAX_MAILBOX_BODY_BYTES;

    fn provider(body: usize, msgs: u32, agg: usize) -> ProviderLimits {
        provider_with(body, msgs, agg, GRAPT_MAX_DIAGNOSTIC_BYTES)
    }

    #[test]
    fn the_body_ceiling_matches_the_durable_vocabulary_exactly() {
        // If these ever diverge, the transport could fetch a body the durable
        // mailbox refuses: mail that arrives and can never be admitted.
        assert_eq!(GRAPT_MAX_BODY_BYTES, MAX_MAILBOX_BODY_BYTES);
    }

    #[test]
    fn a_larger_provider_profile_is_clamped_to_the_se_code_ceiling() {
        let settled = provider_with(1_000_000, 4_096, 8_000_000, 1_000_000).settle();
        assert_eq!(settled.body_utf8_bytes(), GRAPT_MAX_BODY_BYTES);
        assert_eq!(settled.messages_per_page(), GRAPT_MAX_MESSAGES_PER_PAGE);
        assert_eq!(
            settled.aggregate_body_bytes_per_page(),
            GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE
        );
        assert_eq!(settled.diagnostic_bytes(), GRAPT_MAX_DIAGNOSTIC_BYTES);
        let clamped = settled.clamped_fields();
        assert!(clamped.body_utf8_bytes);
        assert!(clamped.messages_per_page);
        assert!(clamped.aggregate_body_bytes_per_page);
        assert!(clamped.diagnostic_bytes);
    }

    #[test]
    fn a_smaller_provider_profile_wins_because_ceilings_are_not_minima() {
        // Addendum amendment 5: Grapt numbers are ceilings, and a provider may
        // legitimately expose a smaller measured profile.
        let settled = provider_with(4_096, 8, 16_384, 512).settle();
        assert_eq!(settled.body_utf8_bytes(), 4_096);
        assert_eq!(settled.messages_per_page(), 8);
        assert_eq!(settled.aggregate_body_bytes_per_page(), 16_384);
        assert_eq!(settled.diagnostic_bytes(), 512);
        assert_eq!(settled.clamped_fields(), ClampedFields::default());
    }

    #[test]
    fn a_smaller_provider_diagnostic_bound_wins_over_the_se_code_ceiling() {
        // Advisory B's provider-smaller case, stated on its own so a mutant that
        // reverses the diagnostic `min` cannot hide inside the body/page arms.
        let settled = provider_with(65_536, 64, 262_144, 64).settle();
        assert_eq!(settled.diagnostic_bytes(), 64);
        assert!(
            settled.diagnostic_bytes() < GRAPT_MAX_DIAGNOSTIC_BYTES,
            "a provider that can only carry 64 bytes must not be handed 4,096"
        );
        assert!(!settled.clamped_fields().diagnostic_bytes);
    }

    #[test]
    fn an_se_code_smaller_diagnostic_bound_wins_over_the_provider() {
        // Advisory B's SE-smaller case: the provider would allow far more, and
        // this workspace still refuses to retain it.
        let settled = provider_with(65_536, 64, 262_144, 1_048_576).settle();
        assert_eq!(settled.diagnostic_bytes(), GRAPT_MAX_DIAGNOSTIC_BYTES);
        assert!(settled.clamped_fields().diagnostic_bytes);
    }

    #[test]
    fn settling_never_raises_a_bound_above_either_side() {
        for (body, msgs, agg, diag) in [
            (1, 1, 1, 1),
            (
                GRAPT_MAX_BODY_BYTES,
                GRAPT_MAX_MESSAGES_PER_PAGE,
                1,
                GRAPT_MAX_DIAGNOSTIC_BYTES,
            ),
            (usize::MAX, u32::MAX, usize::MAX, usize::MAX),
        ] {
            let settled = provider_with(body, msgs, agg, diag).settle();
            assert!(settled.body_utf8_bytes() <= body);
            assert!(settled.body_utf8_bytes() <= GRAPT_MAX_BODY_BYTES);
            assert!(settled.messages_per_page() <= msgs);
            assert!(settled.messages_per_page() <= GRAPT_MAX_MESSAGES_PER_PAGE);
            assert!(settled.aggregate_body_bytes_per_page() <= agg);
            assert!(
                settled.aggregate_body_bytes_per_page() <= GRAPT_MAX_AGGREGATE_BODY_BYTES_PER_PAGE
            );
            assert!(settled.diagnostic_bytes() <= diag);
            assert!(settled.diagnostic_bytes() <= GRAPT_MAX_DIAGNOSTIC_BYTES);
        }
    }

    #[test]
    fn the_settled_profile_carries_every_provider_value_unchanged() {
        // The 20 residual fields are RECORDED even though nothing here consults
        // them. If a future edit dropped one, its consumer in R3b-2B would find a
        // hole rather than a bound.
        let limits = provider(65_536, 64, 262_144);
        let settled = limits.settle();
        assert_eq!(settled.provider(), &limits);
        assert_eq!(settled.provider().max_cursor_bytes().get(), 1_024);
        assert_eq!(settled.provider().max_connect_duration_ticks().get(), 300);
    }

    #[test]
    fn exactly_four_fields_are_ceiling_settled_and_the_rest_are_residual() {
        // Advisory A: the inventory is 24 = 4 settled + 20 residual. This counts
        // the settled side against the SETTLING CODE — a field that stopped being
        // clamped would change `ClampedFields` and fail here — rather than
        // against a sentence in the module doc.
        let clamped = provider_with(usize::MAX, u32::MAX, usize::MAX, usize::MAX)
            .settle()
            .clamped_fields();
        let settled_count = [
            clamped.body_utf8_bytes,
            clamped.messages_per_page,
            clamped.aggregate_body_bytes_per_page,
            clamped.diagnostic_bytes,
        ]
        .iter()
        .filter(|flag| **flag)
        .count();
        assert_eq!(settled_count, 4, "four fields are ceiling-settled");
        assert_eq!(24 - settled_count, 20, "twenty remain provider-owned");
    }

    #[test]
    fn the_advisory_signal_carries_no_mail_and_one_pending_bit() {
        let settled = provider(4_096, 8, 16_384).settle();
        assert_eq!(settled.advisory_signal_payload_bytes(), 0);
        assert_eq!(settled.coalesced_pending_signals(), 1);
    }
}
