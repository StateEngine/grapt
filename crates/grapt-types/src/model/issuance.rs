//! Issuance: turning a loaded catalog release into a routing target, and
//! deciding when it stops being one.
//!
//! [`catalog`](super::catalog) owns what a release DECLARES. This file owns what
//! may be ROUTED to, which is a different authority and a different lifetime. A
//! release can be perfectly valid and no longer current, and the whole point of
//! separating them is that the second question has an answer the first cannot
//! give.
//!
//! Process-local by ruling (R2 §1, on the attended ruling *"I want
//! things to close when the gui and tui close"*). Nothing here is serialized or
//! reconstructed from persisted bytes: what ends at shutdown is the live
//! authority to route, never the durable record of what was routed.

use super::catalog::{ModelCatalog, ModelCatalogEntry, ModelCatalogRecord};
use super::target::ResolvedModelTarget;
use super::{ModelCatalogVersion, ModelId, ModelRouteId, ModelVariantId, ProviderId};
use crate::authority::PolicyVersion;
use crate::credential::{AccountId, CredentialVersion};
use core::fmt;
use std::sync::atomic::{AtomicU64, Ordering};

/// The monotonic generation of one catalog lineage.
///
/// # Why this is not an `opaque_id!`
///
/// Every other identity in this module is an `opaque_id!`, and this one
/// deliberately is not. That macro derives `PartialOrd`/`Ord` over the inner
/// `String`, which is a LEXICOGRAPHIC order: under it `"10"` sorts before
/// `"9"`, so generation ten would compare older than generation nine and a
/// superseded release would read as current. A generation whose whole job is a
/// monotonic comparison cannot be carried by a type whose ordering is textual.
///
/// It is `Copy` because it is a number, and comparing two is not an authority
/// question: it says which release is newer, never which family a target is in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CatalogGeneration(u64);

impl CatalogGeneration {
    /// The generation every freshly loaded lineage starts at.
    pub const FIRST: Self = Self(1);

    /// The next generation, or `None` at the ceiling.
    ///
    /// Saturating would silently stop advancing and make every later release
    /// compare equal to the last, which is the one failure this type exists to
    /// prevent. Overflow is therefore an explicit absence.
    pub fn succ(self) -> Option<Self> {
        self.0.checked_add(1).map(Self)
    }

    /// The ordinal, for display and receipts. Not a construction route.
    pub fn get(self) -> u64 {
        self.0
    }
}

impl fmt::Display for CatalogGeneration {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// The live lineage a sequence of catalog releases belongs to.
///
/// This is the piece pointer identity could not supply. `Arc::ptr_eq`
/// distinguishes two snapshots, but every issued target holds a STRONG clone of
/// the one it was minted from, so dropping the `ModelCatalog` retires nothing: a
/// release withdrawn for bad family data keeps satisfying `is_same_snapshot` and
/// keeps answering `same_family` for the life of the process. Identity can say
/// "not the same release"; only a lineage can say "no longer the current one".
///
/// The current generation is shared, so superseding is observable from every
/// outstanding issuance without the holder having to consult the registry it
/// came from — which it cannot, since a target may outlive any particular
/// borrow of one.
///
/// Process-local by ruling (R2 §1, on the attended ruling *"I want
/// things to close when the gui and tui close"*): never serialized, never
/// reconstructed from persisted bytes.
pub(super) struct CatalogLineage {
    current: AtomicU64,
}

impl CatalogLineage {
    pub(super) fn started_at(generation: CatalogGeneration) -> Self {
        Self {
            current: AtomicU64::new(generation.get()),
        }
    }

    pub(super) fn current(&self) -> CatalogGeneration {
        CatalogGeneration(self.current.load(Ordering::SeqCst))
    }

