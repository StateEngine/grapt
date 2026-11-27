//! The catalog release and the snapshot that admits it: [`ModelCatalogRecord`],
//! [`ModelCatalog`], [`ModelCatalogEntry`] and the private snapshot identity
//! they carry.
//!
//! Why `base_model` and `family` are sealed is stated once in the parent module
//! and is not restated here. What this file owns is the route from an authored
//! release to a vouched-for row: [`ModelCatalog::load`] refuses a release that
//! contradicts itself, and [`ModelCatalog::entry`] is the only producer of the
//! [`ModelCatalogEntry`] a [`ModelRef`](super::ModelRef) is built from.
//!
//! What it deliberately does NOT own is ISSUANCE. Loading a release and issuing
//! a routing target from it are different acts with different authority, and
//! [`issuance`](super::issuance) holds the second one. This file answers "what
//! does this release declare"; that one answers "may this target be routed to,
//! and is the release that vouched for it still current".
//!
//! Every item marked `pub(super)` below was private to the parent module before
//! that module was split. `pub(super)` here resolves to `pub(in crate::model)`,
//! which is the reachability those items already had — the parent and its
//! descendants — and not one caller wider.

use super::issuance::{CatalogGeneration, CatalogLineage};
use super::{
    BaseModelId, ModelCatalogVersion, ModelFamilyId, ModelId, ModelVariantId, ProviderId, Sealed,
};
use core::fmt;
use std::sync::Arc;

/// Process-local identity of ONE loaded catalog snapshot.
///
/// Identity is the allocation, compared with [`Arc::ptr_eq`], not the version
/// text. This is what makes a rogue catalog unable to impersonate the
/// runtime's by copying its version string.
///
/// It is private to this module, never serialized, and never part of a derived
/// [`ModelTargetCapabilityId`]: it is live authority, not durable identity — the same
/// separation the permission lane applies between an audit receipt and the
/// authenticated authority that produced it.
///
/// It now also carries the generation it was issued at and the lineage that
/// generation is measured against. Identity answers "which release"; the
/// lineage answers "is that release still the current one".
#[derive(Clone)]
pub(super) struct CatalogSnapshot {
    version: Arc<ModelCatalogVersion>,
    generation: CatalogGeneration,
    lineage: Arc<CatalogLineage>,
}

impl CatalogSnapshot {
    /// Begin a NEW lineage at [`CatalogGeneration::FIRST`].
    ///
    /// A catalog loaded on its own is its own lineage: nothing has superseded
    /// it, and nothing it did not descend from can.
    pub(super) fn new_lineage(version: ModelCatalogVersion) -> Self {
        Self {
            version: Arc::new(version),
            generation: CatalogGeneration::FIRST,
            lineage: Arc::new(CatalogLineage::started_at(CatalogGeneration::FIRST)),
        }
    }

    /// The successor snapshot in THIS lineage, at `generation`.
    pub(super) fn successor(
        &self,
        version: ModelCatalogVersion,
        generation: CatalogGeneration,
    ) -> Self {
        Self {
            version: Arc::new(version),
            generation,
            lineage: Arc::clone(&self.lineage),
        }
    }

    pub(super) fn version(&self) -> &ModelCatalogVersion {
        &self.version
    }

    pub(super) fn generation(&self) -> CatalogGeneration {
        self.generation
    }

    /// The generation this snapshot's lineage currently considers live.
    pub(super) fn current_generation(&self) -> CatalogGeneration {
        self.lineage.current()
    }

    /// Whether this snapshot is still the current issuance of its lineage.
    ///
    /// This is the staleness question, and it is deliberately separate from
    /// [`is_same_snapshot`](Self::is_same_snapshot): two targets can be the same
    /// issuance and both be stale.
    pub(super) fn is_current(&self) -> bool {
        self.generation == self.lineage.current()
    }

