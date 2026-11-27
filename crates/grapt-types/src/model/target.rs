//! [`ResolvedModelTarget`] and the canonical form its capability derives from.
//!
//! Why this type carries no derived traits, and why its capability accessor is
//! sealed, is stated once in the parent module and is not restated here.

use super::issuance::CatalogGeneration;
use super::reference::ModelRef;
use super::{ModelId, ModelRouteId, ProviderId};
use crate::authority::PolicyVersion;
use crate::capability::ModelTargetCapabilityId;
use crate::credential::{AccountId, CredentialVersion};

/// The exact resolved routing target a physical attempt runs against. Changing
/// the account, provider or model resolves a different target, which is a new
/// receipted `PhysicalAttempt`, never a silent swap. Credentials are referenced
/// by version only — the secret material is never carried here.
///
/// # Three coordinate sets, stated once because they are not the same set
///
/// An earlier revision of this note called the capability's subject "THIS whole
/// 5-tuple". That was false in two directions at once, and a lane that believed
/// it would have built the wrong thing:
///
/// 1. **Issuance** takes FIVE values — `(ModelRef, ModelRouteId, AccountId,
///    CredentialVersion, PolicyVersion)` — and stores six fields, the sixth
///    being the capability it derives.
/// 2. **The capability** derives from EIGHT coordinates and `PolicyVersion` is
///    NOT among them: provider, model, base model, family, variant presence,
///    variant, route, account and credential version, per
///    `canonical_target_form`.
/// 3. **Target sameness** compares FIVE fields and `PolicyVersion` IS among
///    them — see [`describes_same_target`](Self::describes_same_target).
///
/// So two targets differing only in `PolicyVersion` are different targets and
/// carry the SAME capability. That asymmetry is deliberate and ruled: master
/// §8.2 requires the settlement receipt to record "policy/effective graph
/// version" ALONGSIDE the capability, which is exactly what excluding it from
/// the capability means. `PolicyVersion` is a receipt and comparison
/// coordinate, never a capability coordinate.
///
/// # It is a proof, so it has no derived traits
///
/// This type is the model registry's statement that ONE physical attempt may
/// run against ONE target. Everything a derive would add is a way to defeat
/// that:
///
/// - **`Clone`** duplicates the proof. A reusable admission ticket is exactly
///   the defect: admission into an authority graph
///   (`grapt_policy::SnapshotBuilder::model`) consumes the target by value,
///   and without `Clone` a caller cannot keep a second copy to admit somewhere
///   else.
/// - **`Debug`** printed the whole tuple including the derived
///   [`ModelTargetCapabilityId`], whose canonical form is the literal text
///   `…|base=…|family=…|…`. That is not an oracle, it is a plaintext dump of
///   both sealed catalog facts. `Debug` is therefore ABSENT rather than
///   redacted, so the leak cannot come back through a field added later; the
///   inert coordinates remain printable through
///   [`model_ref`](Self::model_ref), whose own `Debug` is redacted.
/// - **`PartialEq`/`Eq`** compared the same tuple and were the same oracle as
///   `ModelRef`'s old row equality, one level up. The two questions a consumer
///   genuinely asks are separated instead:
///   [`describes_same_target`](Self::describes_same_target) over the inert
///   coordinates, and [`is_same_issuance`](Self::is_same_issuance) for exact
///   provenance. Neither can be made to answer a family question.
/// - **`Hash`** was never derived here and is not added.
#[allow(
    missing_debug_implementations,
    reason = "a formatter over an authority proof is a read channel; see the type note"
)]
pub struct ResolvedModelTarget {
    model: ModelRef,
    route: ModelRouteId,
    account: AccountId,
    credential_version: CredentialVersion,
    capability: ModelTargetCapabilityId,
    policy: PolicyVersion,
}

