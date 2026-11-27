//! [`ModelRef`]: the model coordinates a caller may hold, sealed to catalog
//! truth.
//!
//! Why `base_model` and `family` have no public read path, and why equality is
//! the inert endpoint rather than the whole row, is stated once in the parent
//! module and is not restated here.
//!
//! # One audit surface across two files
//!
//! [`PartialEq`] and [`Hash`] below both read
//! [`ModelCatalogRecord::coordinates`], which lives in the sibling
//! [`catalog`](super::catalog) module. The equality oracle and the row it reads
//! are lexically apart and semantically one thing: what `coordinates` returns
//! decides what `==` compares and what `hash` feeds the hasher. The file
//! boundary does not make them two surfaces, and a change to either is a change
//! to both.

use super::catalog::{CatalogSnapshot, ModelCatalogEntry, ModelCatalogRecord};
use super::{
    BaseModelId, ModelCatalogVersion, ModelFamilyId, ModelId, ModelVariantId, ProviderId, Sealed,
};
use core::fmt;
use core::hash::{Hash, Hasher};

/// Full model coordinates. Variant is genuinely optional placement — not an
/// empty-string sentinel.
///
/// Construction is sealed to catalog truth: [`ModelRef::from_catalog`] is the
/// only route, and it consumes a [`ModelCatalogEntry`]. The snapshot that
/// vouched for the row is carried privately, so `ModelRef::same_family` can
/// refuse to compare targets from two different catalogs.
#[derive(Clone)]
pub struct ModelRef {
    snapshot: CatalogSnapshot,
    record: ModelCatalogRecord,
}

/// Redacted. Provider, model, variant and the vouching release are the endpoint
/// question and print; `base_model` and `family` are the routing decision and
/// do not. A derived `Debug` here was a one-line bypass of every seal on this
/// type.
impl fmt::Debug for ModelRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ModelRef")
            .field("provider", self.record.provider())
            .field("model", self.record.model())
            .field("variant", &self.record.variant())
            .field("catalog_version", self.snapshot.version())
            .field("base_model", &Sealed)
            .field("family", &Sealed)
            .finish()
    }
}

impl ModelRef {
    /// The one route from catalog truth to model coordinates.
    ///
    /// Base model and family are not parameters here and are not parameters
    /// anywhere else: they arrive already decided by the catalog.
    pub fn from_catalog(entry: ModelCatalogEntry) -> Self {
        Self {
            snapshot: entry.snapshot,
            record: entry.record,
        }
    }

    /// TEST-ONLY coordinates for in-crate tests that never ask a family
    /// question.
    ///
    /// `#[cfg(test)] pub(crate)` is the only visibility that fails closed: the
    /// function does not exist in any real build, so no dependent can reach it.
    /// A Cargo feature would be additive and therefore enableable by any
    /// dependent, and `#[doc(hidden)]` hides nothing.
    ///
    /// Each call mints its OWN snapshot, so two refs built here are never
    /// `same_family` with each other. A test that asks a family question must
    /// go through a real [`ModelCatalog`].
    #[cfg(test)]
    pub(crate) fn new(
        provider: ProviderId,
        model: ModelId,
        base_model: BaseModelId,
        family: ModelFamilyId,
        variant: Option<ModelVariantId>,
    ) -> Self {
        Self {
            snapshot: CatalogSnapshot::new_lineage(ModelCatalogVersion(
                "test-only-unvouched-catalog".to_owned(),
            )),
            record: ModelCatalogRecord::new(provider, model, base_model, family, variant),
        }
    }

    /// The catalog release that vouched for this target's base model and
    /// family. Audit provenance: it says who made the claim, not that the
    /// claim may be trusted against another catalog's targets.
    pub fn catalog_version(&self) -> &ModelCatalogVersion {
        self.snapshot.version()
    }

    pub fn provider(&self) -> &ProviderId {
        self.record.provider()
    }

    pub fn model(&self) -> &ModelId {
        self.record.model()
    }

