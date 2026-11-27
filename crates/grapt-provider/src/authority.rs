//! The provider request/authority seam, and the record of what the earlier
//! attempt built here, why each attempt was wrong, and what its owner owed.
//!
//! This module was DOCS ONLY until the carrier seam landed. The rule it carried
//! — no types, no exports, no stub, because a ceremonial API with no owner
//! reads as coverage — held for exactly as long as there was no owner. The seam
//! is that owner now, so the code below is the seam the record specifies. The
//! record is kept above it, because a deletion rationale that survives only
//! until someone rebuilds the thing is worthless to the next owner that has to
//! know why the first four attempts failed.
//!
//! The foundational boundary owns identities and closed vocabularies,
//! compile-fail ownership probes and dependency-direction checks. Live request
//! validation, receipt fusion and provider effect execution are this seam's.
//!
//! # What was deleted, and why each reason is a real defect
//!
//! `PreparedPhysicalAttemptRequest` and `PreparedRequestError` were a live
//! implementation of this seam. Five defects, each independently fatal:
//!
//! 1. **It DESTROYED the authority it claimed to fuse.** `prepare` consumed the
//!    only non-`Clone` `ResolvedModelTarget` issuance and then exposed nothing but
//!    the attempt id and the protocol family. No consumer could ever recover
//!    the request, the target or the receipt from it, so the one issuance the
//!    registry will eventually mint went in and nothing usable came out.
//! 2. **The receipt was dead storage.** It was held, never read, and kept alive by
//!    an `#[allow(dead_code)]`. A suppression is not a design.
//! 3. **The derived `Debug` defeated the opacity.** It printed the full
//!    model-visible request — messages, tool definitions, tool choice — and the
//!    receipt's canonical bytes. Every other type on this boundary had its
//!    formatter hand-written or removed for exactly this reason; this one was
//!    restored to shrink a diff, which is not a reason.
//! 4. **The fusion was not enforced at all.**
//!    `PhysicalAttemptAuthoritySnapshot::receipt` is PUBLIC, so a caller derives
//!    and clones the receipt BEFORE calling `prepare` and holds it beside anything
//!    it likes. Removing the binding's accessors closed one route to a pair that
//!    was already obtainable by another. That defect is what retired probes
//!    50 and 50b: they witnessed that two accessors were gone, which was true and
//!    did not amount to the law they were credited with.
//! 5. **The receipt it fused was endpoint-only anyway** — see the deferral note in
//!    `grapt_policy::contributor`.
//!
//! Probes `10`, `50` and `50b` went with the type. Nothing replaced any of it.
//! What stays in this crate is the provider-family request DTOs in
//! [`crate::request`]: named per-protocol types whose field sets are the law
//! (a Codex request has no `max_output_tokens` to strip), which is the earlier
//! attempt's vocabulary.
//!
//! # The synchronous design's counterexamples, kept because the live seam must
//! answer them
//!
//! A synchronous `dispatch`/lease/completion seam was once built here and claimed
//! to satisfy master §8.3 — "one governed entry carrying a non-clone authority
//! lease through effect completion, every terminal class committing a typed
//! governed receipt before the owning attempt is released." It did not:
//!
//! 1. **The error path released everything with no receipt.** An adapter could
//!    `drop(effect)` and return `Err`; the caller's `?` then dropped the attempt,
//!    target, receipt and lease with no completion, no disposition and no terminal
//!    receipt. A seam that guards only its happy path guards nothing.
//! 2. **A synchronous completion cannot bound an asynchronous effect.** The
//!    outcome was allowed to be a stream handle, so an adapter could close the
//!    effect the instant the stream opened — before stream terminality, before
//!    cancellation, and before any durable terminal commit. "Completion" named the
//!    wrong moment.
//! 3. **Removing `Clone` from the request was cosmetic.** Concrete per-route
//!    requests keep public fields of `Clone`/`Copy` types, so anyone holding a
//!    borrow reconstructs an identical owned request field by field. Lending a
//!    request beside its receipt is the defect; a trait bound is not what closes
//!    it. `Clone` was therefore restored.
//! 4. **The receipt could not be an exact resolved-target receipt.** It bound the
//!    catalog VERSION, which is audit text: a repeated load mints a DISTINCT
//!    snapshot, so the same version and endpoint carrying different
//!    `base`/`family`/capability encode identically. Calling that an "unreachable
//!    ambiguity" described admission, not what a receipt asserts.
//!
//! # What the live seam owes
//!
//! The first positive executable seam must retain the exact target, the
//! authority, the request and the lease through a durable typed terminal commit.
//! Concretely:
//!
//! - a SEALED ASYNC dispatcher selected from a registry-issued
//!   [`grapt_types::ResolvedModelTarget`], which the adapter receives and cannot
//!   re-select;
//! - terminalization owned by the attempt aggregate, holding the lease through the
//!   durable terminal commit rather than handing release to the adapter — on the
//!   error and cancellation paths as well as the clean one;
//! - no public lending of a request together with its receipt, and no public
//!   receipt derivation that lets the pair be assembled before the seam is
//!   entered;
//! - a registry-MINTED opaque or keyed exact-target receipt that leaks no model
//!   family, base model, capability canonical form or secret material.
//!
//! # The witnesses this left absent, and how they came back
//!
//! Nine `prepared_request` scenarios were preserved verbatim and hash-locked in
//! `tests/pending/`, blocked on BOTH the registry issuance path and the live
//! seam, and jointly owned by the model-registry/config and provider-adapter
//! lanes.
//!
//! Both blockers are discharged. `ModelRegistry` landed, which is the route by
//! which a component can hold a `ResolvedModelTarget` at all — the exact thing
//! the manifest's `why_pending` named. This crate built the seam. The nine now
//! live in `authority::tests` and the manifest, its eleven bodies, the
//! validator, its fragments and `PENDING_MODEL_REGISTRY_WITNESSES.md` are
//! deleted, per the retirement rule that a witness is restored and its row
//! deleted in one change.
//!
//! **They were re-authored, not restored.** Their preamble imported
//! `PreparedPhysicalAttemptRequest`, `PreparedRequestError` and `ProviderRequest`,
//! all deleted with the seam they tested, so the bodies could not compile as
//! bytes however faithfully they were preserved. Each scenario's CLAIM survives;
//! its code does not. A hash-locked record of bodies that reference deleted types
//! can only ever be re-authored, and calling that a verbatim restoration would
//! have been false on its face.
//!
//! The old manifest required these witnesses before registry acceptance, but
//! they also depended on this live seam. Registry issuance alone could not
//! restore them; both pieces had to exist before the scenarios could execute.
//!
//! # What this seam delivers, and what it deliberately leaves to the runtime
//!
//! This seam takes three of the four obligations above: the sealed selection
//! from a registry-issued target, the refusal to lend a request beside its
//! receipt, and an exact-target binding that leaks no family, base model,
//! capability form or secret.
//!
//! It does NOT take *"terminalization owned by the attempt aggregate, holding the
//! lease through the durable terminal commit"*. That moves ownership into
//! `grapt-runtime`, whose attempt aggregate owns terminalization and answers
//! the synchronous design's counterexamples 1 and 2. Nothing below releases a
//! lease or claims a terminal.