    /// Publish `to` as the current generation of this snapshot's lineage.
    ///
    /// The one mutation in this module, and it is deliberately narrow: the
    /// caller supplies a generation it derived with [`CatalogGeneration::succ`],
    /// and everything outstanding in the lineage goes stale at this call.
    pub(super) fn publish(&self, to: CatalogGeneration) {
        self.lineage.advance_to(to);
    }

    /// Whether both refs were vouched for by the very same loaded snapshot.
    pub(super) fn is_same_snapshot(&self, other: &CatalogSnapshot) -> bool {
        Arc::ptr_eq(&self.version, &other.version)
    }

    /// Whether both snapshots descend from the same lineage, whatever their
    /// generation. This is what makes a re-issuance receipt possible at all: a
    /// selection carried across a reload of an UNRELATED catalog is not carried,
    /// it is substituted.
    pub(super) fn is_same_lineage(&self, other: &CatalogSnapshot) -> bool {
        Arc::ptr_eq(&self.lineage, &other.lineage)
    }
}

impl fmt::Debug for CatalogSnapshot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "CatalogSnapshot({}, generation={})",
            self.version, self.generation
        )
    }
}

/// One row of a model-catalog release: the coordinates that release declares.
///
/// This is the catalog *document*, and it is plain data on purpose — a release
/// is authored somewhere. A record is not authority: it becomes usable truth
/// only once [`ModelCatalog::load`] admits it into a snapshot, and only a
/// snapshot hands out the [`ModelCatalogEntry`] a [`ModelRef`](super::ModelRef) is
/// built from.
///
/// Field order is the lookup key first, so a sorted record list groups every
/// claim about one coordinate together and a self-contradicting release is
/// detectable at load.
///
/// # No public comparison
///
/// `PartialEq`, `Eq`, `Hash`, `PartialOrd` and `Ord` are deliberately absent.
/// Every one of them compares `base_model` and `family`, which turns a row into
/// an equality oracle the moment a genuine row is reachable from a public route
/// — and one already was, because [`ModelRef`](super::ModelRef)'s equality
/// delegated here. Row
/// identity is the crate-private `same_row`; load-time
/// ordering is by lookup coordinate only, which is all
/// [`ModelCatalog::load`] needs.
///
/// `Clone` stays: duplicating a row a caller already holds grants nothing, and
/// [`ModelCatalog::entry`] needs it to issue an entry.
#[derive(Clone)]
pub struct ModelCatalogRecord {
    provider: ProviderId,
    model: ModelId,
    variant: Option<ModelVariantId>,
    base_model: BaseModelId,
    family: ModelFamilyId,
}

impl ModelCatalogRecord {
    /// Declare one catalog row.
    pub fn new(
        provider: ProviderId,
        model: ModelId,
        base_model: BaseModelId,
        family: ModelFamilyId,
        variant: Option<ModelVariantId>,
    ) -> Self {
        Self {
            provider,
            model,
            variant,
            base_model,
            family,
        }
    }

    pub fn provider(&self) -> &ProviderId {
        &self.provider
    }

    pub fn model(&self) -> &ModelId {
        &self.model
    }

    /// SEALED with [`family`](Self::family) — see the module note. A row read
    /// back out of a loaded catalog is the runtime's own statement about a
    /// target, and `base_model`/`family` are exactly the two coordinates the
    /// routing decision is made of.
    ///
    /// LATER-LANE DEPENDENCY: the model registry/config owner, which decides
    /// what a legitimate caller may ask a catalog.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn base_model(&self) -> &BaseModelId {
        &self.base_model
    }

    /// SEALED. Reading this off two rows of the runtime's catalog and comparing
    /// them by hand IS [`ModelRef::same_family`]; see the module note.
    pub(crate) fn family(&self) -> &ModelFamilyId {
        &self.family
    }

    pub fn variant(&self) -> Option<&ModelVariantId> {
        self.variant.as_ref()
    }

    /// The lookup coordinate this row makes a claim about.
    pub(super) fn coordinates(&self) -> (&ProviderId, &ModelId, Option<&ModelVariantId>) {
        (&self.provider, &self.model, self.variant.as_ref())
    }

    /// Whether two rows are the identical claim, sealed coordinates included.
    ///
    /// This is what `PartialEq` used to be, and it is crate-private for the
    /// reason recorded on the type: it compares `family`, so a public version
    /// answers the family question for any caller holding one genuine row.
    pub(super) fn same_row(&self, other: &Self) -> bool {
        self.coordinates() == other.coordinates()
            && self.base_model == other.base_model
            && self.family == other.family
    }
}

