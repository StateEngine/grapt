//! Opening a binding, and what ruling J2 put where the lease used to be.
//!
//! # The lease is gone
//!
//! The admission packet asked for a fenced, revocable `MailLease` and a
//! provider-supplied monotonic fence before any cross-process exclusivity could
//! be claimed. J2 refused all of it: an agent id owns its inbox exactly as it
//! does today, the new binding is a VISIBLE TAKEOVER WITH RECEIPTS BOTH WAYS,
//! and leases and fences are deferred to Studio multiplayer if they are ever
//! wanted at all. The guiding line the ruling states is "honesty features kept,
//! fortress features cut", and a lease is the fortress: it buys mutual exclusion
//! that this product does not need, at the cost of durable coordination,
//! revocation handling and a recovery path for every stale owner.
//!
//! What replaces it is strictly weaker and strictly more honest:
//!
//! * a second opener SUCCEEDS. It is not a `LeaseConflict`, because under J2 it
//!   is not a conflict — it is the newer owner arriving;
//! * the arriving owner receives a [`TakeoverReceipt`] naming who it displaced;
//! * the displaced owner's subscription closes with
//!   [`crate::failure::SubscriptionClosed::DisplacedByTakeover`], naming who
//!   displaced it.
//!   Both halves exist so neither side can silently believe it still owns the
//!   inbox — a stopped seat that looked like a quiet inbox is exactly the
//!   failure this replaces;
//! * [`BindingGeneration`] orders generations for diagnosis. It is NOT a fence
//!   and rejects nobody's write. Duplicate admission is prevented downstream by
//!   exact immutable source identity, which works without any coordination at
//!   all.
//!
//! # Why the handle is private, non-clonable and unforgeable
//!
//! [`MailBinding`] has no `Clone`, no serialized form, and no constructor
//! reachable from outside this crate. That is CODEC law, not fencing: a handle
//! that could be copied into a durable row or a message could be replayed later
//! against a binding that no longer means what it did. Nothing in this crate
//! depends on serde, so no derive can accidentally give it a wire shape.
//!
//! ## The seal this sentence used to overstate
//!
//! An earlier revision claimed "no public constructor" while `MailBinding::new`
//! was `pub` and re-exported. That was false, and not harmlessly: a review
//! demonstrated it by compiling an external consumer that forged a binding
//! naming an arbitrary `verified_principal`, with no adapter and no verification
//! at all. [`MailBinding::verify_expectations`] cannot catch that, because it
//! compares a request against a receipt the same caller authored.
//!
//! Both construction routes are now crate-owned:
//!
//! * `MailBinding::new` and `MailBindingReceipt::new` are `pub(crate)`, as
//!   narrowly as the deterministic fake and a future in-crate adapter require;
//! * [`crate::port::RemoteSeatMailTransportV1`] is sealed by a private
//!   supertrait, so no external type can implement the port and return a binding
//!   that way either.
//!
//! Closing only one of the two would have left the other open. Probes `03` and
//! `04` witness them separately, because a single probe file stops at its first
//! error phase and would witness only whichever failed first.

use crate::failure::OpenFailure;
use crate::identity::{
    AdapterVersion, MailTransportClientInstanceId, ProjectPair, RecipientMappingVersion,
    SourceBindingId, SourcePrincipalId, TargetProjectId,
};
use crate::limits::SettledTransportLimits;
use grapt_types::mailbox::SeatRecipientId;
use grapt_types::protocol::ProtocolVersion;

/// What this workspace asks for when opening a binding.
///
/// It contains ONLY Grapt-side expectations. There is deliberately no
/// caller-authored foreign principal, tenant, credential, registry path or
/// authority claim: the adapter obtains the authenticated foreign identity from
/// its configured transport binding, and a caller that could name its own
/// principal could name someone else's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpenRequest {
    expected_local_project: TargetProjectId,
    expected_recipient: SeatRecipientId,
    requested_protocol: ProtocolVersion,
    client_instance: MailTransportClientInstanceId,
}

impl OpenRequest {
    pub fn new(
        expected_local_project: TargetProjectId,
        expected_recipient: SeatRecipientId,
        requested_protocol: ProtocolVersion,
        client_instance: MailTransportClientInstanceId,
    ) -> Self {
        OpenRequest {
            expected_local_project,
            expected_recipient,
            requested_protocol,
            client_instance,
        }
    }

