//! Subagent operation identity, addressing and model-routing preference.
//!
//! Law (`CORRECT`, amendment §7.4-§7.5 and survey delta §3): the operation is
//! the authoritative address, not a child SessionId and not a reusable display
//! label. Routing is provider-neutral: `agent default` inherits the parent's
//! exact resolved target, a cheaper capability-compatible tier is permitted
//! only inside the same family for bounded low-risk work, and a
//! cross-family/cross-provider child requires the user's explicit Subagent
//! model selection.
//!
//! Amendment §7.5, verbatim: "The GUI Subagent model selector (and exactly
//! equivalent explicit user API/CLI input) is the authority for
//! cross-family/provider routing ... no project policy, catalog, adapter or
//! parent model may cross the model-family or provider boundary while `agent
//! default` is selected ... an explicit user target is snapshotted at child
//! admission."
//!
//! # A selection is authority, not data
//!
//! Because the user's selection is the ONE thing that may cross the boundary,
//! a public value anything can write is a full bypass: an adapter, a policy
//! layer or the parent model could simply say "the user picked this" and route
//! anywhere. So [`SubagentModelPreference::UserSelected`] no longer carries
//! loose fields. It carries a [`UserModelSelection`], which has private fields,
//! no public constructor, and exactly one producer:
//! [`UserSelectionAuthority::select`].
//!
//! That trait is **sealed** — no foreign crate can declare itself the selection
//! authority — and its only implementation is [`UserSelectionSurface`], which
//! the settings/transport owner holds. [`SubagentModelPreference::AgentDefault`]
//! stays freely constructible, because it is not authority; it is the absence
//! of it.

use crate::model::CatalogReissuanceReceipt;
use crate::model::ModelRef;
use crate::transport::{AuthenticatedClientId, DecisionNonce, RequestId};
use core::fmt;

crate::id::opaque_id! {
    /// The durable address of one subagent operation.
    ///
    /// Distinct from a child `SessionId`: the oracle's table keys the operation
    /// by child session id, which is the defect this type removes.
    SubagentOperationId
}

crate::id::opaque_id! {
    /// Identity of one tool call.
    ToolCallId
}

crate::id::opaque_id! {
    /// Content-addressed identity of one immutable effective-authority graph.
    EffectiveGraphId
}

crate::id::opaque_id! {
    /// A human-readable, reusable subagent label.
    ///
    /// Never authority on its own: a label plus a generation is an address.
    SubagentLabel
}

crate::id::opaque_id! {
    /// Version of the user-facing selector that produced a preference.
    SelectorVersion
}

/// Monotonic generation binding a reusable label to one live operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SubagentGeneration(u64);

impl SubagentGeneration {
    pub const FIRST: SubagentGeneration = SubagentGeneration(0);

    pub fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn value(self) -> u64 {
        self.0
    }

    pub fn next(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }
}

/// A routable subagent address.
///
/// Law F2: reusable human labels are non-authoritative unless they carry a
/// generation. Two operations reusing one label at different generations are
/// different addresses, so a message cannot be delivered to a retired
/// recipient that happened to share a name.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SubagentAddress {
    operation: SubagentOperationId,
    label: Option<SubagentLabel>,
    generation: SubagentGeneration,
}

impl SubagentAddress {
    pub fn new(
        operation: SubagentOperationId,
        label: Option<SubagentLabel>,
        generation: SubagentGeneration,
    ) -> Self {
        Self {
            operation,
            label,
            generation,
        }
    }

    /// The authoritative identity.
    pub fn operation(&self) -> &SubagentOperationId {
        &self.operation
    }

    pub fn label(&self) -> Option<&SubagentLabel> {
        self.label.as_ref()
    }

    pub fn generation(&self) -> SubagentGeneration {
        self.generation
    }
}

mod sealed {
    /// Private supertrait. Declared in a private module, so no crate outside
    /// this one can name it, and therefore no foreign type can satisfy the
    /// public traits that require it.
    pub trait Sealed {}
}

/// The authority that may originate a user model selection.
///
/// Sealed on purpose. Amendment §7.5 makes the GUI Subagent model selector —
/// or exactly equivalent explicit user API/CLI input — the *only* authority for
/// cross-family/provider routing, so "who may say the user selected something"
/// is itself a closed question. A foreign crate cannot implement this trait,
/// and its only implementation is [`UserSelectionSurface`].
///
/// [`UserSelectionAuthority::select`] is the only producer of a
/// [`UserModelSelection`] anywhere in the workspace.
pub trait UserSelectionAuthority: sealed::Sealed + fmt::Debug {
    /// The transport-authenticated client the selection came from. The
    /// transport supplies this; request JSON cannot.
    fn authenticated_client(&self) -> &AuthenticatedClientId;

