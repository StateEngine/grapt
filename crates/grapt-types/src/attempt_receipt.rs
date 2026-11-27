//! The operational attempt receipt — the crash-visible half of process-loss
//! reconciliation.
//!
//! Law: `SECODE_RUST_PROVIDER_STREAM_RUNTIME_CONTRACT_2026-08-08.md`
//! §"Cancellation ownership" — "If the process dies, Rust does not pretend
//! `Drop` completed this work. R2 writes an operational attempt receipt before
//! the external call. A receipt with `request_sent` and no terminal becomes
//! `AmbiguousProcessLoss`; it is sufficient for diagnosis and later
//! reconciliation but does not authorize automatic replay or mutate the Session
//! transcript by itself." And §"Initial rulings" — the receipt is
//! "an operational `attempt_receipt`, **not automatically in the minimal
//! Session event log**".
//!
//! The types-only boundary follows the preflight
//! `C:\fixtures\R2C5_PROCESS_LOSS_RECONCILIATION_PREFLIGHT_2026-08-13.md` (SHA-256
//! `d3187f787c4a51ec1148d438bd2c4ad8df9e626c540eb4da105cfd91189bf31d`).
//!
//! # What this module is, and is not
//!
//! It is the **vocabulary and the classification law**: the three operational
//! markers a receipt can carry, and the total function from which markers are
//! present to what a reconciler must record. It performs no I/O, holds no
//! store handle, and decides nothing about *when* a receipt is written.
//!
//! It is **not** the store table (A1), the write path (A1), the startup
//! reconciler (B), or any replay policy (R3 owns continuation). Nothing here
//! queries a provider, and nothing here infers status at read time — the
//! disposition below is what a reconciler **writes**, not a view it computes
//! for display. Survey row D6: "Do not infer permanent status only at read
//! time."
//!
//! # Why the markers are a structure and not a status field
//!
//! A single `status` column would have to be updated at each transition, and a
//! crash between "external call made" and "status updated" is exactly the
//! window this lane exists to describe. Three append-only markers cannot lose
//! that window: whichever ones survive the crash *are* the evidence, and the
//! classification is derived from their presence rather than from a value
//! somebody had to remember to write.

use crate::attempt::{AttemptOrdinal, PhysicalAttemptId, StepNumber};
use crate::authority::Timestamp;
use crate::operation::AmbiguityEvidence;
use crate::session::SessionId;
use crate::terminality::ExternalEffectState;

/// The mandatory first marker: this attempt exists and is about to be run.
///
/// Written at the pre-I/O seam, where the Step identity, the injected clock/id
/// authority and a fencing token are all in hand synchronously — before any
/// socket exists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReservedMarker {
    at: Timestamp,
}

impl ReservedMarker {
    pub fn new(at: Timestamp) -> Self {
        Self { at }
    }

    pub fn at(&self) -> Timestamp {
        self.at
    }
}

/// The marker that changes everything: bytes have left this process.
///
/// Its presence is the entire difference between "safe to replay" and "never
/// replay automatically", which is why it is a distinct append-only marker and
/// not a boolean inside the reserved one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestSentMarker {
    at: Timestamp,
}

impl RequestSentMarker {
    pub fn new(at: Timestamp) -> Self {
        Self { at }
    }

    pub fn at(&self) -> Timestamp {
        self.at
    }
}

/// The attempt reached a durable terminal, so there is nothing to reconcile.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettledMarker {
    at: Timestamp,
}

impl SettledMarker {
    pub fn new(at: Timestamp) -> Self {
        Self { at }
    }

    pub fn at(&self) -> Timestamp {
        self.at
    }
}

/// What a reconciler must do with one receipt.
///
/// Closed, and deliberately two-armed: either the attempt already has its
/// terminal, or it is lost and the effect is exactly determined by which
/// markers survived. There is no "unknown" arm, because an unknown would be the
/// read-time inference survey row D6 forbids.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReceiptDisposition {
    /// The attempt settled. A reconciler must not write a second terminal.
    AlreadySettled,
    /// The attempt did not settle, and this is what was lost.
    Lost(LostEffect),
}