    pub fn expected_local_project(&self) -> &TargetProjectId {
        &self.expected_local_project
    }

    pub fn expected_recipient(&self) -> &SeatRecipientId {
        &self.expected_recipient
    }

    pub fn requested_protocol(&self) -> &ProtocolVersion {
        &self.requested_protocol
    }

    pub fn client_instance(&self) -> &MailTransportClientInstanceId {
        &self.client_instance
    }
}

/// A binding's ordinal, for diagnosis and stale-signal discard.
///
/// Explicitly not a fence. See the module docs: J2 removed fencing, so a higher
/// generation carries no authority to refuse a lower one's work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct BindingGeneration(u64);

impl BindingGeneration {
    pub const FIRST: BindingGeneration = BindingGeneration(1);

    /// Zero is refused so "unbound" cannot be confused with a first binding.
    pub fn new(value: u64) -> Option<Self> {
        if value == 0 {
            None
        } else {
            Some(BindingGeneration(value))
        }
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(BindingGeneration)
    }
}

/// Who held a recipient's inbox before a takeover.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DisplacedOwner {
    principal: SourcePrincipalId,
    client: MailTransportClientInstanceId,
    generation: BindingGeneration,
}

impl DisplacedOwner {
    pub fn new(
        principal: SourcePrincipalId,
        client: MailTransportClientInstanceId,
        generation: BindingGeneration,
    ) -> Self {
        DisplacedOwner {
            principal,
            client,
            generation,
        }
    }

    pub fn principal(&self) -> &SourcePrincipalId {
        &self.principal
    }

    pub fn client(&self) -> &MailTransportClientInstanceId {
        &self.client
    }

    pub fn generation(&self) -> BindingGeneration {
        self.generation
    }
}

/// The arriving owner's half of J2's two-way receipt.
///
/// `displaced` is `None` on a clean first open and `Some` when this open took
/// the inbox from a live owner. Both are ordinary successes — the receipt
/// records WHICH happened so an operator reading a floor can tell a fresh boot
/// from a takeover without guessing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TakeoverReceipt {
    generation: BindingGeneration,
    displaced: Option<DisplacedOwner>,
}

impl TakeoverReceipt {
    pub fn first_open(generation: BindingGeneration) -> Self {
        TakeoverReceipt {
            generation,
            displaced: None,
        }
    }

    pub fn took_over(generation: BindingGeneration, displaced: DisplacedOwner) -> Self {
        TakeoverReceipt {
            generation,
            displaced: Some(displaced),
        }
    }

    pub fn generation(&self) -> BindingGeneration {
        self.generation
    }

    pub fn displaced(&self) -> Option<&DisplacedOwner> {
        self.displaced.as_ref()
    }

    /// True when this open displaced a live owner.
    pub fn is_takeover(&self) -> bool {
        self.displaced.is_some()
    }
}

/// The public-safe description of an open binding.
///
/// Carries opaque typed identities only. No credential, no raw foreign token,
/// no registry path or row: the addendum's amendment 6 lets StateEngine use its
/// own registry BEHIND the boundary, and forbids any of it crossing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailBindingReceipt {
    binding_id: SourceBindingId,
    protocol: ProtocolVersion,
    adapter_version: AdapterVersion,
    projects: ProjectPair,
    verified_principal: SourcePrincipalId,
    local_recipient: SeatRecipientId,
    mapping_version: RecipientMappingVersion,
    takeover: TakeoverReceipt,
    limits: SettledTransportLimits,
}

/// The parts a CRATE-OWNED adapter supplies when it has verified a binding.
///
/// Crate-visible, not public. Every field here is authority-bearing —
/// `verified_principal` most of all — so an external crate that could name them
/// could assert an authentication that never happened.
#[derive(Debug, Clone)]
pub(crate) struct MailBindingReceiptParts {
    pub binding_id: SourceBindingId,
    pub protocol: ProtocolVersion,
    pub adapter_version: AdapterVersion,
    pub projects: ProjectPair,
    pub verified_principal: SourcePrincipalId,
    pub local_recipient: SeatRecipientId,
    pub mapping_version: RecipientMappingVersion,
    pub takeover: TakeoverReceipt,
    pub limits: SettledTransportLimits,
}