    /// The version of the user-facing selector that produced the selection.
    fn selector_version(&self) -> &SelectorVersion;

    /// Snapshot one authenticated selection of `target`.
    ///
    /// The client and selector version are taken from this authority rather
    /// than from arguments, so a caller cannot attribute a selection to a
    /// client that did not make it. The request id and single-use nonce bind
    /// the selection to the one user interaction that carried it.
    fn select(
        &self,
        request: RequestId,
        nonce: DecisionNonce,
        target: ModelRef,
    ) -> UserModelSelection {
        UserModelSelection {
            client: self.authenticated_client().clone(),
            request,
            nonce,
            selector: self.selector_version().clone(),
            target,
        }
    }
}

/// The settings/transport-owned selector surface for one authenticated client.
///
/// Not `Clone`: a selector surface is live authority held by one owner, not a
/// value to scatter through the call graph.
///
/// SEALED, and the distinction matters. An earlier revision of this note said
/// that restricting who calls `establish` "belongs to the settings/transport
/// owner" while leaving the route public. That is an external OBLIGATION, and an
/// obligation is not a seal: the reviewer's finding was that the whole selection
/// authority stayed reachable through it, because both of its arguments are
/// opaque ids with public constructors.
///
/// So the route is crate-private now and there is no public way to obtain a
/// surface. This crate still cannot prove a human clicked — nothing in a
/// dependency-free types crate can — but the failure is closed rather than
/// delegated: what it cannot prove, it does not let anyone assert.
///
/// What it does prove: a selection came from a [`UserSelectionAuthority`], no
/// foreign crate can be one, the selection binds an authenticated client,
/// request and nonce, and it is single-use. The settings/transport lane that
/// owns the GUI Subagent model selector — and issues the
/// [`AuthenticatedClientId`] in the first place — is the named later owner of
/// the live route.
#[derive(Debug)]
pub struct UserSelectionSurface {
    client: AuthenticatedClientId,
    selector: SelectorVersion,
}

impl UserSelectionSurface {
    /// Establish the selector surface for one authenticated client.
    /// SEALED. Establishing the surface was public, which made the whole
    /// selection authority reachable: `AuthenticatedClientId` and
    /// `SelectorVersion` are opaque ids with public constructors, so a caller
    /// could author both, establish a surface, and call the trait's public
    /// `select` to mint a `UserModelSelection` — the one value amendment
    /// section 7.5 lets cross the model-family and provider boundary while
    /// `agent default` is chosen.
    ///
    /// Sealing the TRAIT was not enough and is worth stating plainly, because it
    /// is the mistake this correction fixes: a sealed trait stops a foreign type
    /// *implementing* it, and does nothing about *calling* the public methods of
    /// the implementation that already exists. Probe 26 proved the first and was
    /// read as proving the second.
    ///
    /// LATER-LANE DEPENDENCY: the owner is the settings/transport lane that owns
    /// the GUI Subagent model selector and issues the authenticated client
    /// identity in the first place. Until it exists there is no route to a
    /// surface, so `select` is unreachable and cross-family authority is
    /// unnameable.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn establish(client: AuthenticatedClientId, selector: SelectorVersion) -> Self {
        Self { client, selector }
    }
}

impl sealed::Sealed for UserSelectionSurface {}

impl UserSelectionAuthority for UserSelectionSurface {
    fn authenticated_client(&self) -> &AuthenticatedClientId {
        &self.client
    }

    fn selector_version(&self) -> &SelectorVersion {
        &self.selector
    }
}

/// One authenticated user model selection, snapshotted at child admission.
///
/// Fields are private and there is no public constructor: the only route is
/// [`UserSelectionAuthority::select`]. Not `Clone` and not `Copy` — one
/// authenticated selection admits one child, and a preference that could be
/// duplicated would let a single click authorize an unbounded number of
/// cross-family children.
#[derive(Debug, PartialEq, Eq)]
pub struct UserModelSelection {
    client: AuthenticatedClientId,
    request: RequestId,
    nonce: DecisionNonce,
    selector: SelectorVersion,
    target: ModelRef,
}

impl UserModelSelection {
    /// The authenticated client that made the selection.
    pub fn client(&self) -> &AuthenticatedClientId {
        &self.client
    }

    /// The request the selection arrived on.
    pub fn request(&self) -> &RequestId {
        &self.request
    }

    /// The single-use nonce binding the selection to one user interaction.
    pub fn nonce(&self) -> &DecisionNonce {
        &self.nonce
    }

    /// The selector version that produced the selection.
    pub fn selector(&self) -> &SelectorVersion {
        &self.selector
    }

