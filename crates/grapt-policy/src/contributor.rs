//! The typed authority contributors. Each class is a distinct value type that
//! lowers to a canonical `Node`; none carries a live handle, a raw secret, or
//! a raw environment value — only identities and typed decisions.
//!
//! The two classes whose content is secret-bearing — [`EnvExposure`] and
//! [`ConfigLayer`] — carry a keyed [`ContributionId`] minted by an injected
//! [`ContributionAuthority`]. They used to carry a public unkeyed `Digest`,
//! which was an offline equality oracle over exactly the material it was meant
//! to protect; see [`crate::authority`].
//!
//! Every contributor exposes a `conflict_key`: the identity under which the
//! builder deduplicates. Two contributors of the same class with equal
//! `conflict_key` but non-equal value are a typed conflict, never a silent
//! last-writer-wins.

use crate::authority::{ContributionAuthority, ContributionClass, ContributionId};
use crate::encoding::Node;
use crate::secret::Secret;
use grapt_types::{
    AuthoritySource, CapabilityId, ProjectId, ProviderId, ServiceGeneration, ServiceSlotId,
    SessionId,
};

/// A non-empty tool name (capability-scoped tool visibility vocabulary; owned
/// here because tool visibility is a policy concept).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ToolName(String);

impl ToolName {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A non-empty environment variable name. Only the NAME is exposed; the value
/// enters as a keyed [`ContributionId`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct EnvVarName(String);

impl EnvVarName {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A non-empty custom config-source label. A built-in source is a variant of
/// [`ConfigSource`]; a custom one carries this checked name and is encoded under
/// a distinct discriminant, so `Custom("default")` can never collide with the
/// built-in `Default`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CustomSourceName(String);

impl CustomSourceName {
    pub fn new(raw: impl Into<String>) -> Option<Self> {
        let raw = raw.into();
        if raw.is_empty() {
            None
        } else {
            Some(Self(raw))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The model ENDPOINT the attempt runs against, lowered to a canonical node.
///
/// # Endpoint coordinates only, and why nothing else may be added here
///
/// This note used to say "the canonical bytes are public, so anything encoded
/// here is world-readable". They are no longer public — see
/// [`EffectiveGraphReceipt`](crate::EffectiveGraphReceipt) — and the rule is
/// unchanged, because withholding the bytes was never what made an encoded
/// coordinate safe. The whole-graph digest is still public and still
/// deterministic, so any coordinate with a small enough value space can be
/// recovered by recomputing the graph over candidates. Two things therefore stay
/// out: the catalog's `base_model` and `family`, the facts `agent default` may
/// not cross on; and the derived `CapabilityId`, whose canonical form is the
/// literal `…|base=…|family=…|…` and is therefore those same two facts in
/// plaintext.
///
/// Neither can be hidden behind an unkeyed digest here. The family space is small
/// enough to enumerate offline, so a digest of a family name is an oracle with
/// extra steps — the same finding that removed the two contributor digests. If a
/// later wave needs these coordinates in the graph, the route is a keyed
/// [`ContributionId`], not a hash.
///
/// # DEFERRED TO R2 — an exact resolved-target receipt
///
/// This node includes
/// the route, account, credential version, policy version, catalog release and a
/// capability-derivation scheme label, and claimed the result was an exact receipt for
/// the resolved target. It was not, and the reason is worth keeping: the catalog VERSION
/// is audit text, and a repeated load mints a DISTINCT snapshot, so the same version and
/// endpoint carrying different `base`/`family`/capability encode identically. Round 10
/// recorded that as an ambiguity made unreachable by the issuance check — which is true
/// of admission and irrelevant to what the receipt asserts. An inexact receipt that
/// claims exactness is worse than an endpoint receipt that claims only what it is.
///
/// What R2 owes: a registry-MINTED opaque or keyed exact-target receipt that
/// leaks no family, base model, capability canonical form or secret material.
/// Until it exists, this node records the endpoint the attempt ran against and
/// nothing more, and provenance beyond that is enforced by
/// `ResolvedModelTarget::is_same_issuance` at admission — not by this digest.
///
/// HONEST CONSEQUENCE, unchanged and restated: a catalog release that reassigns an
/// endpoint to a different family does NOT change this receipt. The change is
/// observed on the authority side, because a target resolved under the new release
/// is not the same issuance and cannot enter or share a graph resolved under the
/// old one.
pub(crate) fn model_node(model: &grapt_types::ModelRef) -> Node {
    Node::Map(vec![
        (
            "provider".into(),
            Node::Str(model.provider().as_str().to_owned()),
        ),
        ("model".into(), Node::Str(model.model().as_str().to_owned())),
        // Absent variant is an empty option, distinct from an empty string.
        (
            "variant".into(),
            option_node(model.variant().map(|v| v.as_str())),
        ),
    ])
}

/// Where a config value was resolved from. A closed vocabulary: a built-in
/// source is its own variant; an unknown one is `Custom(name)` encoded under a
/// distinct discriminant, never conflated with a built-in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigSource {
    Default,
    GlobalFile,
    ProjectFile,
    Environment,
    CommandLine,
    Custom(CustomSourceName),
}

impl ConfigSource {
    fn to_node(&self) -> Node {
        // The discriminant is a separate field so no built-in's label can be
        // spoofed by a custom name.
        let (discriminant, name): (&str, Option<&str>) = match self {
            ConfigSource::Default => ("default", None),
            ConfigSource::GlobalFile => ("global-file", None),
            ConfigSource::ProjectFile => ("project-file", None),
            ConfigSource::Environment => ("environment", None),
            ConfigSource::CommandLine => ("command-line", None),
            ConfigSource::Custom(name) => ("custom", Some(name.as_str())),
        };
        Node::Map(vec![
            ("kind".into(), Node::Str(discriminant.to_owned())),
            ("name".into(), option_node(name)),
        ])
    }
}

/// One config-provenance layer at an explicit precedence `rank`. Precedence is
/// the EXPLICIT rank, never insertion order, so provenance supplied in any
/// order resolves to the same graph. Two layers at the same rank with different
/// content are a precedence conflict.
///
/// # Why the content is a keyed identity and not a digest
///
/// This field held a public `Digest` over the layer's bytes. A config layer is
/// a canonical secret carrier — tokens, endpoints, per-account settings — and a
/// published unkeyed digest of it is an offline guessing oracle for any value
/// whose space is small enough to enumerate, which is most of them. Master §8.4
/// forbids that, and the ruling applies it to `ConfigLayer` and not only to
/// [`EnvExposure`]. See [`crate::authority`].
///
/// The fields are private because the constructor is the whole seal: a public
/// `content` field would let a caller move an identity minted for one layer
/// onto another, and a public `rank`/`source` would let one be relabelled after
/// its identity was minted against the old coordinate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLayer {
    rank: u64,
    source: ConfigSource,
    content: ContributionId,
}

impl ConfigLayer {
    /// Resolve one config layer under an injected identity authority.
    ///
    /// `content` is the layer's canonical bytes. It is a [`Secret`] by type
    /// rather than a `&[u8]` deliberately: a config blob is secret-bearing by
    /// default in this design, and a caller who believes otherwise should have
    /// to say so at the call site rather than by picking the cheaper argument.
    pub fn new(
        authority: &ContributionAuthority,
        rank: u64,
        source: ConfigSource,
        content: &Secret,
    ) -> Self {
        Self {
            rank,
            // The coordinate is the rank: it is what makes two layers distinct
            // slots, and binding it into the mint means an identity resolved for
            // rank 5 does not verify as the same contribution at rank 6.
            content: authority.mint(ContributionClass::ConfigContent, &rank.to_string(), content),
            source,
        }
    }

