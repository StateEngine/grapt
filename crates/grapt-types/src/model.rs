//! Agent/provider/model vocabulary. Law: `PRESERVE` — request construction
//! cannot substitute an agent for a provider string; a `ModelRef` names all
//! three coordinates explicitly.
//!
//! Campaign Mode amendment §10 supersedes the old `(model, route)` target:
//! "capability belongs to the exact resolved target. Request construction
//! cannot substitute an agent/provider string, private credentials/endpoints/
//! routing handles are not serializable domain identity." Account is a
//! first-class routing dimension for residency and rate-limit recovery.
//! Account/provider/model fallback is never silent — it creates a new
//! `PhysicalAttempt` with its own selection provenance and spend receipt.
//!
//! # Catalog truth is not caller-asserted
//!
//! [`BaseModelId`] and [`ModelFamilyId`] are *catalog* facts, and family is the
//! boundary `agent default` may not cross (Agency/Continuity Amendment §7.5).
//! A caller who can author them can declare any two unrelated models to be one
//! family and walk a subagent across the boundary without a user selection.
//!
//! So there is no public route that accepts them. The only way to obtain a
//! [`ModelRef`] is [`ModelRef::from_catalog`], which consumes a
//! [`ModelCatalogEntry`], and the only producer of an entry is
//! [`ModelCatalog::entry`] — a lookup in a loaded snapshot.
//!
//! A snapshot is identified by its *allocation*, not by its name: two catalogs
//! that both call themselves version `v7` are still two different authorities.
//! `ModelRef::same_family` requires both refs to come from the same snapshot,
//! so a rogue catalog cannot answer a family question about a target the
//! runtime's catalog vouched for. It fails closed — the answer is `false`, a
//! refusal, never an accidental `true`.
//!
//! # Inert coordinates versus the routing decision
//!
//! Sealing `ModelRef::same_family` was necessary and not sufficient. The
//! predicate is `same snapshot && same provider && same family`, so a caller
//! that can READ `family()` off two refs reconstructs it by hand — the seal
//! guards the function while the accessor hands out its evidence.
//!
//! So the catalog facts that decide routing authority — `base_model` and
//! `family` — have no public read path anywhere: not on [`ModelRef`], not on
//! [`ModelCatalogRecord`], not through [`ModelCatalogEntry`] or
//! [`ModelCatalog`], and not through [`ResolvedModelTarget`]. What stays public
//! is the endpoint question, "which route do I call": provider, model, variant
//! and the audit provenance of the release that vouched for the row.
//!
//! [`ModelRef::is_same_issuance`] is public and is deliberately NOT a conjunct
//! of the family decision: it demands the SAME snapshot AND the SAME row, so it
//! can only ever say "these two values are one issuance of one target". It
//! cannot be combined with anything public to answer "are these two DIFFERENT
//! targets in one family", which is the question `agent default` may not answer
//! wrongly.
//!
//! # An accessor is not the only read channel
//!
//! Auditing the *accessors* and stopping there was the same mistake one level
//! up. A derived trait impl is a read path too, and three of them were wide
//! open:
//!
//! 1. **`Debug`.** `#[derive(Debug)]` on [`ModelCatalogRecord`],
//!    [`ModelCatalogEntry`], [`ModelRef`] and [`ResolvedModelTarget`] printed
//!    `base_model` and `family` verbatim. Sealing `family()` while
//!    `println!("{ref:?}")` spelled it out sealed nothing. Every `Debug` here is
//!    now hand-written and redacts both facts; only the endpoint coordinates and
//!    the audit provenance print.
//!
//! 2. **Equality and `Hash`.** `ModelRef`'s `PartialEq` compared the COMPLETE
//!    catalog row, which made it an equality ORACLE. An attacker authors a row
//!    guessing `family = "claude"`, loads it into a catalog of its own, converts
//!    it to a `ModelRef`, and compares it against a genuine ref: `true` confirms
//!    the guess, `false` refutes it, and the family space is small enough to
//!    enumerate. `Hash` leaked the same bit through digest comparison. Equality
//!    is now the INERT ENDPOINT only — provider, model, variant — so it answers
//!    "do these name the same route" and nothing about routing authority. Exact
//!    row identity stays behind [`ModelRef::is_same_issuance`], which requires
//!    the same loaded snapshot and therefore cannot be satisfied by a value the
//!    attacker authored. `ModelCatalogRecord` has no public `PartialEq`, `Eq`,
//!    `Hash` or `Ord` at all: row-level comparison is a crate-private
//!    `same_row`, and the catalog's own load-time ordering is by lookup
//!    coordinate.
//!
//! 3. **The derived capability, in plaintext.** A
//!    [`ModelTargetCapabilityId`](crate::capability::ModelTargetCapabilityId)
//!    is not a digest — `canonical_target_form` builds the literal string
//!    `…|base=…|family=…|…` and `as_str` hands it back.
//!    A public `ResolvedModelTarget::capability()` was therefore not an oracle
//!    but a plaintext dump of both sealed facts, strictly worse. The accessor is
//!    sealed; see its note for the later-lane owner and the structural fix.
//!
//! `Clone` is a fourth channel of a different kind: it does not read a fact, it
//! duplicates a proof. [`ResolvedModelTarget`] is the registry's statement that
//! one attempt may run against one target, so it is not `Clone` and admission
//! into an authority graph consumes it.