impl ResolvedModelTarget {
    /// Resolve a target and DERIVE its capability from the exact tuple.
    ///
    /// The capability is not a field a caller supplies — that would allow an
    /// unrelated capability to be attached to a target it does not describe.
    /// It is computed here from provider, model, base model, family, route,
    /// account and credential version, so changing any coordinate resolves a
    /// different target with a different capability, which is a new receipted
    /// `PhysicalAttempt` rather than a silent swap.
    /// SEALED for the same reason as [`ModelRef::same_family`]: this is the
    /// authority-producing step, because it DERIVES the
    /// `ModelTargetCapabilityId` the
    /// attempt runs under. Reachable from a rogue catalog it would mint
    /// capability identity for coordinates the attacker authored. The owner is
    /// the model registry/config lane.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn resolve(
        model: ModelRef,
        route: ModelRouteId,
        account: AccountId,
        credential_version: CredentialVersion,
        policy: PolicyVersion,
    ) -> Self {
        let capability = ModelTargetCapabilityId::from_canonical(canonical_target_form(
            &model,
            &route,
            &account,
            &credential_version,
        ));
        Self {
            model,
            route,
            account,
            credential_version,
            capability,
            policy,
        }
    }

    pub fn model_ref(&self) -> &ModelRef {
        &self.model
    }

    pub fn provider(&self) -> &ProviderId {
        self.model.provider()
    }

    pub fn model(&self) -> &ModelId {
        self.model.model()
    }

    // There are deliberately no `base_model()`/`family()` accessors here
    // either, and they are DELETED rather than made crate-private: nothing
    // calls them, and a registry-issued target is not a licence to read the
    // routing coordinates back out. Two genuine targets compared by hand still
    // lose the same-snapshot clause `ModelRef::same_family` enforces, so the
    // hand-rolled answer would be wrong in the one direction that matters — an
    // accidental `true`.

    pub fn route(&self) -> &ModelRouteId {
        &self.route
    }

    pub fn account(&self) -> &AccountId {
        &self.account
    }

    pub fn credential_version(&self) -> &CredentialVersion {
        &self.credential_version
    }

    /// SEALED. The capability derived from this exact target.
    ///
    /// A [`ModelTargetCapabilityId`] is NOT a digest. `canonical_target_form`
    /// builds the literal string
    /// `model-target|provider=…|model=…|base=…|family=…|variant_present=…|…`
    /// and `as_str` returns it unchanged, so a public accessor here was a
    /// plaintext read path to `base_model` and `family` — not an oracle needing
    /// a guess per bit, the values themselves. It survived the round that
    /// deleted `base_model()`/`family()` from this type because that round
    /// audited accessors by NAME.
    ///
    /// LATER-LANE DEPENDENCY: the model registry/config owner together with the
    /// permission/capability owner. A permission decision legitimately needs
    /// this identity, and it cannot be handed out while its canonical form
    /// spells the sealed facts. The structural fix belongs to that lane —
    /// content-address the model-target canonical form, or expose a projection
    /// that carries the endpoint coordinates and a digest of the rest. Sealing
    /// it here is the fail-closed interim, not the destination.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn capability(&self) -> &ModelTargetCapabilityId {
        &self.capability
    }

    pub fn policy(&self) -> &PolicyVersion {
        &self.policy
    }

    /// The catalog generation this target was issued at.
    ///
    /// Public, and safe to be: a generation is an ordinal saying which release
    /// vouched for the target, never which family it is in. It is the one
    /// coordinate of the issuance a consumer legitimately needs, because the
    /// alternative to reporting it is a bare refusal.
    pub fn issuance_generation(&self) -> CatalogGeneration {
        self.model.snapshot().generation()
    }

    /// Whether the issuance carrying this target is still the current one.
    ///
    /// The use-time half of the staleness law. Issue-time refusal alone is not
    /// enough: a target minted while generation *n* was current outlives the
    /// release it came from, because it holds a STRONG clone of that snapshot.
    /// Without this question, a release withdrawn for bad family data keeps
    /// authorising every target already minted from it for the life of the
    /// process.
    ///
    /// Master §7.5: staleness is a property of the ISSUANCE, not of the target.
    /// A `false` here says nothing about whether the target is present or fit,
    /// and it never licenses substituting another family — the caller's legal
    /// next step is to ask the current issuance for the same coordinates and
    /// carry the selection across with a
    /// [`CatalogReissuanceReceipt`](super::CatalogReissuanceReceipt).
    pub fn is_current_issuance(&self) -> bool {
        self.model.snapshot().is_current()
    }

    /// The generation this target's lineage currently considers live.
    ///
    /// Reported alongside [`issuance_generation`](Self::issuance_generation) so
    /// a refusal can name both halves of the comparison it failed. Also an
    /// ordinal, and also not a catalog fact.
    pub fn current_catalog_generation(&self) -> CatalogGeneration {
        self.model.snapshot().current_generation()
    }

    /// Whether `other` names the same target by its INERT coordinates: the same
    /// endpoint, route, account, credential version and policy version.
    ///
    /// This is the readable "is this even the same target" question a consumer
    /// asks before it reports a conflict. It compares nothing the catalog
    /// decided — no base model, no family, and not the derived capability,
    /// whose canonical form contains both. Two targets that agree here but come
    /// from different catalog releases are a FOREIGN resolution, not a
    /// different target, and [`is_same_issuance`](Self::is_same_issuance) is
    /// what separates them.
    pub fn describes_same_target(&self, other: &ResolvedModelTarget) -> bool {
        self.model == other.model
            && self.route == other.route
            && self.account == other.account
            && self.credential_version == other.credential_version
            && self.policy == other.policy
    }

    /// Whether `other` is the SAME issuance of the same target: the same row of
    /// the same loaded catalog snapshot, and the same inert coordinates.
    ///
    /// Safe to expose for the reason [`ModelRef::is_same_issuance`] records: it
    /// demands the rows be identical, so it answers `false` for exactly the
    /// pairs the family question is about — two DIFFERENT targets — and cannot
    /// be assembled into "may `agent default` cross to this one". It fails
    /// closed across catalogs: a target resolved from a release nobody loaded is
    /// never the same issuance as one the runtime resolved, however identical
    /// its text.
    pub fn is_same_issuance(&self, other: &ResolvedModelTarget) -> bool {
        self.model.is_same_issuance(&other.model) && self.describes_same_target(other)
    }
}