    /// The exact target the user selected. Not a family of targets.
    pub fn target(&self) -> &ModelRef {
        &self.target
    }

    /// Carry this selection across a semantically-null catalog reload.
    ///
    /// # The defect this closes
    ///
    /// `ModelCatalog::load` mints a fresh snapshot on every call, even for a
    /// byte-identical release, so an ordinary config reload — including one the
    /// user did not cause, from a file watch — made this selection's target a
    /// foreign issuance. `child_target_is_admissible` requires
    /// `is_same_issuance` for a `UserSelected` child, and this type is not
    /// `Clone` and carries a single-use nonce, so that reload did not refresh
    /// the selection: it destroyed it, and the user had to click again.
    ///
    /// Master §7.5: *"A semantically null reload does not make an issuance
    /// stale."*
    ///
    /// # Why the receipt alone is not enough, and is not trusted as if it were
    ///
    /// A [`CatalogReissuanceReceipt`] proves that SOME target carried forward
    /// between two generations. It names only the two generations, so on its own
    /// it would authorise carrying ANY selection across — the precise shape of a
    /// credential that answers *"was this ever issued"* instead of *"was this
    /// issued for this target"*. A value that verifies by mere possession binds
    /// nothing.
    ///
    /// So the binding is checked here, structurally, from data neither the
    /// caller nor the receipt can restate:
    ///
    /// 1. `reissued` must name the same inert coordinates as the target this
    ///    selection actually holds — `ModelRef`'s equality is exactly those
    ///    three, and it cannot answer a family question;
    /// 2. the receipt must START at the generation this selection was issued at;
    /// 3. `reissued` must SIT AT the generation the receipt carries into.
    ///
    /// The sealed half — same base model, same family — was already proved by
    /// `ModelRegistry::reissue`, which is the only minter of a receipt and
    /// refuses to produce one when either changed. Pairing a genuine receipt
    /// with a different target fails (1); replaying an old receipt against a
    /// later reload fails (3).
    ///
    /// It consumes `self`, so a carried selection cannot also remain live at the
    /// old generation. One selection stays one selection.
    ///
    /// # Surface-agnostic
    ///
    /// The client, request, nonce and selector version are carried through
    /// unchanged: this is the SAME authenticated interaction, still bound to the
    /// one click that made it, now naming the release that replaced the one it
    /// was made against. Nothing here is GUI-shaped, so a selection made from a
    /// TUI or CLI authenticated client carries forward identically.
    pub fn carry_forward(
        self,
        receipt: &CatalogReissuanceReceipt,
        reissued: ModelRef,
    ) -> Result<Self, CarryForwardRefusal> {
        if self.target != reissued {
            return Err(CarryForwardRefusal::NotThisTarget);
        }
        if receipt.from_generation() != self.target.issuance_generation() {
            return Err(CarryForwardRefusal::NotThisIssuance);
        }
        if receipt.to_generation() != reissued.issuance_generation() {
            return Err(CarryForwardRefusal::NotTheCarriedGeneration);
        }
        Ok(Self {
            client: self.client,
            request: self.request,
            nonce: self.nonce,
            selector: self.selector,
            target: reissued,
        })
    }
}

/// Why a selection could not be carried across a catalog reload.
///
/// Closed and typed because §7.5 requires it: *"The refusal is typed and
/// reportable; silent re-selection is not an available outcome."* Each variant
/// names which of the three bindings failed, so a caller reports the exact
/// mismatch rather than "could not carry".
///
/// None of these says the target is gone or unfit. They say this receipt does
/// not describe this carry, which is a statement about the pairing and never a
/// licence to select something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarryForwardRefusal {
    /// The re-issued target does not name the same coordinates as the selection.
    NotThisTarget,
    /// The receipt starts at a generation this selection was not issued at.
    NotThisIssuance,
    /// The re-issued target does not sit at the generation the receipt reaches.
    NotTheCarriedGeneration,
}

/// The user's subagent model preference.
///
/// Law §7.5: this is the *requested* preference, never proof of the route
/// actually used. The resolved target belongs to each PhysicalAttempt receipt.
///
/// Not `Clone`, because `UserSelected` carries single-use authority.
#[derive(Debug, PartialEq, Eq)]
pub enum SubagentModelPreference {
    /// Inherit the parent's exact resolved target, with same-family economy
    /// permitted only for bounded low-risk work.
    ///
    /// Freely constructible: `agent default` is the ABSENCE of cross-boundary
    /// authority, so there is nothing to seal.
    AgentDefault,
    /// An explicit user selection, snapshotted at child admission.
    ///
    /// The variant is nameable but uninhabitable without a
    /// [`UserModelSelection`], which only a [`UserSelectionAuthority`] mints.
    /// Boxed because a selection carries the full authenticated tuple and the
    /// selected target: `agent default` is the overwhelmingly common
    /// preference and must not pay for the rare one.
    UserSelected(Box<UserModelSelection>),
}

