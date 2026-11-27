//! The immutable physical-attempt authority snapshot and its content-addressed
//! receipt.
//!
//! Root correction #4 (spec §"Root corrections"): "config, credential,
//! model/provider, plugin/MCP, environment and tool visibility produce one
//! immutable receipted graph per physical attempt." The foundational-types
//! report places this receipt in the attempt's synchronous `Prepared` state
//! (`Prepared { step, request, graph_receipt }`), before any streaming — it is
//! an operational receipt, "not automatically a public Session event".
//!
//! A [`SnapshotBuilder`] resolves typed contributors, rejecting duplicate and
//! conflicting keys and (for config) precedence conflicts. [`build`] freezes
//! them into a [`PhysicalAttemptAuthoritySnapshot`] with no mutators, no live
//! handles, and no secret material. [`PhysicalAttemptAuthoritySnapshot::receipt`]
//! lowers the snapshot to one canonical node, content-addresses it, and mints
//! the existing [`EffectiveGraphId`] identity.
//!
//! [`build`]: SnapshotBuilder::build

use crate::contributor::{
    model_node, ConfigLayer, CredentialFingerprint, EnvExposure, PermissionDecision,
    ServiceActivation, ToolName, ToolVisibility,
};
use crate::encoding::{content_digest, Node};
use grapt_types::{EffectiveGraphId, ResolvedModelTarget};

/// Typed failure to resolve an effective graph. Every rejection names the class
/// and key so a caller never guesses which contributor collided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PolicyError {
    #[error("effective graph is missing its required model contributor")]
    MissingModel,
    #[error("conflicting model contributor: already set to a different model")]
    ConflictingModel,
    #[error(
        "conflicting model contributor: the same coordinates from a different catalog issuance"
    )]
    ForeignModelResolution,
    /// The target is present and fit; the catalog issuance carrying it has been
    /// superseded.
    ///
    /// A THIRD refusal, distinct from `ConflictingModel` and
    /// `ForeignModelResolution`, because master §7.5 turns on the distinction:
    /// availability and capability are properties of the target, staleness is a
    /// property of the issuance. Neither of the other two would be true here —
    /// there may be no other contributor to conflict with, and the issuance is
    /// not foreign, it is this lineage's own retired generation.
    ///
    /// It reports both generations because a refusal that named neither would be
    /// a bare no, and the caller's legal next step is to ask the current
    /// issuance for the same coordinates.
    #[error(
        "stale model contributor: issued at catalog generation {issued_at}, current is {current}"
    )]
    StaleModelIssuance { issued_at: u64, current: u64 },
    #[error("duplicate {class} contributor for key {key:?}")]
    DuplicateContributor { class: &'static str, key: String },
    #[error("conflicting {class} contributor for key {key:?}: two different values")]
    ConflictingContributor { class: &'static str, key: String },
    #[error("config precedence conflict at rank {rank}: two different layers claim it")]
    PrecedenceConflict { rank: u64 },
    #[error("content address produced an empty identity")]
    EmptyIdentity,
}

/// Builds a [`PhysicalAttemptAuthoritySnapshot`] by resolving contributors.
/// Insertion order does not affect the result: keys are deduplicated and the
/// canonical encoding is order-canonical.
#[derive(Default)]
pub struct SnapshotBuilder {
    model: Option<ResolvedModelTarget>,
    config: Vec<ConfigLayer>,
    credentials: Vec<CredentialFingerprint>,
    services: Vec<ServiceActivation>,
    permissions: Vec<PermissionDecision>,
    tools: Vec<ToolVisibility>,
    environment: Vec<EnvExposure>,
}

/// Redacted, and hand-written because [`ResolvedModelTarget`] has no `Debug`
/// at all (see its note: a formatter over the resolved tuple printed the
/// derived capability, whose canonical form spells `base=` and `family=`). The
/// builder reports only whether a model contributor has been set and how many
/// contributors of each class it holds — enough to diagnose a duplicate or a
/// missing model, and no authority fact.
impl core::fmt::Debug for SnapshotBuilder {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SnapshotBuilder")
            .field("model_set", &self.model.is_some())
            .field("config", &self.config.len())
            .field("credentials", &self.credentials.len())
            .field("services", &self.services.len())
            .field("permissions", &self.permissions.len())
            .field("tools", &self.tools.len())
            .field("environment", &self.environment.len())
            .finish()
    }
}