    pub(crate) fn rank(&self) -> u64 {
        self.rank
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            ("rank".into(), Node::U64(self.rank)),
            ("source".into(), self.source.to_node()),
            (
                "content".into(),
                Node::Bytes(self.content.as_bytes().to_vec()),
            ),
        ])
    }
}

/// A credential's provider identity plus the record and version that own it.
///
/// Master §8.4: an authority graph names a credential record/version, never a
/// digest of the raw secret. Rotation advances the version, which is the
/// change a graph receipt must observe; the material itself never appears and
/// no value here can be used as an offline equality oracle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialFingerprint {
    pub provider: ProviderId,
    pub record: grapt_types::CredentialRecordId,
    pub version: grapt_types::CredentialVersion,
}

impl CredentialFingerprint {
    pub(crate) fn conflict_key(&self) -> String {
        self.provider.as_str().to_owned()
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            (
                "provider".into(),
                Node::Str(self.provider.as_str().to_owned()),
            ),
            ("record".into(), Node::Str(self.record.as_str().to_owned())),
            (
                "version".into(),
                Node::Str(self.version.as_str().to_owned()),
            ),
        ])
    }
}

/// Whether an activated service is a plugin or an MCP server. A closed
/// vocabulary: the two are distinct authority classes and must never collapse
/// onto one slot/generation pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceKind {
    Plugin,
    Mcp,
}