/// What a lost attempt did to the outside world, decided structurally.
///
/// # Why this is not `ExternalEffectState` directly
///
/// The `Ambiguous` arm of the landed vocabulary carries `AmbiguityEvidence`,
/// and evidence "must say what was observed" — `AmbiguityEvidence::new` refuses
/// an empty string for exactly that reason. A receipt has not observed
/// anything: it is a marker structure. If this type manufactured its own
/// evidence it would be asserting an observation nobody made, which is the
/// fabrication the law forbids. So the receipt reports the SHAPE of the loss
/// and the reconciler — which does observe, at startup, under an elected
/// epoch — supplies the evidence when it projects into the landed vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LostEffect {
    /// Nothing left the process. Projects to
    /// [`ExternalEffectState::NoneInitiated`], which is replayable.
    NoneInitiated,
    /// Bytes left and nothing settled. Projects to
    /// [`ExternalEffectState::Ambiguous`], which is never replayable
    /// automatically.
    Ambiguous { sent_at: Timestamp },
}

impl LostEffect {
    /// Project into the landed vocabulary.
    ///
    /// The evidence closure is called **only** for the ambiguous arm, so a
    /// caller reconciling a never-sent attempt is not forced to invent an
    /// observation in order to describe an absence.
    pub fn into_effect_state(
        self,
        evidence: impl FnOnce(Timestamp) -> AmbiguityEvidence,
    ) -> ExternalEffectState {
        match self {
            LostEffect::NoneInitiated => ExternalEffectState::NoneInitiated,
            LostEffect::Ambiguous { sent_at } => ExternalEffectState::Ambiguous {
                evidence: evidence(sent_at),
            },
        }
    }

    /// Whether the lost work may be re-run, without needing evidence to ask.
    ///
    /// Agrees with `ExternalEffectState::safe_to_replay` by construction — the
    /// witness `the_replay_rule_agrees_with_the_landed_vocabulary` pins that,
    /// so this convenience can never drift into a second, softer rule.
    pub fn safe_to_replay(self) -> bool {
        matches!(self, LostEffect::NoneInitiated)
    }
}

/// What applying a marker did.
///
/// Closed and three-armed on purpose (O1). A2 and B persist the same law, so a
/// caller can tell an idempotent retry from a real disagreement without
/// comparing markers by hand — and `Conflict` carries both instants so the
/// disagreement can be reported rather than merely detected.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarkerOutcome {
    /// The marker was absent and is now recorded.
    Recorded,
    /// The identical marker was already present. Idempotent, and normal after a
    /// retried write.
    ExactRetry,
    /// A DIFFERENT marker was already present. The held one is preserved; the
    /// offered one is reported, never silently collapsed into it.
    Conflict { held: Timestamp, offered: Timestamp },
}

impl MarkerOutcome {
    /// Whether the receipt changed. `ExactRetry` and `Conflict` both leave it
    /// exactly as it was.
    pub fn changed(self) -> bool {
        matches!(self, MarkerOutcome::Recorded)
    }

    /// Whether two different observations disagree about the same marker.
    pub fn is_conflict(self) -> bool {
        matches!(self, MarkerOutcome::Conflict { .. })
    }
}

/// An identity that exceeded the maximum its holder was constructed with.
///
/// Carries both numbers so a refusal can be reported rather than merely
/// detected, and names the identity kind so two refusals are not confused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdTooLong {
    pub kind: BoundedIdKind,
    pub bytes: usize,
    pub maximum: u32,
}

/// Which identity a [`IdTooLong`] refusal was about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundedIdKind {
    Attempt,
    Session,
}