    /// Publish a new current generation. Monotonic by construction: the caller
    /// derives `next` with [`CatalogGeneration::succ`].
    pub(super) fn advance_to(&self, next: CatalogGeneration) {
        self.current.store(next.get(), Ordering::SeqCst);
    }
}

impl fmt::Debug for CatalogLineage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "CatalogLineage(current={})", self.current())
    }
}

/// Proof that one target survived a catalog reload unchanged.
///
/// The routing analogue of `NodeCarryForwardReceipt`, and it exists because of a
/// measured defect: [`ModelCatalog::load`] mints a fresh snapshot on EVERY call,
/// even for a byte-identical release, so an ordinary config reload — including
/// one the user did not cause, from a file watch — made every outstanding
/// issuance foreign. A live `UserModelSelection` is not `Clone` and carries a
/// single-use `DecisionNonce`, so that reload destroyed the user's Subagent
/// model selection outright rather than refreshing it.
///
/// Master §7.5: *"A semantically null reload does not make an issuance stale.
/// Carrying a live selection or an in-flight routing authority across a catalog
/// reload requires a re-issuance receipt proving same coordinates, same base
/// model, same family and a new generation."* All four are checked here, and a
/// reload that changes any of them — the family included — produces **no**
/// receipt, so the selections it carried refuse as stale.
///
/// # It carries no coordinates a reader could learn from
///
/// The receipt is a PROOF, not a record. It names the two generations, which are
/// ordinals, and nothing else. It deliberately does not carry the base model or
/// the family it verified: a receipt that reported which family it matched would
/// be the family oracle this module removes, arriving through the door built to
/// preserve selections. What a holder learns is "this target carried forward",
/// which is the question, and no more.
///
/// # Surface-agnostic by construction
///
/// It is minted from two catalog issuances and nothing else. It names no client,
/// no selector and no surface, so admitting a selection made in a TUI, a CLI or
/// any other authenticated client needs no change here — the authenticated
/// identity stays where it already lives, on `UserModelSelection`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogReissuanceReceipt {
    from: CatalogGeneration,
    to: CatalogGeneration,
}

impl CatalogReissuanceReceipt {
    /// The generation the carried selection was issued at.
    pub fn from_generation(&self) -> CatalogGeneration {
        self.from
    }

    /// The generation it was carried into. Strictly greater than `from`.
    pub fn to_generation(&self) -> CatalogGeneration {
        self.to
    }
}

/// The reason an issuance was refused.
///
/// Master §7.5 names three cases and requires them to stay distinct:
/// *"If unavailable, incapable of satisfying the task/tool/authority contract,
/// or carried by a stale issuance, admission refuses or asks rather than
/// silently choosing another family."*
///
/// They are three variants rather than one with a message because the law turns
/// on telling them apart: *"Availability and capability are properties of the
/// target; staleness is a property of the issuance. A stale refusal is therefore
/// never evidence that the target is gone or unfit, and never licenses
/// substituting another family."* A caller that matched on a string, or on a
/// single `Refused` variant, would have to re-derive that distinction from prose.
///
/// The enum is closed and carries no catchall. Adding a fourth case breaks every
/// exhaustive match, which is the intended cost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IssuanceRefusal {
    /// The catalog does not carry this coordinate at all. A property of the
    /// TARGET: there is no row, so there is no base model and no family.
    Unavailable,
    /// The catalog carries the coordinate, but the requested contract cannot be
    /// satisfied by it. A property of the TARGET.
    ///
    /// It carries the declared reason rather than a free string so a caller can
    /// report which contract failed without parsing prose.
    Incapable { contract: IncapableContract },
    /// The coordinate is present and capable, and the ISSUANCE carrying it is no
    /// longer current.
    ///
    /// This says nothing about the target — it is present and it is fit. Both
    /// generations are named because a refusal that reported neither would be a
    /// bare no, and the caller's next legal step is to ask the current issuance
    /// for the same coordinates.
    Stale {
        issued_at: CatalogGeneration,
        current: CatalogGeneration,
    },
}