impl SnapshotBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the model contributor from a REGISTRY-ISSUED target.
    ///
    /// This takes [`ResolvedModelTarget`], not `ModelRef`, and that is the whole
    /// point. A `ModelRef` is inert metadata anyone can mint by authoring a
    /// catalog release and loading it; letting one in here meant a rogue
    /// snapshot, and from there a rogue prepared request. The resolved target is
    /// sealed — `ResolvedModelTarget::resolve` is crate-private to
    /// `grapt-types` with no public route — so authority cannot be entered
    /// from a catalog the caller wrote.
    ///
    /// Setting the SAME ISSUANCE twice is idempotent; a different target is a
    /// typed conflict, and the same coordinates from a different catalog
    /// issuance is a distinct, separately typed conflict.
    ///
    /// It takes the target BY VALUE and `ResolvedModelTarget` is not `Clone`, so
    /// one issuance admits one authority graph. A caller cannot keep a copy back
    /// to admit a second graph under the same registry statement.
    ///
    /// The two comparisons are the target's own non-oracular predicates rather
    /// than `==`, which no longer exists on it: equality over the resolved tuple
    /// compared the catalog's `base_model`/`family` and the capability derived
    /// from them, so it answered the sealed routing question for anyone holding
    /// one genuine target. [`ResolvedModelTarget::describes_same_target`] asks
    /// only about the inert coordinates, and
    /// [`ResolvedModelTarget::is_same_issuance`] asks about provenance; the
    /// pair distinguishes exactly the three outcomes below without either one
    /// exposing a family fact.
    ///
    /// The F1A fail-closed state is now DISCHARGED. While no component could
    /// issue a target, this builder could not be completed at all, and the
    /// twelve acceptance witnesses that need one were parked in a pending
    /// inventory. L2 built the issuing owner — `grapt_types::ModelRegistry` —
    /// and those witnesses are restored through it in `tests/graph_receipt.rs`
    /// and `tests/graph_contributors.rs`. `ResolvedModelTarget::resolve` stays
    /// crate-private to `grapt-types`: issuance is a narrower route, not a
    /// widened one.
    pub fn model(&mut self, model: ResolvedModelTarget) -> Result<&mut Self, PolicyError> {
        // Checked BEFORE the conflict arms, and that order is load-bearing. A
        // stale target that happens to describe the same coordinates as the one
        // already set would otherwise be admitted as an idempotent re-set, which
        // is the exact case a withdrawn release produces: same coordinates, same
        // lineage, retired generation.
        if !model.is_current_issuance() {
            return Err(PolicyError::StaleModelIssuance {
                issued_at: model.issuance_generation().get(),
                current: model.current_catalog_generation().get(),
            });
        }
        match &self.model {
            Some(existing) if !existing.describes_same_target(&model) => {
                Err(PolicyError::ConflictingModel)
            }
            Some(existing) if !existing.is_same_issuance(&model) => {
                Err(PolicyError::ForeignModelResolution)
            }
            _ => {
                self.model = Some(model);
                Ok(self)
            }
        }
    }

    /// Add a config-provenance layer. Precedence is the layer's explicit `rank`;
    /// a repeated rank with identical content is a duplicate, with different
    /// content a precedence conflict.
    pub fn config_layer(&mut self, layer: ConfigLayer) -> Result<&mut Self, PolicyError> {
        if let Some(existing) = self.config.iter().find(|l| l.rank() == layer.rank()) {
            return if *existing == layer {
                Err(PolicyError::DuplicateContributor {
                    class: "config",
                    key: layer.rank().to_string(),
                })
            } else {
                Err(PolicyError::PrecedenceConflict { rank: layer.rank() })
            };
        }
        self.config.push(layer);
        Ok(self)
    }

    pub fn credential(
        &mut self,
        credential: CredentialFingerprint,
    ) -> Result<&mut Self, PolicyError> {
        dedup_push(&mut self.credentials, credential, "credential", |c| {
            c.conflict_key()
        })?;
        Ok(self)
    }

    pub fn service(&mut self, service: ServiceActivation) -> Result<&mut Self, PolicyError> {
        dedup_push(&mut self.services, service, "service", |s| s.conflict_key())?;
        Ok(self)
    }

    pub fn permission(&mut self, permission: PermissionDecision) -> Result<&mut Self, PolicyError> {
        dedup_push(&mut self.permissions, permission, "permission", |p| {
            p.conflict_key()
        })?;
        Ok(self)
    }

    pub fn tool(&mut self, tool: ToolVisibility) -> Result<&mut Self, PolicyError> {
        dedup_push(&mut self.tools, tool, "tool", |t| t.conflict_key())?;
        Ok(self)
    }

    pub fn environment(&mut self, exposure: EnvExposure) -> Result<&mut Self, PolicyError> {
        dedup_push(&mut self.environment, exposure, "environment", |e| {
            e.conflict_key()
        })?;
        Ok(self)
    }

    /// Freeze the resolved contributors. The model is required; everything else
    /// may legitimately be empty (an attempt with no plugins, no saved
    /// credentials, no exposed environment).
    pub fn build(self) -> Result<PhysicalAttemptAuthoritySnapshot, PolicyError> {
        let model = self.model.ok_or(PolicyError::MissingModel)?;
        Ok(PhysicalAttemptAuthoritySnapshot {
            model,
            config: self.config,
            credentials: self.credentials,
            services: self.services,
            permissions: self.permissions,
            tools: self.tools,
            environment: self.environment,
        })
    }
}