/// A `PhysicalAttemptId` proven to fit an injected byte maximum.
///
/// # Why this exists (R2 finding Z2)
///
/// `ReconciliationPage` and `ReconciliationCeiling` bound **rows**, and a row
/// count cannot bound memory while an identity is unbounded: `page × row` is
/// unbounded at any page value. The bound that actually bounds memory is a byte
/// bound on the variable-length fields, which for this receipt are exactly the
/// two identities — every other field is fixed width.
///
/// # Why it owns the validated identity, never a `String`
///
/// A `String`-owning wrapper would be a **second identity**: it could hold a
/// value `PhysicalAttemptId` never validated, and the receipt would then carry
/// something that is not the id. This type owns the already-validated identity
/// and adds a bound and nothing else — `id()` returns it unchanged, byte for
/// byte. Compile-fail probe `52` pins that the `String` form does not exist.
///
/// # The maximum is injected, and this crate chooses none
///
/// No production `PhysicalAttemptId` mint or prefix is ruled, so no number is
/// baked here: no `Default`, no constant, no chosen production value. The
/// caller supplies a `NonZeroU32` and the ruling that fixes it supplies the
/// value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedAttemptId {
    id: PhysicalAttemptId,
}

impl BoundedAttemptId {
    /// Admit an attempt identity under `maximum` **UTF-8 bytes**.
    ///
    /// Bytes, not characters: `str::len` is the UTF-8 byte length, and a
    /// multi-byte identity must be measured as it is stored, not as it renders.
    pub fn new(id: PhysicalAttemptId, maximum: std::num::NonZeroU32) -> Result<Self, IdTooLong> {
        let bytes = id.as_str().len();
        if bytes > maximum.get() as usize {
            return Err(IdTooLong {
                kind: BoundedIdKind::Attempt,
                bytes,
                maximum: maximum.get(),
            });
        }
        Ok(Self { id })
    }

    /// The identity, unchanged. The wrapper adds a bound, not a value.
    pub fn id(&self) -> &PhysicalAttemptId {
        &self.id
    }

    pub fn byte_len(&self) -> usize {
        self.id.as_str().len()
    }
}

/// A `SessionId` proven to fit an injected byte maximum.
///
/// Same law as [`BoundedAttemptId`]. `SessionId`'s accepted parser admits only
/// ASCII alphanumerics and `_` after its prefix, so its byte length equals its
/// character count today — but the bound is stated and measured in **bytes**
/// regardless, because a parser is a current fact and the storage unit is not.
///
/// This wrapper is **not** retroactive and cannot be: nothing historical
/// constructs one, so it can never refuse an import, a decode or a replay. Its
/// only consequence is availability — a session whose identity exceeds the
/// maximum obtains no receipt, and therefore its attempts refuse before any
/// external call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoundedSessionId {
    id: SessionId,
}

impl BoundedSessionId {
    pub fn new(id: SessionId, maximum: std::num::NonZeroU32) -> Result<Self, IdTooLong> {
        let bytes = id.as_str().len();
        if bytes > maximum.get() as usize {
            return Err(IdTooLong {
                kind: BoundedIdKind::Session,
                bytes,
                maximum: maximum.get(),
            });
        }
        Ok(Self { id })
    }

    pub fn id(&self) -> &SessionId {
        &self.id
    }

    pub fn byte_len(&self) -> usize {
        self.id.as_str().len()
    }
}

/// One operational receipt for one PhysicalAttempt.
///
/// # Construction order is the law
///
/// A receipt **cannot** exist without its reserved marker: `new` takes it by
/// value, and the sent/settled markers are added by consuming transitions. So
/// "request sent for an attempt that was never reserved" is not a state this
/// type can hold — it is unrepresentable rather than validated, which is the
/// distinction the compile-fail probe pins.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DurableAttemptReceipt {
    attempt: BoundedAttemptId,
    session: BoundedSessionId,
    step: StepNumber,
    ordinal: AttemptOrdinal,
    reserved: ReservedMarker,
    request_sent: Option<RequestSentMarker>,
    settled: Option<SettledMarker>,
}