/// Which declared contract an [`IssuanceRefusal::Incapable`] failed.
///
/// Closed, and deliberately small: this lane admits only the contract it can
/// actually evaluate at issue time. A capability lattice belongs to the policy
/// owner, and inventing variants here that nothing evaluates would be the
/// "described, not carried" defect.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IncapableContract {
    /// The requested variant is not the one the catalog row declares.
    Variant,
}

/// The inert routing coordinates an issuance is requested under.
///
/// Grouped rather than passed loose because they travel together through every
/// issuance route, and because a four-coordinate positional call is how a route
/// and an account get transposed.
///
/// # Why the fields are public
///
/// All four are inert endpoint coordinates with public constructors already:
/// `ModelRouteId`, `AccountId`, `CredentialVersion` and `PolicyVersion` are
/// `opaque_id!`-class values a caller can mint from any non-empty string. None
/// of them is catalog truth, so private fields here would add ceremony without
/// adding a seal — the sealed facts are `base_model` and `family`, which this
/// type deliberately cannot carry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoutingRequest {
    pub route: ModelRouteId,
    pub account: AccountId,
    pub credential_version: CredentialVersion,
    pub policy: PolicyVersion,
}

/// The model registry: the owner that decides which catalog is genuine and
/// issues routing targets from it.
///
/// This is the "later-lane dependency" named throughout [`super`] — on
/// `ModelCatalogRecord::base_model`, on `ModelCatalog::records`, on
/// `ModelRef::same_family` and on `ResolvedModelTarget::resolve`. Those items
/// stay sealed. What this type adds is the ONE legitimate route by which a
/// resolved target enters the workspace, which is what the seal was waiting for
/// rather than something it forbade: `grapt-policy`'s twelve pending witnesses
/// are pending precisely because *"no component in this workspace can issue one
/// yet."*
///
/// # Why this is not a widened `resolve`
///
/// `ResolvedModelTarget::resolve` remains crate-private with no public route.
/// Issuance is narrower than resolution in three ways a public `resolve` would
/// not be: it can only name a coordinate the held catalog actually declares, it
/// refuses a superseded generation, and it consumes a `ModelCatalogEntry` that
/// only a loaded snapshot produces. A caller still cannot assert a base model or
/// a family, and a target issued by a registry built over a catalog the caller
/// wrote is still not the same issuance as the runtime's — so
/// `SnapshotBuilder::model` still refuses it with `ForeignModelResolution`.
///
/// Not `Clone`: a registry is a held authority. The runtime holds the real one.
#[derive(Debug)]
pub struct ModelRegistry {
    catalog: ModelCatalog,
}

impl ModelRegistry {
    /// Install a loaded release as this registry's current catalog.
    pub fn install(catalog: ModelCatalog) -> Self {
        Self { catalog }
    }

    /// The catalog currently in force.
    pub fn catalog(&self) -> &ModelCatalog {
        &self.catalog
    }

    /// The generation currently in force.
    pub fn generation(&self) -> CatalogGeneration {
        self.catalog.generation()
    }

    /// Issue a routing target for one coordinate.
    ///
    /// The only public route to a [`ResolvedModelTarget`]. Refuses with a typed
    /// [`IssuanceRefusal`] rather than an `Option`, because the three reasons are
    /// not interchangeable and §7.5 turns on which one it was.
    ///
    /// The staleness check is here, at issue time, and not only at use time: a
    /// registry whose catalog has been superseded must not mint anything new
    /// against the retired generation.
    pub fn issue(
        &self,
        provider: &ProviderId,
        model: &ModelId,
        variant: Option<&ModelVariantId>,
        request: RoutingRequest,
    ) -> Result<ResolvedModelTarget, IssuanceRefusal> {
        let snapshot = self.catalog.snapshot();
        if !snapshot.is_current() {
            return Err(IssuanceRefusal::Stale {
                issued_at: snapshot.generation(),
                current: snapshot.current_generation(),
            });
        }
        let entry = self
            .catalog
            .entry(provider, model, variant)
            .ok_or(IssuanceRefusal::Unavailable)?;
        Ok(ResolvedModelTarget::resolve(
            super::ModelRef::from_catalog(entry),
            request.route,
            request.account,
            request.credential_version,
            request.policy,
        ))
    }