impl SubagentModelPreference {
    /// Snapshot an authenticated selection at child admission.
    ///
    /// Consumes the selection: one authenticated selection admits one child.
    pub fn user_selected(selection: UserModelSelection) -> Self {
        SubagentModelPreference::UserSelected(Box::new(selection))
    }

    /// The authenticated selection behind this preference, if any.
    pub fn selection(&self) -> Option<&UserModelSelection> {
        match self {
            SubagentModelPreference::AgentDefault => None,
            SubagentModelPreference::UserSelected(selection) => Some(selection),
        }
    }

    /// The exact target the user selected, if any.
    pub fn selected_target(&self) -> Option<&ModelRef> {
        self.selection().map(UserModelSelection::target)
    }

    /// Whether this preference can authorize a cross-family or cross-provider
    /// child at all.
    ///
    /// `AgentDefault` never can — that is the whole boundary.
    pub fn permits_cross_family(&self) -> bool {
        self.selection().is_some()
    }
}

/// Why a child attempt resolved to the target it did.
///
/// Closed per law: every PhysicalAttempt receipts one of these, so "which
/// model actually ran and why" is never inferred from a marketing name.
///
/// A provenance value is a LABEL, not authority: `UserSelected` here means
/// nothing on its own, and [`DelegationSelectionProvenance::is_admissible_under`]
/// refuses it unless the preference carries a real [`UserModelSelection`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DelegationSelectionProvenance {
    /// The parent's exact resolved target was inherited.
    InheritedExact,
    /// A cheaper capability-compatible tier in the *same* family was chosen
    /// for bounded low-risk work under versioned harness policy.
    SameFamilyEconomy,
    /// The user's explicit Subagent model selection was used.
    UserSelected,
    /// Project policy narrowed the target.
    ProjectPolicyConstrained,
}

impl DelegationSelectionProvenance {
    /// Whether this provenance is admissible under `preference`.
    ///
    /// `UserSelected` provenance requires a user-selected preference: no
    /// adapter, catalog or policy can produce it. `SameFamilyEconomy` and
    /// `InheritedExact` belong to `agent default`.
    pub fn is_admissible_under(self, preference: &SubagentModelPreference) -> bool {
        match self {
            DelegationSelectionProvenance::InheritedExact
            | DelegationSelectionProvenance::SameFamilyEconomy => preference.selection().is_none(),
            DelegationSelectionProvenance::UserSelected => preference.selection().is_some(),
            DelegationSelectionProvenance::ProjectPolicyConstrained => true,
        }
    }
}

/// Whether a proposed child target is admissible.
///
/// This is the one place the family boundary is decided. A cross-family or
/// cross-provider target under `agent default` is refused *before* request
/// construction, so no provider bytes are sent.
/// SEALED. This is the admissibility DECISION behind amendment section 7.5,
/// and it is built on [`ModelRef::same_family`], which is now sealed as an
/// authority operation. Exposing the decision while its evidence is sealed would
/// be the same defect one level up.
///
/// LATER-LANE DEPENDENCY: the routing/policy owner, jointly with the model
/// registry that establishes which catalog is genuine.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn child_target_is_admissible(
    parent: &ModelRef,
    child: &ModelRef,
    preference: &SubagentModelPreference,
    provenance: DelegationSelectionProvenance,
) -> bool {
    if !provenance.is_admissible_under(preference) {
        return false;
    }

    // A user-selected child must be THE selected target, exactly. Policy
    // narrowing may refuse a selection but can never silently substitute a
    // different one, so equality here is mandatory rather than a family check.
    if let Some(selected) = preference.selected_target() {
        if provenance == DelegationSelectionProvenance::UserSelected {
            // Issuance, not spelling. `ModelRef` equality is now narrowed to
            // the inert endpoint coordinates — deliberately, so it cannot
            // answer a routing-authority question — which means a same-endpoint
            // ref vouched for by a catalog nobody loaded would otherwise
            // satisfy an explicit user selection. The user chose a target from
            // the real catalog; admit only that issuance of it.
            return child.is_same_issuance(selected);
        }
        // Any other provenance under an explicit selection must stay inside
        // the parent's family: it is not carrying the user's authority.
        return parent.same_family(child);
    }

    // `agent default`: never leaves the parent's provider or family.
    parent.same_family(child)
}

#[cfg(test)]
mod tests;