impl ServiceKind {
    fn label(self) -> &'static str {
        match self {
            ServiceKind::Plugin => "plugin",
            ServiceKind::Mcp => "mcp",
        }
    }
}

/// A plugin/MCP activation: a kind + slot at a monotonic generation. The kind
/// participates in identity and encoding, so a plugin and an MCP server sharing
/// a raw slot and generation are distinct and do not conflict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceActivation {
    pub kind: ServiceKind,
    pub slot: ServiceSlotId,
    pub generation: ServiceGeneration,
}

impl ServiceActivation {
    pub(crate) fn conflict_key(&self) -> String {
        format!("{}:{}", self.kind.label(), self.slot.as_str())
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            ("kind".into(), Node::Str(self.kind.label().to_owned())),
            ("slot".into(), Node::Str(self.slot.as_str().to_owned())),
            ("generation".into(), Node::U64(self.generation.value())),
        ])
    }
}

/// The settled decision for a capability, carrying its authority SCOPE. A bare
/// "session" or "always" would erase which session or project the authority
/// binds to, so two different scoped grants would hash identically — the scope
/// is part of the decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PermissionScope {
    /// Allowed once, for this attempt only.
    Once,
    /// Allowed for the lifetime of a specific session.
    Session(SessionId),
    /// Saved for a specific project.
    Always(ProjectId),
    /// Refused.
    Rejected,
}

impl PermissionScope {
    fn to_node(&self) -> Node {
        // Discriminant + the scoped identity, so Session(a) and Session(b)
        // differ, and Once/Rejected carry an explicit empty scope.
        let (discriminant, scope): (&str, Option<&str>) = match self {
            PermissionScope::Once => ("once", None),
            PermissionScope::Session(session) => ("session", Some(session.as_str())),
            PermissionScope::Always(project) => ("always", Some(project.as_str())),
            PermissionScope::Rejected => ("rejected", None),
        };
        Node::Map(vec![
            ("decision".into(), Node::Str(discriminant.to_owned())),
            ("scope".into(), option_node(scope)),
        ])
    }
}

/// A permission decision: the capability, the authority that settled it, and
/// the scoped decision itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionDecision {
    pub capability: CapabilityId,
    pub authority: AuthoritySource,
    pub scope: PermissionScope,
}

impl PermissionDecision {
    pub(crate) fn conflict_key(&self) -> String {
        self.capability.as_str().to_owned()
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            (
                "capability".into(),
                Node::Str(self.capability.as_str().to_owned()),
            ),
            ("authority".into(), authority_node(&self.authority)),
            ("scope".into(), self.scope.to_node()),
        ])
    }
}

/// Whether a tool is visible under a required capability scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolVisibility {
    pub tool: ToolName,
    pub required: CapabilityId,
    pub visible: bool,
}

impl ToolVisibility {
    pub(crate) fn conflict_key(&self) -> String {
        self.tool.as_str().to_owned()
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            ("tool".into(), Node::Str(self.tool.as_str().to_owned())),
            (
                "required".into(),
                Node::Str(self.required.as_str().to_owned()),
            ),
            ("visible".into(), Node::U64(u64::from(self.visible))),
        ])
    }
}