    /// Admit a successor release and retire the current one.
    ///
    /// Returns the superseded catalog so the caller can derive re-issuance
    /// receipts against the new generation for selections it still holds. The
    /// order matters and is the whole point: the successor is built and
    /// validated FIRST, and only a successful admission advances the lineage, so
    /// a release that contradicts itself cannot retire a good one by failing.
    ///
    /// `None` — leaving this registry untouched — if the successor contradicts
    /// itself or the generation ceiling is reached.
    pub fn supersede(
        &mut self,
        version: ModelCatalogVersion,
        records: Vec<ModelCatalogRecord>,
    ) -> Option<ModelCatalog> {
        let successor = self.catalog.successor(version, records)?;
        let generation = successor.generation();
        let retired = std::mem::replace(&mut self.catalog, successor);
        // Published only now: everything outstanding goes stale at this line,
        // and nothing before it.
        self.catalog.snapshot().publish(generation);
        Some(retired)
    }

    /// Prove that `carried` survives into the current generation unchanged, and
    /// re-issue it there.
    ///
    /// This is the semantically-null reload of master §7.5. It returns the
    /// receipt and a target issued at the CURRENT generation, so the caller
    /// replaces the stale value rather than keeping it alive.
    ///
    /// Refuses with [`IssuanceRefusal::Stale`] if `carried` belongs to a
    /// different lineage: a selection carried across an unrelated catalog has not
    /// been carried, it has been substituted, and substitution is the thing §7.5
    /// forbids.
    ///
    /// Refuses if any of the four proofs fails — same coordinates, same base
    /// model, same family, new generation. The family check is not decorative:
    /// without it a reload that reassigned a model to another family would carry
    /// the user's selection straight across the boundary `agent default` may not
    /// cross.
    pub fn reissue(
        &self,
        carried: &ResolvedModelTarget,
        request: RoutingRequest,
    ) -> Result<(CatalogReissuanceReceipt, ResolvedModelTarget), IssuanceRefusal> {
        let carried_ref = carried.model_ref();
        let carried_snapshot = carried_ref.snapshot();
        let current = self.catalog.snapshot();

        if !carried_snapshot.is_same_lineage(current) {
            return Err(IssuanceRefusal::Stale {
                issued_at: carried_snapshot.generation(),
                current: current.current_generation(),
            });
        }

        let from = carried_snapshot.generation();
        let to = current.generation();
        if from >= to {
            // Not a reload: there is nothing to carry forward, and minting a
            // receipt for a generation that did not advance would let a caller
            // manufacture proof of a reissuance that never happened.
            return Err(IssuanceRefusal::Stale {
                issued_at: from,
                current: current.current_generation(),
            });
        }

        let coordinates = carried_ref.lookup_coordinates();
        let successor_row = self
            .catalog
            .rows()
            .iter()
            .find(|record| record.coordinates() == coordinates)
            .ok_or(IssuanceRefusal::Unavailable)?;

        if !carried_ref.declares_same_catalog_facts(successor_row) {
            // Same coordinates, different base model or family. No receipt, and
            // the selection this would have carried refuses as stale.
            return Err(IssuanceRefusal::Stale {
                issued_at: from,
                current: current.current_generation(),
            });
        }

        let entry = ModelCatalogEntry {
            snapshot: current.clone(),
            record: successor_row.clone(),
        };
        Ok((
            CatalogReissuanceReceipt { from, to },
            ResolvedModelTarget::resolve(
                super::ModelRef::from_catalog(entry),
                request.route,
                request.account,
                request.credential_version,
                request.policy,
            ),
        ))
    }
}