impl DurableAttemptReceipt {
    /// Mint a receipt in its only legal initial state: reserved, nothing else.
    pub fn reserved(
        attempt: BoundedAttemptId,
        session: BoundedSessionId,
        step: StepNumber,
        ordinal: AttemptOrdinal,
        reserved: ReservedMarker,
    ) -> Self {
        Self {
            attempt,
            session,
            step,
            ordinal,
            reserved,
            request_sent: None,
            settled: None,
        }
    }

    /// Record that bytes left the process.
    ///
    /// # O1 — exact retry is idempotent, a different instant is a conflict
    ///
    /// R1 finding (review, adopted): the first version silently kept the
    /// first marker and discarded any second one, so an exact retry — the same
    /// instant re-applied after a crash or a duplicated write — was
    /// indistinguishable from a genuine conflict between two different
    /// observations. Silence is the wrong answer to both: the first is normal
    /// and must be idempotent, the second is a defect and must be visible.
    ///
    /// The first marker is **always** preserved, in every outcome. The returned
    /// [`MarkerOutcome`] says which of the three cases occurred, and a caller
    /// that ignores it still cannot corrupt the receipt.
    #[must_use = "the outcome distinguishes an exact retry from a conflict"]
    pub fn mark_request_sent(mut self, marker: RequestSentMarker) -> (Self, MarkerOutcome) {
        match self.request_sent.as_ref() {
            None => {
                self.request_sent = Some(marker);
                (self, MarkerOutcome::Recorded)
            }
            Some(held) if held.at() == marker.at() => (self, MarkerOutcome::ExactRetry),
            Some(held) => {
                let outcome = MarkerOutcome::Conflict {
                    held: held.at(),
                    offered: marker.at(),
                };
                (self, outcome)
            }
        }
    }

    /// Record the durable terminal, under the same O1 law as
    /// [`DurableAttemptReceipt::mark_request_sent`].
    #[must_use = "the outcome distinguishes an exact retry from a conflict"]
    pub fn mark_settled(mut self, marker: SettledMarker) -> (Self, MarkerOutcome) {
        match self.settled.as_ref() {
            None => {
                self.settled = Some(marker);
                (self, MarkerOutcome::Recorded)
            }
            Some(held) if held.at() == marker.at() => (self, MarkerOutcome::ExactRetry),
            Some(held) => {
                let outcome = MarkerOutcome::Conflict {
                    held: held.at(),
                    offered: marker.at(),
                };
                (self, outcome)
            }
        }
    }

    pub fn attempt(&self) -> &PhysicalAttemptId {
        self.attempt.id()
    }

    pub fn session(&self) -> &SessionId {
        self.session.id()
    }

    /// The two variable-length fields' total byte length. Every other field is
    /// fixed width, so this is the whole of the receipt's variability.
    pub fn identity_bytes(&self) -> usize {
        self.attempt.byte_len() + self.session.byte_len()
    }

    pub fn step(&self) -> StepNumber {
        self.step
    }

    pub fn ordinal(&self) -> AttemptOrdinal {
        self.ordinal
    }

    pub fn reserved_at(&self) -> Timestamp {
        self.reserved.at()
    }

    pub fn request_sent_at(&self) -> Option<Timestamp> {
        self.request_sent.as_ref().map(RequestSentMarker::at)
    }

    pub fn settled_at(&self) -> Option<Timestamp> {
        self.settled.as_ref().map(SettledMarker::at)
    }