/// Deterministic canonical encoding of the exact resolved routing tuple.
///
/// Separators are escaped, and optional presence is encoded STRUCTURALLY, so
/// two distinct tuples cannot encode alike.
///
/// # Why presence is a label and not a sentinel value
///
/// This function used to write the absent variant as the literal value `-`.
/// `push` escapes `\`, `|` and `=`; it does not escape `-`, and
/// [`ModelVariantId`] accepts any non-empty string, so `ModelVariantId::new("-")`
/// succeeds. `(provider, model, None)` and `(provider, model, Some("-"))` are
/// DISTINCT lookup coordinates — [`ModelCatalogRecord::coordinates`] compares an
/// `Option`, so [`ModelCatalog::load`]'s contradiction check admits both rows in
/// one release, carrying different base models and different families — and both
/// encoded to the identical `variant=-|`. Two rows in different families derived
/// one capability.
///
/// The direction of that collision is what made it serious: it produced an
/// accidental EQUALITY, and equality is the authorization comparison, so it read
/// as "same authority" rather than as a refusal.
///
/// Presence is therefore a LABEL position, which no value can occupy. A variant
/// spelling `0`, `1` or `-` lands after `variant=`, never after
/// `variant_present=`, and the escape table keeps it from synthesising a field
/// boundary. Absent emits `variant_present=0|` with no `variant=` field; present
/// emits `variant_present=1|variant=…|`. The two shapes cannot coincide.
///
/// The catalog SNAPSHOT is deliberately absent: it is process-local live
/// authority, and a capability must derive identically in every process that
/// resolves the same target. The catalog VERSION is absent for a different
/// reason — the facts it vouches for, `base` and `family`, are already encoded
/// here, so a release that reassigns a family already resolves a different
/// capability without naming the release.
fn canonical_target_form(
    model: &ModelRef,
    route: &ModelRouteId,
    account: &AccountId,
    credential_version: &CredentialVersion,
) -> String {
    fn push(out: &mut String, label: &str, value: &str) {
        out.push_str(label);
        out.push('=');
        for ch in value.chars() {
            match ch {
                '\\' => out.push_str("\\\\"),
                '|' => out.push_str("\\p"),
                '=' => out.push_str("\\e"),
                other => out.push(other),
            }
        }
        out.push('|');
    }
    let mut out = String::from("model-target|");
    push(&mut out, "provider", model.provider().as_str());
    push(&mut out, "model", model.model().as_str());
    push(&mut out, "base", model.base_model().as_str());
    push(&mut out, "family", model.family().as_str());
    match model.variant() {
        None => out.push_str("variant_present=0|"),
        Some(variant) => {
            out.push_str("variant_present=1|");
            push(&mut out, "variant", variant.as_str());
        }
    }
    push(&mut out, "route", route.as_str());
    push(&mut out, "account", account.as_str());
    push(&mut out, "credential", credential_version.as_str());
    out
}
