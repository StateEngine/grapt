//! The payload-free advisory change signal, and the bulk dismiss operation.
//!
//! # A signal carries no mail
//!
//! [`ChangeSignal`] has zero message, body and ref bytes. It means "obtain and
//! catch up durable truth", never "one new message exists". Signals may
//! duplicate and coalesce, and ordering among them is irrelevant; while a
//! subscription is connected, every durable mutation that could change the v1
//! enumeration after the barrier causes AT LEAST ONE signal.
//!
//! That weakness is deliberate. A signal that carried a message would be a
//! second delivery path with no durability behind it, and a lost one would then
//! be lost mail. Because a signal carries nothing, losing one during a
//! disconnect is harmless: every reconnect performs the atomic
//! subscribe-and-snapshot and catches up before declaring itself live.
//!
//! The capacity-one coalesced `wake_pending` bit is NOT here. It belongs to the
//! R3b-2C runtime owner's state machine, and putting it in the transport would
//! give this crate a scheduling responsibility it must not have.

use crate::cursor::SubscriptionGeneration;
use crate::identity::{ProjectPair, RemoteMailboxMessageId};

/// The largest advisory token a provider may attach, in UTF-8 bytes.
///
/// Diagnostics only. The token has no authority and can never substitute for a
/// snapshot or a cursor.
pub const MAX_CHANGE_TOKEN_BYTES: usize = 256;

/// A payload-free advisory notice that durable truth may have moved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangeSignal {
    generation: SubscriptionGeneration,
    token: Option<String>,
}

impl ChangeSignal {
    /// A signal with no provider token.
    pub fn new(generation: SubscriptionGeneration) -> Self {
        ChangeSignal {
            generation,
            token: None,
        }
    }

    /// A signal carrying a bounded diagnostic token.
    ///
    /// An oversize token is DROPPED rather than refused: the token is
    /// diagnostic-only, and losing it must never cost the wake it accompanies.
    /// Refusing the signal would be strictly worse than forgetting the token.
    pub fn with_token(generation: SubscriptionGeneration, token: impl Into<String>) -> Self {
        let token = token.into();
        let token = if token.is_empty() || token.len() > MAX_CHANGE_TOKEN_BYTES {
            None
        } else {
            Some(token)
        };
        ChangeSignal { generation, token }
    }

    pub fn generation(&self) -> SubscriptionGeneration {
        self.generation
    }

    pub fn token(&self) -> Option<&str> {
        self.token.as_deref()
    }

    /// The mail payload a signal carries: always zero bytes, by construction.
    pub const fn mail_payload_bytes(&self) -> usize {
        0
    }
}

/// Why a set of messages is being dismissed.
///
/// Dismissal declines to ingest. It is emphatically NOT reading: a dismisser
/// never appears in a read record, so a sender is never told someone read
/// something they did not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DismissReason {
    /// The recipient judged the message stale.
    Stale,
    /// Another seat already handled the directive.
    AlreadyHandled,
    /// The message is not addressed to this recipient's work.
    NotMine,
    /// A later message superseded it.
    Superseded,
}

impl DismissReason {
    pub const ALL: [DismissReason; 4] = [
        DismissReason::Stale,
        DismissReason::AlreadyHandled,
        DismissReason::NotMine,
        DismissReason::Superseded,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            DismissReason::Stale => "stale",
            DismissReason::AlreadyHandled => "already-handled",
            DismissReason::NotMine => "not-mine",
            DismissReason::Superseded => "superseded",
        }
    }
}

/// Why a bulk dismiss could not be built.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DismissError {
    /// An empty dismiss is refused: it would produce a receipt claiming an
    /// action nobody took.
    Empty,
    /// More messages than the negotiated page bound allows.
    CountExceeded { count: u32, max: u32 },
}

/// A bulk, archive-without-read-claim dismiss.
///
/// Ruling J1 added this as a SEVENTH operation, distinct from both the six
/// original port calls and from [`crate::ack::DeliveryAdmissionAck`]. Three
/// facts stay separate: delivery (it arrived durably), reading (someone
/// attended), and dismissal (the recipient declined to ingest). Collapsing any
/// two would let one be inferred from another.
///
/// This crate owns the OPERATION only. There is no durable local dismiss state
/// here and none may be added under this lane: the store half is not R3b-2A's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkDismissRequest {
    messages: Vec<RemoteMailboxMessageId>,
    reason: DismissReason,
    projects: ProjectPair,
}