    /// The classification law, total over the marker structure.
    ///
    /// | reserved | request sent | settled | disposition |
    /// |---|---|---|---|
    /// | yes | no | no | `Lost(NoneInitiated)` — replayable |
    /// | yes | yes | no | `Lost(Ambiguous { sent_at })` — never automatically replayed |
    /// | yes | any | yes | `AlreadySettled` — nothing to reconcile |
    ///
    /// **Disclosed refinement of my own preflight prose.** The preflight said
    /// "a receipt with `AttemptSettled` is `Settled`", and the admission
    /// quoted that phrasing. Implemented literally it would be false in one
    /// case: an attempt that settled *without ever sending bytes* initiated no
    /// external effect, so calling it `ExternalEffectState::Settled` — whose
    /// `safe_to_replay` is `false` — would assert an external effect that never
    /// happened. The honest domain is narrower: a settled receipt is not
    /// reconciled **at all**, so it needs no effect state, and `AlreadySettled`
    /// says exactly that. The two unsettled rows are unchanged from the ruling.
    /// If the literal three-way mapping is wanted instead, this function and
    /// its witness are the single place to change.
    pub fn disposition(&self) -> ReceiptDisposition {
        if self.settled.is_some() {
            return ReceiptDisposition::AlreadySettled;
        }
        match self.request_sent.as_ref() {
            None => ReceiptDisposition::Lost(LostEffect::NoneInitiated),
            Some(sent) => ReceiptDisposition::Lost(LostEffect::Ambiguous { sent_at: sent.at() }),
        }
    }

    /// The description a reconciler may record as its evidence text.
    ///
    /// Offered rather than imposed: it carries the attempt identity and the two
    /// instants and nothing else — no payload, nothing provider-shaped, and no
    /// length that varies with a secret. A reconciler that observed something
    /// more specific is free to say so instead.
    pub fn describe_ambiguity(&self, sent_at: Timestamp) -> String {
        format!(
            "attempt {} step {} ordinal {} reserved at {} sent at {} with no settlement",
            self.attempt.id().as_str(),
            self.step.value(),
            self.ordinal.value(),
            self.reserved.at().as_millis(),
            sent_at.as_millis()
        )
    }
}

/// A bounded page size for the later reconciliation scan.
///
/// **This bounds ROWS, and rows only.** It bounds cursor granularity, read
/// transaction duration and the progress a pass can record — it does **not**
/// bound bytes or memory, and R2 finding Z2 exists because the first version's
/// prose let that be read the other way. Memory is bounded by the byte maxima
/// on [`BoundedAttemptId`] and [`BoundedSessionId`], because those are the
/// receipt's only variable-length fields.
///
/// P2: the page size and per-pass
/// ceiling are **injected non-zero bounds with no `Default`**, and A0 may
/// define the validated types but must not choose production values. So this
/// type exists and carries no constant: nothing in the workspace ratifies a
/// number, and composition records the production values as a residual.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciliationPage(std::num::NonZeroU32);