/// Push `value` unless its key already exists. A repeated key with an equal
/// value is a duplicate; with a different value, a conflict.
fn dedup_push<T: PartialEq>(
    into: &mut Vec<T>,
    value: T,
    class: &'static str,
    key_of: impl Fn(&T) -> String,
) -> Result<(), PolicyError> {
    let key = key_of(&value);
    if let Some(existing) = into.iter().find(|item| key_of(item) == key) {
        return if *existing == value {
            Err(PolicyError::DuplicateContributor { class, key })
        } else {
            Err(PolicyError::ConflictingContributor { class, key })
        };
    }
    into.push(value);
    Ok(())
}

/// An immutable, content-addressable authority snapshot for one physical
/// attempt. There are no mutators and no public fields: once built it cannot
/// change, so its receipt is stable. It holds identities, typed decisions, and
/// digests only — never a live handle, a raw secret, or a raw environment
/// value.
///
/// # No `Clone`, no equality, no `Debug`
///
/// Root correction #4 says ONE immutable receipted authority graph per physical
/// attempt, and the derived traits each broke that in their own way:
///
/// - **`Clone`** made the graph a value to scatter. One built snapshot could
///   be duplicated and spent against two different provider requests, so "one
///   authority per attempt" held only by convention. It is now single-owner, so
///   the R2 consumer that takes it BY VALUE spends it exactly once. F1A ships no
///   such consumer — see the DEFERRED TO R2 note in `grapt_provider::authority`
///   for why the Round-10 attempt at one was deleted rather than repaired.
/// - **`PartialEq`/`Eq`** compared the contained [`ResolvedModelTarget`],
///   inheriting its oracle over the catalog's `base_model`/`family`. Comparing
///   two snapshots is not a question the boundary needs; comparing their
///   [`receipt`](Self::receipt)s is, and a receipt carries only endpoint
///   coordinates, typed labels and digests.
/// - **`Debug`** printed the same target, including its derived capability,
///   whose canonical form is the literal `…|base=…|family=…|…`. It is absent
///   rather than redacted so a later field cannot reopen it silently.
#[allow(
    missing_debug_implementations,
    reason = "a formatter over an authority graph is a read channel; see the type note"
)]
pub struct PhysicalAttemptAuthoritySnapshot {
    model: ResolvedModelTarget,
    config: Vec<ConfigLayer>,
    credentials: Vec<CredentialFingerprint>,
    services: Vec<ServiceActivation>,
    permissions: Vec<PermissionDecision>,
    tools: Vec<ToolVisibility>,
    environment: Vec<EnvExposure>,
}