use core::fmt;

use grapt_policy::{PhysicalAttemptAuthoritySnapshot, ToolName};
use grapt_types::{
    AccountId, CatalogGeneration, CredentialVersion, ModelRef, ModelRouteId, PhysicalAttemptId,
    ResolvedModelTarget,
};

/// An endpoint issued for exactly one route.
///
/// The route is a property of the ISSUANCE, not of the URL: two backends can
/// serve the same host and differ in route, and a base string cannot be parsed
/// back into the route it was minted for. An adapter that implements this trait
/// is asserting that its endpoint was minted knowing the route, which is what
/// makes `ProviderAttemptCarrier::bind`'s second check meaningful rather than
/// a string comparison wearing a type.
pub trait RouteBoundEndpoint {
    fn route(&self) -> &ModelRouteId;
}

/// A credential lease that names the account and credential version it carries,
/// and never the secret.
pub trait AccountBoundLease {
    fn account(&self) -> &AccountId;
    fn credential_version(&self) -> &CredentialVersion;
}

/// A provider request that names the model it will be sent for.
///
/// The master requires one request type per wire family, so this is the only
/// property the carrier can ask of all of them in common.
pub trait TargetedRequest {
    fn model(&self) -> &ModelRef;

    /// Every tool name this request offers the provider, in offer order.
    ///
    /// The carrier checks each against the authority's visible set. A request
    /// that offers nothing returns an empty slice and passes trivially, which is
    /// correct: the law is that an OFFERED tool must be visible, not that a tool
    /// must be offered.
    fn offered_tools(&self) -> Vec<String>;