use core::fmt;

/// `Debug` placeholder for a catalog fact that decides routing authority.
///
/// Printing the field NAME with this value is deliberate: it records that the
/// coordinate exists and was withheld, rather than quietly omitting it and
/// leaving a future field addition to leak by default.
struct Sealed;

impl fmt::Debug for Sealed {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("<sealed>")
    }
}

crate::id::opaque_id! { AgentId }
crate::id::opaque_id! { ProviderId }
crate::id::opaque_id! { ModelId }
crate::id::opaque_id! { ModelVariantId }
crate::id::opaque_id! {
    /// An opaque routing handle for a model. The routing target, not a provider
    /// or model string; private endpoints/handles are not serializable identity.
    ModelRouteId
}
crate::id::opaque_id! {
    /// The validated base-model projection of a dated or pinned model id.
    ///
    /// Supplied by the versioned catalog, never guessed by trimming a suffix
    /// off a marketing name. It reaches a [`ModelRef`] only through
    /// [`ModelCatalog`]; no public constructor accepts one.
    BaseModelId
}
crate::id::opaque_id! {
    /// The model family a target belongs to.
    ///
    /// This is the boundary `agent default` may not cross. It is catalog data
    /// rather than an unversioned substring guess, because "same family" is a
    /// routing authority decision. It reaches a [`ModelRef`] only through
    /// [`ModelCatalog`]; no public constructor accepts one.
    ModelFamilyId
}
crate::id::opaque_id! {
    /// The durable, serializable version of one model-catalog release.
    ///
    /// This is an AUDIT identity: it records which release vouched for a
    /// target's base model and family. It is deliberately NOT the live
    /// authority — a name can be copied, and two snapshots claiming the same
    /// version are still distinct authorities (see [`ModelCatalog::load`]).
    ModelCatalogVersion
}

mod catalog;
mod issuance;
mod reference;
mod target;

pub use catalog::{ModelCatalog, ModelCatalogEntry, ModelCatalogRecord};
pub use issuance::{
    CatalogGeneration, CatalogReissuanceReceipt, IncapableContract, IssuanceRefusal, ModelRegistry,
    RoutingRequest,
};
pub use reference::ModelRef;
pub use target::ResolvedModelTarget;

#[cfg(test)]
pub(crate) mod test_catalog {
    //! In-crate catalog fixtures. Every family question in this crate's tests
    //! goes through a real [`ModelCatalog`], never through the test-only
    //! [`ModelRef::new`].

    use super::{
        BaseModelId, ModelCatalog, ModelCatalogRecord, ModelCatalogVersion, ModelFamilyId, ModelId,
        ModelRef, ProviderId,
    };

    /// `(provider, family, model)` as a catalog row with no variant.
    pub(crate) fn record(provider: &str, family: &str, model: &str) -> Option<ModelCatalogRecord> {
        Some(ModelCatalogRecord::new(
            ProviderId::new(provider)?,
            ModelId::new(model)?,
            BaseModelId::new(model)?,
            ModelFamilyId::new(family)?,
            None,
        ))
    }

    /// Load a snapshot from `(provider, family, model)` triples.
    pub(crate) fn catalog(version: &str, rows: &[(&str, &str, &str)]) -> Option<ModelCatalog> {
        let mut records = Vec::with_capacity(rows.len());
        for (provider, family, model) in rows {
            records.push(record(provider, family, model)?);
        }
        ModelCatalog::load(ModelCatalogVersion::new(version)?, records)
    }

    /// Look one row up and build the ref the catalog vouches for.
    pub(crate) fn model_ref(
        catalog: &ModelCatalog,
        provider: &str,
        model: &str,
    ) -> Option<ModelRef> {
        let entry = catalog.entry(&ProviderId::new(provider)?, &ModelId::new(model)?, None)?;
        Some(ModelRef::from_catalog(entry))
    }
}

#[cfg(test)]
mod tests;