impl PhysicalAttemptAuthoritySnapshot {
    /// A fresh builder.
    pub fn builder() -> SnapshotBuilder {
        SnapshotBuilder::new()
    }

    /// The model coordinates this attempt runs under.
    pub fn model(&self) -> &ResolvedModelTarget {
        &self.model
    }

    /// Whether a tool is visible under this authority: true only when a
    /// [`ToolVisibility`] entry names it AND marks it visible. This is the sole
    /// policy query a request-boundary needs to validate offered tools — it
    /// answers a single tool-name decision and exposes NO permission decision,
    /// credential, or grant material.
    ///
    /// [`ToolVisibility`]: crate::ToolVisibility
    pub fn tool_is_visible(&self, tool: &ToolName) -> bool {
        self.tools
            .iter()
            .any(|entry| entry.tool == *tool && entry.visible)
    }

    /// Lower the snapshot to its canonical root node. Every collection is an
    /// order-independent set; the top level is a sorted-key map. This is the
    /// single source of the receipt bytes.
    fn root(&self) -> Node {
        Node::Map(vec![
            ("model".into(), model_node(self.model.model_ref())),
            ("config".into(), set_of(&self.config, ConfigLayer::to_node)),
            (
                "credentials".into(),
                set_of(&self.credentials, CredentialFingerprint::to_node),
            ),
            (
                "services".into(),
                set_of(&self.services, ServiceActivation::to_node),
            ),
            (
                "permissions".into(),
                set_of(&self.permissions, PermissionDecision::to_node),
            ),
            ("tools".into(), set_of(&self.tools, ToolVisibility::to_node)),
            (
                "environment".into(),
                set_of(&self.environment, EnvExposure::to_node),
            ),
        ])
    }

    /// The content-addressed receipt: the identity and its digest.
    /// Deterministic and pure.
    pub fn receipt(&self) -> Result<EffectiveGraphReceipt, PolicyError> {
        let root = self.root();
        let digest = content_digest(&root);
        let id = EffectiveGraphId::new(format!("efg_{}", digest.to_hex()))
            .ok_or(PolicyError::EmptyIdentity)?;
        Ok(EffectiveGraphReceipt {
            id,
            digest_hex: digest.to_hex(),
        })
    }
}

fn set_of<T>(items: &[T], to_node: impl Fn(&T) -> Node) -> Node {
    Node::Set(items.iter().map(to_node).collect())
}

/// The immutable receipt for a resolved effective graph: the identity and its
/// digest, and nothing else.
///
/// # `canonical_bytes` is gone, not hidden
///
/// It used to publish the whole preimage. Two things were wrong with that. The
/// smaller one is that the preimage now contains keyed contribution identities,
/// and publishing them would be exactly the "byte accessor" the ruling denies
/// [`ContributionId`](crate::ContributionId) — a caller could read out through
/// the receipt what the type itself refuses. The larger one is that the accessor
/// existed to let a holder re-derive the digest, which is the same act an
/// attacker performs when guessing; a receipt that hands out its own preimage
/// has no secret to keep no matter what the preimage contains.
///
/// The field is not retained at all, rather than retained and demoted, because
/// a private field is one accessor away from being public again and this one had
/// already been public once.
///
/// What remains is safe under the ruling's exact condition: the whole
/// `EffectiveGraphId`/digest may be exposed once no unkeyed secret-derived bytes
/// enter its preimage. After [`crate::authority`], none do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectiveGraphReceipt {
    id: EffectiveGraphId,
    digest_hex: String,
}

impl EffectiveGraphReceipt {
    /// The content-addressed identity.
    pub fn id(&self) -> &EffectiveGraphId {
        &self.id
    }

    /// Lowercase hex of the content digest.
    pub fn digest_hex(&self) -> &str {
        &self.digest_hex
    }
}