    /// The tool the request demands or names, if it constrains the choice.
    ///
    /// `None` means the provider may choose freely among the offered tools.
    fn tool_choice(&self) -> RequestToolChoice;
}

/// What a request asks of tool selection, in the vocabulary the carrier checks.
///
/// Deliberately not the adapter's own `ToolChoice`: each wire family spells this
/// differently and the carrier must ask one question of all of them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RequestToolChoice {
    /// No constraint.
    Unconstrained,
    /// The provider must call some tool, but the request does not say which.
    RequiresSome,
    /// The provider must call this exact tool.
    RequiresNamed(String),
}

/// What the attempt aggregate RETAINS while the adapter holds the wire.
///
/// # Why this exists, and why this seam rather than the runtime builds it
///
/// `CodexAdapter::open` consumes its input by value and returns a stream or a
/// failure fragment; the target and the lease go in and nothing comes back. The
/// runtime side is sealed the same way — an attempt is admitted as a closure
/// whose `Debug` prints `<sealed>` — so an aggregate that must hold custody
/// through a durable terminal commit can observe neither.
///
/// The terminalization preflight found the gap while this seam was still open
/// — the only moment it could be fixed without a signature change after
/// landing. So the carrier now yields custody to the caller BEFORE the adapter
/// takes the rest.
///
/// # It follows the §8.3 precedent rather than inventing a held lease
///
/// `run_authorized` does not carry an `AuthorityLease` through terminal — it
/// consumes the lease at entry and carries the resulting identity. Custody is
/// the same shape: the adapter consumes the credential lease, and what survives
/// into the terminal commit is an identity that can be checked and receipted,
/// never secret material.
///
/// # What it deliberately cannot answer
///
/// No model family, no base model, no capability canonical form, no secret, and
/// no `ModelRef`. It answers only which attempt this is, which issuance it ran
/// under, and whether that issuance is still current — which is what a terminal
/// receipt must assert and the most a holder may learn.
#[must_use = "custody is what the attempt aggregate retains to receipt a terminal; dropping it silently releases everything with no receipt"]
pub struct AttemptCustody {
    attempt: PhysicalAttemptId,
    authority: PhysicalAttemptAuthoritySnapshot,
}

impl AttemptCustody {
    /// Which attempt this custody belongs to.
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// The generation the target was issued at. An ordinal, not a coordinate.
    pub fn issuance_generation(&self) -> CatalogGeneration {
        self.authority.model().issuance_generation()
    }

    /// Whether the issuance this attempt runs under is still current.
    ///
    /// The aggregate asks this at the terminal commit. An attempt that outlived
    /// its release is a fact the receipt must record, and it is exactly the
    /// question that only became askable when the registry made a release retirable.
    pub fn issuance_is_current(&self) -> bool {
        self.authority.model().is_current_issuance()
    }