impl MailBindingReceipt {
    pub(crate) fn new(parts: MailBindingReceiptParts) -> Self {
        MailBindingReceipt {
            binding_id: parts.binding_id,
            protocol: parts.protocol,
            adapter_version: parts.adapter_version,
            projects: parts.projects,
            verified_principal: parts.verified_principal,
            local_recipient: parts.local_recipient,
            mapping_version: parts.mapping_version,
            takeover: parts.takeover,
            limits: parts.limits,
        }
    }

    pub fn binding_id(&self) -> &SourceBindingId {
        &self.binding_id
    }

    pub fn protocol(&self) -> &ProtocolVersion {
        &self.protocol
    }

    pub fn adapter_version(&self) -> &AdapterVersion {
        &self.adapter_version
    }

    pub fn projects(&self) -> &ProjectPair {
        &self.projects
    }

    pub fn verified_principal(&self) -> &SourcePrincipalId {
        &self.verified_principal
    }

    pub fn local_recipient(&self) -> &SeatRecipientId {
        &self.local_recipient
    }

    pub fn mapping_version(&self) -> &RecipientMappingVersion {
        &self.mapping_version
    }

    pub fn takeover(&self) -> &TakeoverReceipt {
        &self.takeover
    }

    pub fn limits(&self) -> &SettledTransportLimits {
        &self.limits
    }
}

/// The private handle proving a binding is open.
///
/// Deliberately NOT `Clone`, and with no serialized form anywhere in this crate.
/// Operations take it by reference, so a caller cannot stash a second copy and
/// keep using it after the binding closes.
#[derive(Debug)]
pub struct MailBinding {
    receipt: MailBindingReceipt,
}

impl MailBinding {
    /// Constructed only by a CRATE-OWNED implementation of the port, after it
    /// has verified the authenticated principal maps to the expected recipient.
    ///
    /// `pub(crate)` is the whole seal on this side: an external consumer holds
    /// no route to a binding except calling `open` on a sealed adapter.
    pub(crate) fn new(receipt: MailBindingReceipt) -> Self {
        MailBinding { receipt }
    }

    pub fn receipt(&self) -> &MailBindingReceipt {
        &self.receipt
    }