/// Redacted. `base_model` and `family` are the two coordinates the routing
/// decision is made of; a formatter is a read path like any other.
impl fmt::Debug for ModelCatalogRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelCatalogRecord")
            .field("provider", &self.provider)
            .field("model", &self.model)
            .field("variant", &self.variant)
            .field("base_model", &Sealed)
            .field("family", &Sealed)
            .finish()
    }
}

/// A loaded model-catalog snapshot: the authority on base model and family.
///
/// Not `Clone`: a catalog is a held authority, not a value to scatter. The
/// runtime holds the real one; anything else that loads a catalog holds a
/// *different* authority whose refs can never answer a family question about
/// the runtime's targets.
#[derive(Debug)]
pub struct ModelCatalog {
    snapshot: CatalogSnapshot,
    records: Vec<ModelCatalogRecord>,
}

impl ModelCatalog {
    /// Load a catalog release into a fresh snapshot.
    ///
    /// Refuses a release that declares two different truths for one
    /// `(provider, model, variant)` coordinate: a catalog that contradicts
    /// itself cannot be the authority on family membership, and silently
    /// picking a winner would be exactly the "family is whatever the last
    /// writer said" defect this type exists to remove. Exact duplicate rows
    /// are collapsed, since they assert nothing new.
    ///
    /// Every call mints a NEW snapshot identity even when `version` repeats,
    /// so the version string is audit provenance and never the authority.
    ///
    /// The ordering is by LOOKUP COORDINATE, not by the whole row. A public
    /// `Ord` over the row would sort on `base_model` and `family` and hand out
    /// a comparison oracle for both (see the type's note), and the load-time
    /// job does not need one: sorting by coordinate already makes every claim
    /// about one coordinate adjacent, which is the only property the
    /// contradiction check below depends on. Exact duplicates are then
    /// collapsed with the crate-private row equality. A repeated coordinate
    /// carrying two different claims never becomes adjacent-equal and is
    /// refused either way.
    pub fn load(version: ModelCatalogVersion, records: Vec<ModelCatalogRecord>) -> Option<Self> {
        let records = Self::admit(records)?;
        Some(Self {
            snapshot: CatalogSnapshot::new_lineage(version),
            records,
        })
    }

    /// Admit `records` as the SUCCESSOR release of this catalog's lineage.
    ///
    /// Same validation as [`load`](Self::load); the difference is the resulting
    /// snapshot descends from this one instead of starting a lineage of its own,
    /// which is what makes the old generation stale rather than merely
    /// different. Publishing the new generation is the caller's step, so a
    /// registry can build the successor and derive re-issuance receipts against
    /// it BEFORE anything outstanding goes stale.
    ///
    /// `None` if the release contradicts itself or the generation ceiling is
    /// reached.
    pub(super) fn successor(
        &self,
        version: ModelCatalogVersion,
        records: Vec<ModelCatalogRecord>,
    ) -> Option<Self> {
        let records = Self::admit(records)?;
        let generation = self.snapshot.generation().succ()?;
        Some(Self {
            snapshot: self.snapshot.successor(version, generation),
            records,
        })
    }