    /// SEALED — catalog truth about routing, not endpoint identity. See the
    /// module note: a caller that can read it can reconstruct
    /// [`same_family`](Self::same_family) by hand.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn base_model(&self) -> &BaseModelId {
        self.record.base_model()
    }

    /// SEALED — this is the boundary `agent default` may not cross, and
    /// comparing it between two refs IS the boundary decision.
    pub(crate) fn family(&self) -> &ModelFamilyId {
        self.record.family()
    }

    pub fn variant(&self) -> Option<&ModelVariantId> {
        self.record.variant()
    }

    /// The catalog generation that vouched for this ref.
    ///
    /// Public for the reason the same accessor on `ResolvedModelTarget` is: a
    /// generation is an ordinal naming which release made the claim, never what
    /// the claim was. It cannot be combined with anything public to answer a
    /// family question.
    pub fn issuance_generation(&self) -> super::issuance::CatalogGeneration {
        self.snapshot.generation()
    }

    /// The snapshot that vouched for this ref.
    ///
    /// `pub(super)` — the module's own issuance machinery asks it the staleness
    /// and lineage questions. It is not an accessor for anyone else: a snapshot
    /// is live authority, and handing one out would let a holder mint entries
    /// the runtime's catalog never issued.
    pub(super) fn snapshot(&self) -> &CatalogSnapshot {
        &self.snapshot
    }

    /// The lookup coordinate this ref names, for finding the same row in a
    /// successor release. Inert: the three coordinates `PartialEq` already
    /// compares.
    pub(super) fn lookup_coordinates(&self) -> (&ProviderId, &ModelId, Option<&ModelVariantId>) {
        self.record.coordinates()
    }

    /// Whether `row` declares the same sealed catalog facts as this ref.
    ///
    /// The "same base model, same family" half of a re-issuance proof. It
    /// returns a BOOLEAN and lives here rather than exposing either coordinate,
    /// so the receipt can be proved without the family becoming readable —
    /// which is the whole reason those two accessors are sealed.
    pub(super) fn declares_same_catalog_facts(&self, row: &ModelCatalogRecord) -> bool {
        self.record.base_model() == row.base_model() && self.record.family() == row.family()
    }

    /// Whether `other` is the SAME issuance of the SAME target: one loaded
    /// snapshot, one row.
    ///
    /// This is the identity a consumer needs when it must not let two values
    /// that merely *spell* the same target stand in for each other — a request
    /// and the authority it runs under, for instance. `PartialEq` cannot serve
    /// there, because it is deliberately provenance-blind (see its note).
    ///
    /// It is NOT a piece of the family decision and cannot be assembled into
    /// one. The sealed `same_family` predicate asks whether two DIFFERENT rows
    /// share a family; this demands the rows be equal, so it answers `false`
    /// for exactly the pairs that question is about. It fails closed: two refs
    /// from two catalogs are never one issuance, however identical their text.
    pub fn is_same_issuance(&self, other: &ModelRef) -> bool {
        self.snapshot.is_same_snapshot(&other.snapshot) && self.record.same_row(&other.record)
    }

    /// Whether `other` is reachable without crossing a family or provider
    /// boundary.
    ///
    /// Three things must hold, and the first is the authority question:
    ///
    /// 1. both targets were vouched for by the SAME loaded catalog snapshot —
    ///    a family claim from another catalog is not an answer about this one,
    ///    so it is refused rather than believed;
    /// 2. the provider matches;
    /// 3. the family matches.
    ///
    /// The law treats cross-provider and cross-family as equally hard
    /// boundaries for `agent default`.
    /// SEALED — this is a routing AUTHORITY decision, not a query.
    ///
    /// It answers the one question `agent default` may not answer wrongly: may a
    /// child cross to this target? A caller could previously author two
    /// `ModelCatalogRecord`s, load them into one rogue `ModelCatalog`, and get
    /// `true` for a pair it invented. Same-snapshot `Arc` identity makes a rogue
    /// catalog unable to vouch for the RUNTIME's targets, which is why the
    /// reload semantics stand, but identity of a snapshot is not authority over
    /// it: the attacker owns both rows.
    ///
    /// The catalog types stay public because a `ModelRef` is inert coordinates
    /// that downstream crates legitimately hold in a provider request. What is
    /// sealed is authority-producing resolution: this predicate and
    /// [`ResolvedModelTarget::resolve`], which derives a
    /// `ModelTargetCapabilityId` — AND,
    /// since the round that added this paragraph, the evidence the predicate is
    /// made of. Sealing the function while `family()` stayed readable left the
    /// decision reproducible by hand; see the module note.
    ///
    /// LATER-LANE DEPENDENCY: the model registry/config owner, which knows which
    /// catalog release is the real one.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) fn same_family(&self, other: &ModelRef) -> bool {
        self.snapshot.is_same_snapshot(&other.snapshot)
            && self.record.provider() == other.record.provider()
            && self.record.family() == other.record.family()
    }
}

/// Equality is the INERT ENDPOINT: provider, model, variant.
///
/// It answers "do these two values name the same route to call", which is not
/// an authority question, and it answers nothing else. It is kept public
/// because real consumers need exactly that much and cannot be reached from
/// here: `CompactionSummary::is_redeemable_by` asks whether an opaque summary
/// may be replayed against a target, `ProviderRequest`/`UserModelSelection`
/// derive it, and the provider request boundary uses it for its readable
/// mismatch diagnostic before the strict issuance check.
///
/// # Why it is no longer the whole row
///
/// It used to compare the COMPLETE [`ModelCatalogRecord`], `family` included,
/// and that made it an oracle for the one fact this module seals. The attack
/// needs nothing privileged: author a row for a genuine endpoint guessing
/// `family = "claude"`, load it into a catalog of your own, build a `ModelRef`,
/// and compare it against a genuine ref obtained from the runtime's catalog.
/// `true` confirms the guess and `false` refutes it, one bit per comparison,
/// over a family space small enough to enumerate — after which the attacker
/// authors a rogue row that matches the runtime's family by string and every
/// comparison-by-name downstream agrees with it.
///
/// Catalog provenance stays excluded for the original reason too: two releases
/// that agree about a target name the same target, and an explicit user
/// selection must still match the admitted child after a catalog reload.
/// Provenance is an authority question, asked by the crate-private
/// `ModelRef::same_family` and by the public [`ModelRef::is_same_issuance`] —
/// never silently folded into `==`.
impl PartialEq for ModelRef {
    fn eq(&self, other: &Self) -> bool {
        self.record.coordinates() == other.record.coordinates()
    }
}

impl Eq for ModelRef {}

/// Hashes exactly what `PartialEq` compares. Hashing the whole row would rebuild
/// the same oracle through digest comparison instead of `==`, so the sealed
/// coordinates are not fed to the hasher.
impl Hash for ModelRef {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let (provider, model, variant) = self.record.coordinates();
        provider.hash(state);
        model.hash(state);
        variant.hash(state);
    }
}