    /// Whether this custody covers THIS attempt running against THAT target.
    ///
    /// Returns a BOOLEAN so the comparison runs without either sealed coordinate
    /// becoming readable — the shape the registry used for `declares_same_catalog_facts`.
    ///
    /// The attempt is a required argument rather than an optional courtesy. Two
    /// `ModelRegistry::issue` calls on one un-superseded registry for the same
    /// coordinates yield two DISTINCT targets that are nonetheless the same
    /// issuance, because both clone the one `CatalogSnapshot` whose identity is
    /// `Arc::ptr_eq`. An issuance-only predicate would therefore answer `true`
    /// for a sibling attempt's target, and a checker that asked only that would
    /// be answered yes for the wrong attempt. Taking the attempt makes the
    /// misuse unspellable instead of documented.
    pub fn covers(&self, attempt: &PhysicalAttemptId, target: &ResolvedModelTarget) -> bool {
        self.attempt == *attempt && self.authority.model().is_same_issuance(target)
    }
}

impl fmt::Debug for AttemptCustody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AttemptCustody")
            .field("attempt", &self.attempt)
            .field("issuance", &self.authority.model().issuance_generation())
            .finish_non_exhaustive()
    }
}

/// Why a carrier refused to bind its four coordinates into one attempt.
///
/// Closed and one variant per binding, so a caller reports which coordinate
/// disagreed rather than "authority mismatch". None of these says the target is
/// unavailable or incapable: every one of them says THIS request, endpoint or
/// lease does not belong to THIS issuance, which is a statement about the
/// pairing and never a licence to select something else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CarrierRefusal {
    /// The issuance the target was minted at is no longer current. Distinct from
    /// unavailable and incapable per master §7.5: the target is present and fit,
    /// and only its issuance is stale.
    StaleIssuance { issued_at: u64, current: u64 },
    /// The request names a different model than the target resolves.
    RequestNamesAnotherModel,
    /// The request names the same model TEXT as the target but was vouched for
    /// by a different catalog issuance.
    ///
    /// Distinct from [`Self::RequestNamesAnotherModel`] on purpose: the
    /// coordinates match and only the authority differs, so a caller reading
    /// "another model" would go looking for a typo that is not there.
    RequestFromAnotherCatalog,
    /// The endpoint was issued for a different route.
    EndpointServesAnotherRoute,
    /// The lease carries a different account.
    LeaseCarriesAnotherAccount,
    /// The lease carries a different credential version.
    LeaseCarriesAnotherCredentialVersion,
    /// The request offers a tool the authority does not make visible — either
    /// marked not visible, or absent from the authority altogether. Both are one
    /// refusal because both mean the same thing to a caller: this attempt may not
    /// offer that tool.
    HiddenTool { name: String },
    /// The request offers the same tool name twice, or offers an empty name.
    /// A duplicate is not merely untidy: the provider would see two definitions
    /// under one name and the authority check would pass for whichever was
    /// examined.
    MalformedToolName { name: String },
    /// The request names a tool it does not itself offer.
    ToolChoiceNotOffered { name: String },
    /// The request requires a tool call while offering no visible tool, which is
    /// unsatisfiable by construction rather than at the provider.
    ToolChoiceUnsatisfiable,
}

/// One physical attempt's four coordinates, bound together and inseparable.
///
/// # Why a struct and not four arguments
///
/// The deleted `PreparedPhysicalAttemptRequest` failed because it consumed the
/// only non-`Clone` issuance and returned nothing usable, and because the pair it
/// claimed to fuse was already obtainable another way. This type answers both:
/// it RETAINS everything it consumed, and it is the only route to the adapter, so
/// there is no second way to assemble the pair.
///
/// # It lends the request only to the crate that will serialize it
///
/// The synchronous design's counterexample 3 recorded that removing `Clone`
/// from the request was cosmetic, because concrete per-route requests keep
/// public fields of `Clone` types and a holder rebuilds an identical request
/// field by field. The law is therefore not a trait bound. It is that **the
/// request is never lent beside a receipt**: there is no public accessor for
/// either, and `Self::into_parts` is crate-private, so only the adapter that
/// serializes bytes can decompose one.
///
/// A caller may still build any request it likes. It cannot get that request to a
/// provider except through `Self::bind`, which refuses every request whose model
/// is not the one the registry issued.
#[allow(
    missing_debug_implementations,
    reason = "it owns a lease and a model-visible request; a derived formatter would print both"
)]
pub struct ProviderAttemptCarrier<E, L, R> {
    attempt: PhysicalAttemptId,
    authority: PhysicalAttemptAuthoritySnapshot,
    endpoint: E,
    lease: L,
    request: R,
}