/// An exposed environment variable: its NAME plus a keyed opaque identity for
/// its value. The value bytes never appear, and neither does anything a holder
/// of the receipt could compare a guess against.
///
/// # Why not a credential record/version, like [`CredentialFingerprint`]
///
/// Because an environment value has no record. `CredentialFingerprint` names a
/// stored credential whose owner already versions it, so rotation is observable
/// through the version alone and no derived value is needed. An environment
/// value is raw content supplied per attempt: the only thing that can observe
/// its rotation is something derived from the value itself. That is precisely
/// the case the ruling assigns to the keyed opaque identity, and precisely why
/// blanking the field was not an option — it would have removed the oracle and
/// the rotation signal together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvExposure {
    name: EnvVarName,
    value: ContributionId,
}

impl EnvExposure {
    /// Resolve one exposed environment variable under an injected identity
    /// authority. The value is consumed as a [`Secret`] and never retained.
    pub fn new(authority: &ContributionAuthority, name: EnvVarName, value: &Secret) -> Self {
        Self {
            value: authority.mint(ContributionClass::EnvValue, name.as_str(), value),
            name,
        }
    }

    /// The variable's NAME. Exposed because which variables an attempt runs
    /// under is authority information a reader legitimately needs; the value is
    /// what must not be recoverable.
    pub fn name(&self) -> &EnvVarName {
        &self.name
    }

    pub(crate) fn conflict_key(&self) -> String {
        self.name.as_str().to_owned()
    }

    pub(crate) fn to_node(&self) -> Node {
        Node::Map(vec![
            ("name".into(), Node::Str(self.name.as_str().to_owned())),
            ("value".into(), Node::Bytes(self.value.as_bytes().to_vec())),
        ])
    }
}

/// Optional presence encoded as a 0-or-1-element sequence — absent is an empty
/// option, never an empty string.
fn option_node(value: Option<&str>) -> Node {
    match value {
        None => Node::Seq(Vec::new()),
        Some(v) => Node::Seq(vec![Node::Str(v.to_owned())]),
    }
}

// Adapted to the corrected `AuthoritySource` vocabulary (amendment §8.2 /
// survey row C3), which supersedes the two-variant `{Layer, Fallback}` shape.
// The encoded node keeps its previous `kind` strings so the effective-graph
// digest stays comparable; the new variants each contribute the exact version
// or identity that a bare `fallback` marker used to hide.
fn authority_node(authority: &AuthoritySource) -> Node {
    match authority {
        AuthoritySource::DirectReply => {
            Node::Map(vec![("kind".into(), Node::Str("direct-reply".into()))])
        }
        AuthoritySource::DefaultPolicy { policy } => Node::Map(vec![
            ("kind".into(), Node::Str("fallback".into())),
            ("policy".into(), Node::Str(policy.as_str().to_owned())),
        ]),
        AuthoritySource::SavedRule { policy } => Node::Map(vec![
            ("kind".into(), Node::Str("saved-rule".into())),
            ("policy".into(), Node::Str(policy.as_str().to_owned())),
        ]),
        AuthoritySource::Hook { hook } => Node::Map(vec![
            ("kind".into(), Node::Str("hook".into())),
            ("name".into(), Node::Str(hook.as_str().to_owned())),
        ]),
        AuthoritySource::PolicyLayer { layer } => Node::Map(vec![
            ("kind".into(), Node::Str("layer".into())),
            ("name".into(), Node::Str(layer.as_str().to_owned())),
        ]),
    }
}

#[cfg(test)]
mod tests {
    //! The BYTE-level half of the secret-absence law.
    //!
    //! The snapshot-level witness cannot run at all in this wave —
    //! `SnapshotBuilder::model` requires a registry-issued `ResolvedModelTarget`
    //! and nothing can issue one — so it stays in the pending inventory. But the
    //! law it names does not need a whole snapshot to be witnessed: a secret can
    //! only reach the canonical stream through a contributor's own node, and
    //! that node is reachable here, inside the crate, with no visibility widened
    //! and no test-only feature added.

    use super::*;
    use crate::authority::{AuthorityRealm, KeyVersion};
    use crate::encoding::canonical_bytes;