    /// Sort, collapse exact duplicates, and refuse a self-contradicting
    /// release. Shared by [`load`](Self::load) and `successor` so the two
    /// cannot drift: a successor admitted under weaker validation than the
    /// release it replaces would be a way in.
    fn admit(records: Vec<ModelCatalogRecord>) -> Option<Vec<ModelCatalogRecord>> {
        let mut records = records;
        records.sort_by(|left, right| left.coordinates().cmp(&right.coordinates()));
        records.dedup_by(|left, right| left.same_row(right));
        if records
            .windows(2)
            .any(|pair| pair[0].coordinates() == pair[1].coordinates())
        {
            return None;
        }
        Some(records)
    }

    /// The snapshot backing this release. `pub(super)` so the sibling
    /// [`issuance`](super::issuance) module can ask it the staleness and
    /// lineage questions; it is not a public read path.
    pub(super) fn snapshot(&self) -> &CatalogSnapshot {
        &self.snapshot
    }

    /// The admitted rows, for the sibling module's re-issuance lookup.
    ///
    /// Distinct from the SEALED public [`records`](Self::records): this one is
    /// `pub(super)` and exists so a successor release can be searched for the
    /// row a carried target names, without that search widening anything.
    pub(super) fn rows(&self) -> &[ModelCatalogRecord] {
        &self.records
    }

    /// The release this snapshot was loaded from. Audit provenance only.
    pub fn version(&self) -> &ModelCatalogVersion {
        self.snapshot.version()
    }

    /// The generation this release was admitted at.
    pub fn generation(&self) -> CatalogGeneration {
        self.snapshot.generation()
    }

    /// SEALED. Enumerating the loaded release hands out every row's `family`,
    /// which is the runtime's routing truth for every target it carries — the
    /// widest possible version of the manual-comparison bypass.
    ///
    /// LATER-LANE DEPENDENCY: the model registry/config owner. A model picker
    /// that legitimately needs to LIST targets belongs there and should be
    /// served a projection carrying the endpoint coordinates only.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn records(&self) -> &[ModelCatalogRecord] {
        &self.records
    }

    /// Look up the catalog's own statement about one coordinate.
    ///
    /// `None` for a model this release does not carry: a target absent from
    /// the catalog has no base model and no family, and therefore cannot be
    /// named as a [`ModelRef`](super::ModelRef) at all.
    pub fn entry(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        variant: Option<&ModelVariantId>,
    ) -> Option<ModelCatalogEntry> {
        let record = self
            .records
            .iter()
            .find(|record| record.coordinates() == (provider, model, variant))?;
        Some(ModelCatalogEntry {
            snapshot: self.snapshot.clone(),
            record: record.clone(),
        })
    }
}

/// The catalog's own statement about one model, carrying the snapshot that
/// made it.
///
/// Fields are private and there is no public constructor: the only producer is
/// [`ModelCatalog::entry`]. Holding one means the snapshot that issued it
/// actually contains this row — a caller cannot assert a base model or family
/// the catalog never declared.
#[derive(Clone)]
pub struct ModelCatalogEntry {
    pub(super) snapshot: CatalogSnapshot,
    pub(super) record: ModelCatalogRecord,
}

/// Redacted, for the same reason as the row it wraps: a derived `Debug` here
/// printed the record's sealed coordinates through the wrapper.
impl fmt::Debug for ModelCatalogEntry {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelCatalogEntry")
            .field("catalog_version", self.snapshot.version())
            .field("provider", self.record.provider())
            .field("model", self.record.model())
            .field("variant", &self.record.variant())
            .field("base_model", &Sealed)
            .field("family", &Sealed)
            .finish()
    }
}

impl ModelCatalogEntry {
    /// The release that vouched for this row.
    pub fn catalog_version(&self) -> &ModelCatalogVersion {
        self.snapshot.version()
    }

    // There is deliberately no `record()` accessor. An entry is issued by the
    // RUNTIME's catalog for a coordinate the caller merely named, so handing
    // back the row would let any caller read the genuine `family` of any
    // genuine target and then author a rogue row that matches it. The entry
    // exists to be CONSUMED by `ModelRef::from_catalog`, not to be read, and a
    // crate-private accessor nobody calls would only be a misleading stub.
}