impl<E, L, R> ProviderAttemptCarrier<E, L, R>
where
    E: RouteBoundEndpoint,
    L: AccountBoundLease,
    R: TargetedRequest,
{
    /// Bind four coordinates to one issuance, or refuse naming the one that
    /// disagreed.
    ///
    /// The target is consumed BY VALUE. `ResolvedModelTarget` is not `Clone`, so
    /// a caller that binds an issuance no longer holds it, and one issuance
    /// cannot ride two attempts.
    ///
    /// The staleness check is first and deliberately so. It is the check that
    /// only exists because the registry made an issuance retirable, and running it after
    /// the coordinate checks would report a coordinate disagreement for a target
    /// whose real defect is that its release was withdrawn.
    // Crate-private for the same reason `into_parts` is. A `pub` constructor
    // whose only recovery path is `pub(crate)` would let an external caller
    // consume the non-`Clone` issuance and get back nothing but `attempt()` —
    // the same defect the deleted `PreparedPhysicalAttemptRequest` had, resurrected.
    // The registry-issued target enters this crate through the adapter that
    // serializes it, and nothing else needs to build one. The production
    // caller the suppression promised is `crate::opener::AttemptOpener`,
    // so the suppression is deleted exactly as its removal condition said.
    pub(crate) fn bind(
        attempt: PhysicalAttemptId,
        authority: PhysicalAttemptAuthoritySnapshot,
        endpoint: E,
        lease: L,
        request: R,
    ) -> Result<Self, CarrierRefusal> {
        let target = authority.model();
        if !target.is_current_issuance() {
            return Err(CarrierRefusal::StaleIssuance {
                issued_at: target.issuance_generation().get(),
                current: target.current_catalog_generation().get(),
            });
        }
        // `!=` is NOT sufficient here and using it was a real defect. `ModelRef`'s
        // `PartialEq` compares `record.coordinates()` only and is deliberately
        // provenance-blind — its own note says so — so a rogue catalog loaded
        // through the PUBLIC `ModelCatalog::load` with a row spelling the same
        // provider/model/variant produces a `ModelRef` that compares EQUAL to the
        // genuine one. That is precisely
        // `a_request_from_another_catalog_cannot_ride_a_genuine_authority`, and
        // the deleted `PreparedPhysicalAttemptRequest` refused it. A replacement
        // weaker than what it replaced is not a replacement.
        //
        // The asymmetry below is deliberate: route, account and credential
        // version are inert `opaque_id!` values with no stronger predicate
        // available, so `!=` is the right question for those three. `ModelRef` is
        // the one coordinate that offers a strictly stronger public form.
        if !request.model().is_same_issuance(target.model_ref()) {
            return Err(if request.model() == target.model_ref() {
                CarrierRefusal::RequestFromAnotherCatalog
            } else {
                CarrierRefusal::RequestNamesAnotherModel
            });
        }
        if endpoint.route() != target.route() {
            return Err(CarrierRefusal::EndpointServesAnotherRoute);
        }
        if lease.account() != target.account() {
            return Err(CarrierRefusal::LeaseCarriesAnotherAccount);
        }
        if lease.credential_version() != target.credential_version() {
            return Err(CarrierRefusal::LeaseCarriesAnotherCredentialVersion);
        }

        // Tools are the other half of the authority and the half the routing
        // tuple cannot answer. A target says which model may be called; only the
        // effective graph says which tools that call may offer.
        let offered = request.offered_tools();
        let mut seen: Vec<&str> = Vec::with_capacity(offered.len());
        for name in &offered {
            if name.trim().is_empty() || seen.contains(&name.as_str()) {
                return Err(CarrierRefusal::MalformedToolName { name: name.clone() });
            }
            seen.push(name.as_str());
            let Some(tool) = ToolName::new(name.clone()) else {
                return Err(CarrierRefusal::MalformedToolName { name: name.clone() });
            };
            // Not visible and not listed are one refusal on purpose: to a caller
            // both mean this attempt may not offer that tool, and distinguishing
            // them would report whether a tool exists in an authority the caller
            // was not shown.
            if !authority.tool_is_visible(&tool) {
                return Err(CarrierRefusal::HiddenTool { name: name.clone() });
            }
        }

        match request.tool_choice() {
            RequestToolChoice::Unconstrained => {}
            RequestToolChoice::RequiresSome => {
                if offered.is_empty() {
                    return Err(CarrierRefusal::ToolChoiceUnsatisfiable);
                }
            }
            RequestToolChoice::RequiresNamed(name) => {
                if !offered.contains(&name) {
                    return Err(CarrierRefusal::ToolChoiceNotOffered { name });
                }
            }
        }

        Ok(Self {
            attempt,
            authority,
            endpoint,
            lease,
            request,
        })
    }

    /// The attempt this carrier was bound for. Inert, and the only coordinate
    /// safe to publish: it names the attempt without describing the route.
    pub fn attempt(&self) -> &PhysicalAttemptId {
        &self.attempt
    }

    /// Split into the custody the AGGREGATE retains and the parts the adapter
    /// serializes.
    ///
    /// Crate-private on purpose. This is the single point at which the request
    /// stops being bound to its target, and it is reachable only from the adapter
    /// that turns it into bytes — never from a caller that could hold the request
    /// beside anything else.
    ///
    /// # What the split does and does not do
    ///
    /// An earlier note here claimed custody "leaves by a different door" from the
    /// wire parts. That was false: this returns ONE tuple to ONE caller, and so
    /// does `from_carrier`. Whoever calls either holds custody beside the
    /// request, and nothing structural separates them.
    ///
    /// What the split actually buys is that the aggregate has SOMETHING to hold
    /// at all — before it, the target and lease entered `open` by value and
    /// nothing came back, so no terminal could be receipted. That is the real
    /// gain and it is enough for the runtime terminalization.
    ///
    /// What it also does, and this is a widening rather than a seal: it carries a
    /// live `ResolvedModelTarget` out of the carrier inside a public type, so a
    /// holder can ask `issuance_is_current` and `covers` indefinitely. Nothing
    /// outside the carrier could ask those before. The questions are safe — see
    /// the `AttemptCustody` note — but the widening is real and is recorded here
    /// rather than left for a reader to discover.
    ///
    /// # The law that request-and-receipt are never lent together rests on
    /// `CodexAttemptInput`, not on this split
    ///
    /// It holds because `CodexAttemptInput` exposes no accessor for its request,
    /// endpoint or lease — not because `into_parts` is crate-private. Adding one
    /// such accessor later breaks the law with no change to this file.
    /// A future consumer that needs the request back should make the pair
    /// structurally exclusive — hand custody to a distinct owner — rather than
    /// rely on that type staying accessor-free.
    ///
    /// The production caller the `dead_code` suppression named is
    /// `CodexAttemptInput::from_carrier`, now reached from
    /// `crate::opener::AttemptOpener::open`; the suppression is deleted exactly
    /// as its removal condition said.
    pub(crate) fn into_parts(self) -> (AttemptCustody, E, L, R) {
        let custody = AttemptCustody {
            attempt: self.attempt,
            authority: self.authority,
        };
        (custody, self.endpoint, self.lease, self.request)
    }
}

#[cfg(test)]
mod tests;