    /// Checks a caller's expectations against what the provider actually
    /// verified.
    ///
    /// A message's own `to` field can never establish or widen this mapping;
    /// only the authenticated binding does, which is why this compares the
    /// REQUEST with the RECEIPT and not with any message.
    pub fn verify_expectations(&self, request: &OpenRequest) -> Result<(), OpenFailure> {
        if request.requested_protocol() != self.receipt.protocol() {
            return Err(OpenFailure::ProtocolVersionUnsupported);
        }
        if request.expected_local_project() != self.receipt.projects().target() {
            return Err(OpenFailure::ForeignProjectMismatch);
        }
        if request.expected_recipient() != self.receipt.local_recipient() {
            return Err(OpenFailure::RecipientMismatch);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SourceProjectId;

    fn limits() -> SettledTransportLimits {
        crate::test_support::settled_at_ceilings()
    }

    fn receipt(
        takeover: TakeoverReceipt,
    ) -> Result<MailBindingReceipt, Box<dyn std::error::Error>> {
        Ok({
            MailBindingReceipt::new(MailBindingReceiptParts {
                binding_id: SourceBindingId::new("bind-1").ok_or("binding")?,
                protocol: ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1)
                    .ok_or("protocol")?,
                adapter_version: AdapterVersion::new("fake-1").ok_or("adapter")?,
                projects: ProjectPair::new(
                    SourceProjectId::new("stateengine").ok_or("source")?,
                    TargetProjectId::new("secode").ok_or("target")?,
                ),
                verified_principal: SourcePrincipalId::new("agt-xztg").ok_or("principal")?,
                local_recipient: SeatRecipientId::new("seat-delta").ok_or("recipient")?,
                mapping_version: RecipientMappingVersion::new("map-1").ok_or("mapping")?,
                takeover,
                limits: limits(),
            })
        })
    }

    fn request() -> Result<OpenRequest, Box<dyn std::error::Error>> {
        Ok({
            OpenRequest::new(
                TargetProjectId::new("secode").ok_or("target")?,
                SeatRecipientId::new("seat-delta").ok_or("recipient")?,
                ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
                MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
            )
        })
    }

    #[test]
    fn a_first_open_is_not_a_takeover() {
        let receipt = TakeoverReceipt::first_open(BindingGeneration::FIRST);
        assert!(!receipt.is_takeover());
        assert!(receipt.displaced().is_none());
        assert_eq!(receipt.generation(), BindingGeneration::FIRST);
    }

    #[test]
    fn a_second_opener_succeeds_and_names_who_it_displaced(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Under J2 this is NOT a conflict. The old taxonomy would have refused
        // here with LeaseConflict; the ruling says the newer owner is simply
        // the owner, and the fact must be visible rather than fatal.
        let displaced = DisplacedOwner::new(
            SourcePrincipalId::new("agt-xztg").ok_or("principal")?,
            MailTransportClientInstanceId::new("pid-22360").ok_or("client")?,
            BindingGeneration::FIRST,
        );
        let second = BindingGeneration::FIRST.next().ok_or("next generation")?;
        let receipt = TakeoverReceipt::took_over(second, displaced);

        assert!(receipt.is_takeover());
        let who = receipt
            .displaced()
            .ok_or("a takeover names its predecessor")?;
        assert_eq!(who.client().as_str(), "pid-22360");
        assert_eq!(who.generation(), BindingGeneration::FIRST);
        assert!(receipt.generation() > who.generation());
        Ok(())
    }

    #[test]
    fn the_generation_orders_owners_without_fencing_anyone(
    ) -> Result<(), Box<dyn std::error::Error>> {
        // Ordering evidence only. There is deliberately no API here that takes
        // a generation and refuses a write, because J2 removed fencing.
        let first = BindingGeneration::FIRST;
        let second = first.next().ok_or("next")?;
        assert!(second > first);
        assert!(BindingGeneration::new(0).is_none());
        let last = BindingGeneration::new(u64::MAX).ok_or("max is valid")?;
        assert!(last.next().is_none(), "exhaustion refuses, never wraps");
        Ok(())
    }

    #[test]
    fn a_binding_verifies_the_callers_expectations_against_the_receipt(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let binding = MailBinding::new(receipt(TakeoverReceipt::first_open(
            BindingGeneration::FIRST,
        ))?);
        assert_eq!(binding.verify_expectations(&request()?), Ok(()));
        Ok(())
    }

    #[test]
    fn a_wrong_recipient_project_or_protocol_refuses_before_any_mail(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let binding = MailBinding::new(receipt(TakeoverReceipt::first_open(
            BindingGeneration::FIRST,
        ))?);

        let wrong_recipient = OpenRequest::new(
            TargetProjectId::new("secode").ok_or("target")?,
            SeatRecipientId::new("seat-gamma").ok_or("recipient")?,
            ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
            MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
        );
        assert_eq!(
            binding.verify_expectations(&wrong_recipient),
            Err(OpenFailure::RecipientMismatch)
        );

        let wrong_project = OpenRequest::new(
            TargetProjectId::new("stateengine").ok_or("target")?,
            SeatRecipientId::new("seat-delta").ok_or("recipient")?,
            ProtocolVersion::new(crate::REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?,
            MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
        );
        assert_eq!(
            binding.verify_expectations(&wrong_project),
            Err(OpenFailure::ForeignProjectMismatch)
        );

        let wrong_protocol = OpenRequest::new(
            TargetProjectId::new("secode").ok_or("target")?,
            SeatRecipientId::new("seat-delta").ok_or("recipient")?,
            ProtocolVersion::new("remote-seat-mail-transport/v2").ok_or("protocol")?,
            MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
        );
        assert_eq!(
            binding.verify_expectations(&wrong_protocol),
            Err(OpenFailure::ProtocolVersionUnsupported)
        );
        Ok(())
    }

    #[test]
    fn the_receipt_carries_both_project_halves_even_across_projects(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let binding = MailBinding::new(receipt(TakeoverReceipt::first_open(
            BindingGeneration::FIRST,
        ))?);
        let projects = binding.receipt().projects();
        assert_eq!(projects.source().as_str(), "stateengine");
        assert_eq!(projects.target().as_str(), "secode");
        assert!(!projects.is_same_project());
        Ok(())
    }
}