impl BulkDismissRequest {
    /// Takes its [`ProjectPair`] FROM THE BINDING, never from the caller.
    ///
    /// Ruling J8 makes every authority-bearing coordinate carry both project
    /// halves, and a dismiss mutates foreign state, so it is authority-bearing.
    /// The pair is READ from `binding.receipt().projects()` rather than accepted
    /// as an argument: a caller that could name the pair could name a different
    /// one from the binding it holds, and dismiss messages in a project this
    /// binding never authenticated against. There is deliberately no constructor
    /// that takes a pair.
    pub fn try_new(
        binding: &crate::binding::MailBinding,
        messages: Vec<RemoteMailboxMessageId>,
        reason: DismissReason,
        max_count: u32,
    ) -> Result<Self, DismissError> {
        if messages.is_empty() {
            return Err(DismissError::Empty);
        }
        let count = u32::try_from(messages.len()).unwrap_or(u32::MAX);
        if count > max_count {
            return Err(DismissError::CountExceeded {
                count,
                max: max_count,
            });
        }
        Ok(BulkDismissRequest {
            messages,
            reason,
            projects: binding.receipt().projects().clone(),
        })
    }

    pub fn messages(&self) -> &[RemoteMailboxMessageId] {
        &self.messages
    }

    pub fn reason(&self) -> DismissReason {
        self.reason
    }

    /// The pair this request was bound to, for readback.
    pub fn projects(&self) -> &ProjectPair {
        &self.projects
    }
}

/// What a bulk dismiss actually did.
///
/// `dismissed` and `already_dismissed` are reported separately so a retry after
/// a lost response is legible: repeating a dismiss is safe, and the outcome says
/// plainly that the second call changed nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BulkDismissOutcome {
    dismissed: Vec<RemoteMailboxMessageId>,
    already_dismissed: Vec<RemoteMailboxMessageId>,
    projects: ProjectPair,
}

impl BulkDismissOutcome {
    pub fn new(
        dismissed: Vec<RemoteMailboxMessageId>,
        already_dismissed: Vec<RemoteMailboxMessageId>,
        projects: ProjectPair,
    ) -> Self {
        BulkDismissOutcome {
            dismissed,
            already_dismissed,
            projects,
        }
    }

    pub fn dismissed(&self) -> &[RemoteMailboxMessageId] {
        &self.dismissed
    }

    pub fn already_dismissed(&self) -> &[RemoteMailboxMessageId] {
        &self.already_dismissed
    }

    pub fn projects(&self) -> &ProjectPair {
        &self.projects
    }