impl ReconciliationPage {
    pub fn new(value: std::num::NonZeroU32) -> Self {
        Self(value)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

/// The per-pass ceiling on receipts examined.
///
/// Same rule as [`ReconciliationPage`] in both directions: validated here and
/// valued elsewhere, and bounding **rows** — how much work one pass may do
/// before yielding — never bytes or memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconciliationCeiling(std::num::NonZeroU32);

impl ReconciliationCeiling {
    pub fn new(value: std::num::NonZeroU32) -> Self {
        Self(value)
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A FIXTURE maximum. It is not a production value and names none: no
    /// PhysicalAttemptId mint or prefix is ruled, so every test injects its own
    /// bound exactly as a caller must.
    fn max_bytes(value: u32) -> std::num::NonZeroU32 {
        match std::num::NonZeroU32::new(value) {
            Some(value) => value,
            None => panic!("a non-zero fixture maximum"),
        }
    }

    fn bounded(
        attempt: PhysicalAttemptId,
        session: SessionId,
    ) -> (BoundedAttemptId, BoundedSessionId) {
        let attempt = match BoundedAttemptId::new(attempt, max_bytes(64)) {
            Ok(bounded) => bounded,
            Err(error) => panic!("fixture attempt id fits: {error:?}"),
        };
        let session = match BoundedSessionId::new(session, max_bytes(64)) {
            Ok(bounded) => bounded,
            Err(error) => panic!("fixture session id fits: {error:?}"),
        };
        (attempt, session)
    }

    fn ids() -> (PhysicalAttemptId, SessionId, StepNumber, AttemptOrdinal) {
        // `SessionId` is a prefix-validated parse, not an opaque_id: it is the
        // only one of the four that refuses a wrong prefix rather than only an
        // empty string.
        let session: SessionId = match "ses_033601c26ffe".parse() {
            Ok(session) => session,
            Err(_) => panic!("a well-formed session id parses"),
        };
        match (
            PhysicalAttemptId::new("att_1"),
            StepNumber::new(3),
            AttemptOrdinal::new(2),
        ) {
            (Some(attempt), Some(step), Some(ordinal)) => (attempt, session, step, ordinal),
            _ => panic!("test identity coordinates must be valid"),
        }
    }

    fn reserved() -> DurableAttemptReceipt {
        let (attempt, session, step, ordinal) = ids();
        let (attempt, session) = bounded(attempt, session);
        DurableAttemptReceipt::reserved(
            attempt,
            session,
            step,
            ordinal,
            ReservedMarker::new(Timestamp::from_millis(1_000)),
        )
    }

    fn evidence_from(
        receipt: &DurableAttemptReceipt,
    ) -> impl FnOnce(Timestamp) -> AmbiguityEvidence + '_ {
        move |sent_at| match AmbiguityEvidence::new(receipt.describe_ambiguity(sent_at)) {
            Some(evidence) => evidence,
            None => panic!("the offered description is never empty"),
        }
    }

    #[test]
    fn reserved_without_sent_is_none_initiated_and_replayable() {
        let receipt = reserved();
        match receipt.disposition() {
            ReceiptDisposition::Lost(effect) => {
                assert_eq!(effect, LostEffect::NoneInitiated);
                assert!(effect.safe_to_replay(), "nothing left the process");
                assert_eq!(
                    effect.into_effect_state(evidence_from(&receipt)),
                    ExternalEffectState::NoneInitiated
                );
            }
            other => panic!("expected a lost attempt, got {other:?}"),
        }
    }

    #[test]
    fn sent_without_settled_is_ambiguous_and_never_replayable() {
        let (receipt, _) =
            reserved().mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(1_200)));
        let effect = match receipt.disposition() {
            ReceiptDisposition::Lost(effect) => effect,
            other => panic!("expected a lost attempt, got {other:?}"),
        };
        assert_eq!(
            effect,
            LostEffect::Ambiguous {
                sent_at: Timestamp::from_millis(1_200)
            }
        );
        assert!(
            !effect.safe_to_replay(),
            "an ambiguous external effect is never automatically replayed"
        );
        match effect.into_effect_state(evidence_from(&receipt)) {
            ExternalEffectState::Ambiguous { evidence } => {
                let text = evidence.as_str();
                assert!(text.contains("att_1"), "{text}");
                assert!(text.contains("1200"), "{text}");
                assert!(text.contains("no settlement"), "{text}");
            }
            other => panic!("expected an ambiguous state, got {other:?}"),
        }
    }