    const SENTINEL: &[u8] = b"SENTINEL_TOKEN_do_not_leak_9d3f";

    fn authority() -> ContributionAuthority {
        let realm = match AuthorityRealm::new("grapt-test") {
            Some(v) => v,
            None => panic!("realm"),
        };
        let version = match KeyVersion::new("v1") {
            Some(v) => v,
            None => panic!("version"),
        };
        ContributionAuthority::new(realm, version, Secret::new(b"owner-held".to_vec()))
    }

    fn env_name(raw: &str) -> EnvVarName {
        match EnvVarName::new(raw) {
            Some(v) => v,
            None => panic!("env var name"),
        }
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn an_environment_value_never_reaches_the_canonical_bytes() {
        let exposure = EnvExposure::new(
            &authority(),
            env_name("GRAPT_TOKEN"),
            &Secret::new(SENTINEL.to_vec()),
        );
        let bytes = canonical_bytes(&exposure.to_node());
        assert!(
            !contains(&bytes, SENTINEL),
            "the value reached the canonical stream"
        );
        // The NAME is present on purpose, so the assertion above is not passing
        // because the node is empty.
        assert!(contains(&bytes, b"GRAPT_TOKEN"));
    }

    #[test]
    fn a_config_layers_content_never_reaches_the_canonical_bytes() {
        let layer = ConfigLayer::new(
            &authority(),
            5,
            ConfigSource::ProjectFile,
            &Secret::new(SENTINEL.to_vec()),
        );
        let bytes = canonical_bytes(&layer.to_node());
        assert!(
            !contains(&bytes, SENTINEL),
            "the layer content reached the canonical stream"
        );
        assert!(contains(&bytes, b"project-file"));
    }

    #[test]
    fn an_unkeyed_digest_of_the_value_is_not_in_the_canonical_bytes_either() {
        // The defect this closes was not "the raw bytes appear" — they never
        // did. It was that an UNKEYED SHA-256 of them appeared, which a holder
        // could recompute from a guess. So the witness that matters asserts the
        // absence of the DIGEST, not of the material; asserting the material
        // alone would have passed against the defective code.
        use sha2::Digest as _;
        let unkeyed: [u8; 32] = sha2::Sha256::digest(SENTINEL).into();
        let exposure = EnvExposure::new(
            &authority(),
            env_name("GRAPT_TOKEN"),
            &Secret::new(SENTINEL.to_vec()),
        );
        assert!(
            !contains(&canonical_bytes(&exposure.to_node()), &unkeyed),
            "an unkeyed digest of the value is an offline oracle and is back in the stream"
        );
    }

    #[test]
    fn two_variables_holding_one_value_do_not_share_an_identity() {
        // The name is the coordinate, so the same secret under two names mints
        // two identities. Without it, a reader could learn that two variables
        // hold the same value by comparing the graph.
        let authority = authority();
        let value = Secret::new(SENTINEL.to_vec());
        let one = EnvExposure::new(&authority, env_name("A"), &value);
        let two = EnvExposure::new(&authority, env_name("B"), &value);
        assert_ne!(one.value, two.value);
    }

    #[test]
    fn a_config_layer_identity_is_bound_to_its_rank() {
        let authority = authority();
        let content = Secret::new(b"same-bytes".to_vec());
        let five = ConfigLayer::new(&authority, 5, ConfigSource::ProjectFile, &content);
        let six = ConfigLayer::new(&authority, 6, ConfigSource::ProjectFile, &content);
        assert_ne!(
            five.content, six.content,
            "an identity minted at one precedence must not verify at another"
        );
    }

    #[test]
    fn the_debug_rendering_of_a_contributor_carries_no_value() {
        let exposure = EnvExposure::new(
            &authority(),
            env_name("GRAPT_TOKEN"),
            &Secret::new(SENTINEL.to_vec()),
        );
        let rendered = format!("{exposure:?}");
        assert!(rendered.contains("GRAPT_TOKEN"));
        assert!(!rendered.contains("SENTINEL"));
        assert!(rendered.contains("<opaque>"));
    }
}