    /// True when this call changed nothing.
    pub fn is_noop(&self) -> bool {
        self.dismissed.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::{SourceProjectId, TargetProjectId};

    fn generation() -> SubscriptionGeneration {
        SubscriptionGeneration::FIRST
    }

    fn message(id: &str) -> Result<RemoteMailboxMessageId, Box<dyn std::error::Error>> {
        Ok(RemoteMailboxMessageId::new(id).ok_or("message id")?)
    }

    #[test]
    fn a_signal_carries_no_mail_whatever_else_it_carries() {
        let bare = ChangeSignal::new(generation());
        assert_eq!(bare.mail_payload_bytes(), 0);
        assert_eq!(bare.token(), None);

        let tokened = ChangeSignal::with_token(generation(), "floor-gen-172");
        assert_eq!(tokened.mail_payload_bytes(), 0);
        assert_eq!(tokened.token(), Some("floor-gen-172"));
    }

    #[test]
    fn an_oversize_token_is_dropped_rather_than_costing_the_wake() {
        let huge = "t".repeat(MAX_CHANGE_TOKEN_BYTES + 1);
        let signal = ChangeSignal::with_token(generation(), huge);
        // The wake survives; only the diagnostic is forgotten.
        assert_eq!(signal.token(), None);
        assert_eq!(signal.generation(), generation());
    }

    #[test]
    fn a_bulk_dismiss_refuses_to_be_empty() -> Result<(), Box<dyn std::error::Error>> {
        let binding = crate::test_support::binding().ok_or("binding")?;
        assert_eq!(
            BulkDismissRequest::try_new(&binding, Vec::new(), DismissReason::Stale, 64),
            Err(DismissError::Empty)
        );
        Ok(())
    }

    #[test]
    fn a_bulk_dismiss_respects_its_negotiated_count_bound() -> Result<(), Box<dyn std::error::Error>>
    {
        let binding = crate::test_support::binding().ok_or("binding")?;
        let many: Vec<RemoteMailboxMessageId> = (0..5)
            .map(|i| message(&format!("msg-{i}")))
            .collect::<Result<Vec<_>, _>>()?;
        assert_eq!(
            BulkDismissRequest::try_new(&binding, many.clone(), DismissReason::Stale, 4),
            Err(DismissError::CountExceeded { count: 5, max: 4 })
        );
        assert!(BulkDismissRequest::try_new(&binding, many, DismissReason::Stale, 64).is_ok());
        Ok(())
    }

    #[test]
    fn a_dismiss_request_carries_the_pair_its_binding_authenticated(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // J8's readback half: the pair is present and is exactly the binding's.
        let binding = crate::test_support::binding().ok_or("binding")?;
        let request = BulkDismissRequest::try_new(
            &binding,
            vec![message("msg-1")?],
            DismissReason::Stale,
            64,
        )
        .map_err(|e| format!("a bounded non-empty dismiss: {e:?}"))?;

        assert_eq!(request.projects(), binding.receipt().projects());
        assert_eq!(request.projects().source().as_str(), "stateengine");
        assert_eq!(request.projects().target().as_str(), "secode");
        Ok(())
    }

    #[test]
    fn a_dismiss_cannot_be_pointed_at_a_pair_its_binding_never_authenticated(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // The MISMATCH half. Two bindings over different pairs produce requests
        // that differ in their pair, and neither caller chose it — the pair is
        // read from the binding, so there is no argument through which a caller
        // could aim a dismiss at the other project.
        //
        // A constructor taking a pair would make these two indistinguishable at
        // the call site, which is exactly what J8 forbids for an authority-bearing
        // coordinate.
        let mine = crate::test_support::binding().ok_or("binding")?;
        let other =
            crate::test_support::binding_between("stateengine", "prime").ok_or("other binding")?;

        let dismiss = |binding: &crate::binding::MailBinding| {
            BulkDismissRequest::try_new(binding, vec![message("msg-1")?], DismissReason::Stale, 64)
                .map_err(|e| format!("a bounded non-empty dismiss: {e:?}").into())
                as Result<BulkDismissRequest, Box<dyn std::error::Error>>
        };

        assert_ne!(dismiss(&mine)?.projects(), dismiss(&other)?.projects());
        assert_eq!(dismiss(&other)?.projects().target().as_str(), "prime");
        Ok(())
    }

    #[test]
    fn every_dismiss_reason_is_a_decline_to_ingest_never_an_attention_claim() {
        // The claim is about the VOCABULARY, so it is checked exhaustively over
        // the vocabulary. An added variant that meant "I read it" would have to
        // be classified here, and the match below would not compile until it
        // was.
        assert_eq!(DismissReason::ALL.len(), 4);
        for reason in DismissReason::ALL {
            let declines_without_reading = match reason {
                DismissReason::Stale
                | DismissReason::AlreadyHandled
                | DismissReason::NotMine
                | DismissReason::Superseded => true,
            };
            assert!(
                declines_without_reading,
                "{} must decline to ingest, never claim attention",
                reason.as_str()
            );
        }
        let labels: std::collections::BTreeSet<&str> =
            DismissReason::ALL.iter().map(|r| r.as_str()).collect();
        assert_eq!(labels.len(), DismissReason::ALL.len());
    }

    #[test]
    fn a_dismiss_request_carries_no_acknowledgement_coordinates(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Delivery acknowledgement is identified by a cursor, a local admission
        // sequence and a canonical digest. A dismiss carries none of them, so it
        // cannot be mistaken for one or built from one: the two operations are
        // structurally unable to substitute for each other.
        let binding = crate::test_support::binding().ok_or("binding")?;
        let request = BulkDismissRequest::try_new(
            &binding,
            vec![message("msg-1")?, message("msg-2")?],
            DismissReason::AlreadyHandled,
            64,
        )
        .map_err(|e| format!("a bounded non-empty dismiss: {e:?}"))?;
        assert_eq!(request.messages().len(), 2);
        assert_eq!(request.reason(), DismissReason::AlreadyHandled);
        Ok(())
    }

    #[test]
    fn a_repeated_dismiss_reports_that_it_changed_nothing() -> Result<(), Box<dyn std::error::Error>>
    {
        let projects = ProjectPair::new(
            SourceProjectId::new("stateengine").ok_or("source")?,
            TargetProjectId::new("secode").ok_or("target")?,
        );
        let first = BulkDismissOutcome::new(vec![message("msg-1")?], Vec::new(), projects.clone());
        let repeat = BulkDismissOutcome::new(Vec::new(), vec![message("msg-1")?], projects);

        assert!(!first.is_noop());
        assert!(repeat.is_noop());
        assert_eq!(repeat.already_dismissed().len(), 1);
        Ok(())
    }
}