    #[test]
    fn the_replay_rule_agrees_with_the_landed_vocabulary() {
        // This convenience must never become a second, softer rule than the
        // accepted one it shadows.
        let receipt = reserved();
        let none = LostEffect::NoneInitiated;
        assert_eq!(
            none.safe_to_replay(),
            none.into_effect_state(evidence_from(&receipt))
                .safe_to_replay()
        );
        let receipt = reserved()
            .mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(5)))
            .0;
        let ambiguous = LostEffect::Ambiguous {
            sent_at: Timestamp::from_millis(5),
        };
        assert_eq!(
            ambiguous.safe_to_replay(),
            ambiguous
                .into_effect_state(evidence_from(&receipt))
                .safe_to_replay()
        );
    }

    #[test]
    fn a_settled_receipt_is_not_reconciled_at_all() {
        let (sent, _) =
            reserved().mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(1_200)));
        let (settled, _) = sent.mark_settled(SettledMarker::new(Timestamp::from_millis(1_500)));
        assert_eq!(settled.disposition(), ReceiptDisposition::AlreadySettled);

        // And the same holds for an attempt that settled before sending: it is
        // not reconciled, and in particular it is NOT reported as an external
        // effect that never happened.
        let (never_sent, _) =
            reserved().mark_settled(SettledMarker::new(Timestamp::from_millis(1_100)));
        assert_eq!(never_sent.disposition(), ReceiptDisposition::AlreadySettled);
    }

    #[test]
    fn the_classification_is_total_over_the_marker_structure() {
        let base = reserved();
        let (sent, _) = base
            .clone()
            .mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(2)));
        let (settled, _) = base
            .clone()
            .mark_settled(SettledMarker::new(Timestamp::from_millis(3)));
        let (both, _) = sent
            .clone()
            .mark_settled(SettledMarker::new(Timestamp::from_millis(4)));

        assert_eq!(
            base.disposition(),
            ReceiptDisposition::Lost(LostEffect::NoneInitiated)
        );
        assert_eq!(
            sent.disposition(),
            ReceiptDisposition::Lost(LostEffect::Ambiguous {
                sent_at: Timestamp::from_millis(2)
            })
        );
        assert_eq!(settled.disposition(), ReceiptDisposition::AlreadySettled);
        assert_eq!(both.disposition(), ReceiptDisposition::AlreadySettled);
    }

    #[test]
    fn an_exact_retry_is_idempotent_and_a_different_marker_is_a_conflict() {
        // O1. The two cases the first version could not tell apart.
        let (receipt, first) =
            reserved().mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(10)));
        assert_eq!(first, MarkerOutcome::Recorded);
        assert!(first.changed());

        // Exact retry: same instant, idempotent, no conflict.
        let (receipt, retry) =
            receipt.mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(10)));
        assert_eq!(retry, MarkerOutcome::ExactRetry);
        assert!(!retry.changed());
        assert!(!retry.is_conflict());
        assert_eq!(receipt.request_sent_at(), Some(Timestamp::from_millis(10)));

        // A DIFFERENT instant is a typed conflict carrying both values, and the
        // held marker survives untouched.
        let (receipt, conflict) =
            receipt.mark_request_sent(RequestSentMarker::new(Timestamp::from_millis(99)));
        assert_eq!(
            conflict,
            MarkerOutcome::Conflict {
                held: Timestamp::from_millis(10),
                offered: Timestamp::from_millis(99),
            }
        );
        assert!(conflict.is_conflict());
        assert_eq!(
            receipt.request_sent_at(),
            Some(Timestamp::from_millis(10)),
            "the first marker is preserved in every outcome"
        );

        // The settled marker obeys the identical law.
        let (receipt, recorded) =
            receipt.mark_settled(SettledMarker::new(Timestamp::from_millis(20)));
        assert_eq!(recorded, MarkerOutcome::Recorded);
        let (receipt, retry) = receipt.mark_settled(SettledMarker::new(Timestamp::from_millis(20)));
        assert_eq!(retry, MarkerOutcome::ExactRetry);
        let (receipt, conflict) =
            receipt.mark_settled(SettledMarker::new(Timestamp::from_millis(80)));
        assert!(conflict.is_conflict());
        assert_eq!(receipt.settled_at(), Some(Timestamp::from_millis(20)));
    }

    #[test]
    fn a_receipt_carries_its_own_identity_coordinates() {
        let receipt = reserved();
        assert_eq!(receipt.attempt().as_str(), "att_1");
        assert_eq!(receipt.session().as_str(), "ses_033601c26ffe");
        assert_eq!(receipt.step().value(), 3);
        assert_eq!(receipt.ordinal().value(), 2);
        assert_eq!(receipt.reserved_at().as_millis(), 1_000);
        assert_eq!(receipt.request_sent_at(), None);
        assert_eq!(receipt.settled_at(), None);
    }

    #[test]
    fn the_scan_bounds_are_validated_and_have_no_default_value() {
        // P2: the TYPES exist here; the numbers do not. `NonZeroU32` is what
        // makes a zero page unrepresentable rather than a runtime check.
        let page = match std::num::NonZeroU32::new(1) {
            Some(value) => ReconciliationPage::new(value),
            None => panic!("1 is non-zero"),
        };
        assert_eq!(page.get(), 1);
        let ceiling = match std::num::NonZeroU32::new(7) {
            Some(value) => ReconciliationCeiling::new(value),
            None => panic!("7 is non-zero"),
        };
        assert_eq!(ceiling.get(), 7);
        assert!(std::num::NonZeroU32::new(0).is_none());
    }

    // -----------------------------------------------------------------------
    // R2 correction witnesses (Z2). Every maximum here is a FIXTURE value.
    // -----------------------------------------------------------------------

    /// An identity of exactly the maximum is admitted; one byte over is
    /// refused, and the refusal names both numbers.
    #[test]
    fn an_identity_at_the_maximum_is_admitted_and_one_over_is_refused() {
        let at_max = match PhysicalAttemptId::new("a".repeat(8)) {
            Some(id) => id,
            None => panic!("non-empty"),
        };
        match BoundedAttemptId::new(at_max, max_bytes(8)) {
            Ok(bounded) => assert_eq!(bounded.byte_len(), 8),
            Err(error) => panic!("exactly at the maximum must be admitted: {error:?}"),
        }

        let one_over = match PhysicalAttemptId::new("a".repeat(9)) {
            Some(id) => id,
            None => panic!("non-empty"),
        };
        assert_eq!(
            BoundedAttemptId::new(one_over, max_bytes(8)),
            Err(IdTooLong {
                kind: BoundedIdKind::Attempt,
                bytes: 9,
                maximum: 8,
            })
        );
    }

    /// The bound is measured in UTF-8 BYTES, not characters: a three-character
    /// identity of two-byte code points is six bytes long.
    #[test]
    fn the_attempt_bound_counts_utf8_bytes_not_characters() {
        let multibyte = "AAA";
        let multibyte = multibyte.replace('A', "\u{00e9}");
        assert_eq!(multibyte.chars().count(), 3);
        assert_eq!(multibyte.len(), 6);
        let id = match PhysicalAttemptId::new(multibyte) {
            Some(id) => id,
            None => panic!("non-empty"),
        };
        assert_eq!(
            BoundedAttemptId::new(id, max_bytes(3)),
            Err(IdTooLong {
                kind: BoundedIdKind::Attempt,
                bytes: 6,
                maximum: 3,
            }),
            "a character count would have admitted this"
        );
    }

    /// The session bound is stated and measured in bytes too, and its refusal
    /// is a DIFFERENT kind so two refusals cannot be confused.
    #[test]
    fn the_session_bound_is_measured_in_bytes_and_named_separately() {
        let session: SessionId = match "ses_033601c26ffe".parse() {
            Ok(session) => session,
            Err(_) => panic!("well-formed"),
        };
        assert_eq!(session.as_str().len(), 16);
        match BoundedSessionId::new(session.clone(), max_bytes(16)) {
            Ok(bounded) => assert_eq!(bounded.byte_len(), 16),
            Err(error) => panic!("exactly at the maximum: {error:?}"),
        }
        assert_eq!(
            BoundedSessionId::new(session, max_bytes(15)),
            Err(IdTooLong {
                kind: BoundedIdKind::Session,
                bytes: 16,
                maximum: 15,
            })
        );
    }

    /// The wrapper adds a bound and NOT a value: the identity comes back byte
    /// for byte, which is what separates this from a projection.
    #[test]
    fn the_wrapper_returns_the_identity_unchanged() {
        let receipt = reserved();
        assert_eq!(receipt.attempt().as_str(), "att_1");
        assert_eq!(receipt.session().as_str(), "ses_033601c26ffe");
        assert_eq!(
            receipt.identity_bytes(),
            "att_1".len() + "ses_033601c26ffe".len(),
            "the two identities are the receipt's only variable-length content"
        );
    }
}
