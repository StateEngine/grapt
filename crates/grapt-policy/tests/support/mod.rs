//! Shared support for the effective-authority acceptance witnesses.
//!
//! Laws (commission + review HOLD): input-order equivalence;
//! every contributor class changes the receipt on a semantic change; equivalent
//! resolved graphs hash equally despite provenance input order;
//! delimiter/collision adversaries; precedence conflicts typed and
//! deterministic; plugin vs MCP are distinct classes; config discriminant is not
//! spoofable by a custom label; permission authority SCOPE is part of the
//! decision; secret material absent from Debug/canonical bytes/receipt; the
//! graph is immutable after construction; illegal empty identities are
//! unrepresentable.
//!
//! # Released from `pending/` by L2
//!
//! These witnesses were preserved as `.rs.pending` bodies because
//! `SnapshotBuilder::model` requires a registry-issued `ResolvedModelTarget` and
//! nothing in the workspace could issue one. L2 built the issuing owner, so the
//! release condition is discharged **through the real path**: [`issue`] below
//! calls `ModelRegistry::issue`, the only public route there is.
//!
//! Nothing here widens visibility. `ResolvedModelTarget::resolve` is still
//! crate-private to `grapt-types`, there is no test-support Cargo feature, no
//! `#[cfg(test)] pub(crate)`, no `doc(hidden)` and no `unvouched()`-style mint.
//! A caller still cannot assert a base model or a family: both arrive from a
//! loaded catalog release, exactly as they do in production.
//!
//! # Why the model is coordinates rather than a target
//!
//! `ResolvedModelTarget` is deliberately not `Clone` — one issuance admits one
//! authority graph — and these witnesses clone a [`Parts`] bundle, tweak one
//! class and compare receipts. So `Parts` carries the *coordinates*, and every
//! `snapshot()` issues a fresh target from its own one-row release. That is the
//! same shape the captured helper had, where each `model_ref` call also minted
//! its own catalog, and it is faithful to what the graph actually encodes:
//! `model_node` lowers provider, model and variant, so two issuances of one
//! coordinate hash equally and the receipt witnesses stay meaningful.

// Shared by two test binaries; each uses a subset, and an unused helper in one
// of them is not a defect. Scoped to this support module so no witness file
// silently inherits it.
#![allow(dead_code)]

use grapt_policy::{
    AuthorityRealm, ConfigLayer, ConfigSource, ContributionAuthority, CredentialFingerprint,
    EnvExposure, EnvVarName, KeyVersion, PermissionDecision, PermissionScope,
    PhysicalAttemptAuthoritySnapshot, PolicyError, Secret, ServiceActivation, ServiceKind,
    SnapshotBuilder, ToolName, ToolVisibility,
};
use grapt_types::{
    AuthoritySource, CapabilityId, LayerName, ModelRegistry, ProjectId, ProviderId, RoutingRequest,
    ServiceGeneration, ServiceSlotId, SessionId,
};

pub type TestResult = Result<(), Box<dyn std::error::Error>>;

// ---- helpers (test-only; panic on a malformed literal, never unwrap/expect) -

pub fn provider(s: &str) -> ProviderId {
    match ProviderId::new(s) {
        Some(v) => v,
        None => panic!("provider id {s:?}"),
    }
}

/// The model coordinates one graph is built over.
///
/// Strings rather than ids because a `Parts` is cloned and re-issued; the ids
/// are minted at issue time from a release that declares them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelCoordinates {
    pub provider: String,
    pub model: String,
    pub variant: Option<String>,
}

pub fn coordinates(prov: &str, model: &str, variant: Option<&str>) -> ModelCoordinates {
    ModelCoordinates {
        provider: prov.to_owned(),
        model: model.to_owned(),
        variant: variant.map(str::to_owned),
    }
}

/// A registry over a one-row release declaring `coords`.
///
/// Base model and family are catalog truth, so they are declared by the release
/// rather than asserted at the call site — the family is derived from the
/// provider, which is all these witnesses need and which no witness here asks a
/// question about.
pub fn registry(coords: &ModelCoordinates) -> ModelRegistry {
    let provider = provider(&coords.provider);
    let model = match grapt_types::ModelId::new(&coords.model) {
        Some(v) => v,
        None => panic!("model id"),
    };
    let variant = coords
        .variant
        .as_deref()
        .map(|v| match grapt_types::ModelVariantId::new(v) {
            Some(x) => x,
            None => panic!("variant id"),
        });
    let base_model = match grapt_types::BaseModelId::new(&coords.model) {
        Some(x) => x,
        None => panic!("base model id"),
    };
    let family = match grapt_types::ModelFamilyId::new(&coords.provider) {
        Some(x) => x,
        None => panic!("model family id"),
    };
    let record = grapt_types::ModelCatalogRecord::new(provider, model, base_model, family, variant);
    let catalog = match grapt_types::ModelCatalogVersion::new("test-catalog")
        .and_then(|version| grapt_types::ModelCatalog::load(version, vec![record]))
    {
        Some(catalog) => catalog,
        None => panic!("test catalog"),
    };
    ModelRegistry::install(catalog)
}

pub fn routing_request() -> RoutingRequest {
    RoutingRequest {
        route: match grapt_types::ModelRouteId::new("rt_1") {
            Some(v) => v,
            None => panic!("route id"),
        },
        account: match grapt_types::AccountId::new("acct_1") {
            Some(v) => v,
            None => panic!("account id"),
        },
        credential_version: match grapt_types::CredentialVersion::new("1") {
            Some(v) => v,
            None => panic!("credential version"),
        },
        policy: match grapt_types::PolicyVersion::new("pol_1") {
            Some(v) => v,
            None => panic!("policy version"),
        },
    }
}

/// Issue a routing target for `coords` through the REAL issuance path.
///
/// This is the line the release condition is about.
pub fn issue(reg: &ModelRegistry, coords: &ModelCoordinates) -> grapt_types::ResolvedModelTarget {
    let provider = provider(&coords.provider);
    let model = match grapt_types::ModelId::new(&coords.model) {
        Some(v) => v,
        None => panic!("model id"),
    };
    let variant = coords
        .variant
        .as_deref()
        .map(|v| match grapt_types::ModelVariantId::new(v) {
            Some(x) => x,
            None => panic!("variant id"),
        });
    match reg.issue(&provider, &model, variant.as_ref(), routing_request()) {
        Ok(target) => target,
        Err(refusal) => panic!("registry refused a row it declares: {refusal:?}"),
    }
}

pub fn derived_capability(s: &str) -> Option<CapabilityId> {
    // Capability identity is derived from resolved coordinates, never authored
    // from a string (amendment §8.1). These tests model one capability per
    // action name, so the action becomes the program coordinate.
    if s.is_empty() {
        return None;
    }
    Some(CapabilityId::derive(&grapt_types::ResolvedCapability::new(
        grapt_types::EffectClass::ToolInvoke,
        Some(grapt_types::ProgramIdentity::new(
            grapt_types::PathIdentity::canonical(s),
            Vec::new(),
            grapt_types::PathIdentity::Unresolvable,
            Vec::new(),
        )),
        grapt_types::FilesystemAuthority::default(),
        grapt_types::EgressAuthority::default(),
        grapt_types::RealizedEnforcement::HarnessOnly,
    )))
}

pub fn fallback_authority() -> &'static AuthoritySource {
    // The corrected vocabulary requires a default-policy decision to name its
    // exact policy version; a bare `Fallback` marker no longer exists.
    static FALLBACK: std::sync::OnceLock<AuthoritySource> = std::sync::OnceLock::new();
    FALLBACK.get_or_init(|| AuthoritySource::DefaultPolicy {
        policy: match grapt_types::PolicyVersion::new("default") {
            Some(policy) => policy,
            None => unreachable!("non-empty policy version"),
        },
    })
}

pub fn capability(s: &str) -> CapabilityId {
    match derived_capability(s) {
        Some(v) => v,
        None => panic!("capability id {s:?}"),
    }
}

pub fn slot(s: &str) -> ServiceSlotId {
    match ServiceSlotId::new(s) {
        Some(v) => v,
        None => panic!("slot id"),
    }
}

pub fn tool_name(s: &str) -> ToolName {
    match ToolName::new(s) {
        Some(v) => v,
        None => panic!("tool name"),
    }
}

pub fn env_name(s: &str) -> EnvVarName {
    match EnvVarName::new(s) {
        Some(v) => v,
        None => panic!("env name"),
    }
}

pub fn layer(s: &str) -> AuthoritySource {
    match LayerName::new(s) {
        Some(layer) => AuthoritySource::PolicyLayer { layer },
        None => panic!("layer name"),
    }
}

/// ONE identity authority for the whole inventory, with a fixed realm and key
/// version.
///
/// That is not test convenience — it is the law under test. A per-call or
/// randomly keyed authority would mint a different identity for the same value
/// every time, and every receipt-equality witness here would fail for a reason
/// that has nothing to do with what it is asserting. The stable realm/key-version
/// is what keeps the graph content-addressed.
pub fn authority() -> &'static ContributionAuthority {
    static AUTHORITY: std::sync::OnceLock<ContributionAuthority> = std::sync::OnceLock::new();
    AUTHORITY.get_or_init(|| {
        let realm = match AuthorityRealm::new("grapt-pending") {
            Some(v) => v,
            None => unreachable!("non-empty realm"),
        };
        let version = match KeyVersion::new("v1") {
            Some(v) => v,
            None => unreachable!("non-empty key version"),
        };
        ContributionAuthority::new(realm, version, Secret::new(b"owner-held-key".to_vec()))
    })
}

pub fn config_layer(rank: u64, source: ConfigSource, content: &[u8]) -> ConfigLayer {
    ConfigLayer::new(authority(), rank, source, &Secret::new(content.to_vec()))
}

pub fn env_exposure(name: &str, value: &[u8]) -> EnvExposure {
    EnvExposure::new(authority(), env_name(name), &Secret::new(value.to_vec()))
}

pub fn session(s: &str) -> SessionId {
    match s.parse() {
        Ok(v) => v,
        Err(e) => panic!("session id {s:?}: {e}"),
    }
}

pub fn project(s: &str) -> ProjectId {
    match ProjectId::new(s) {
        Some(v) => v,
        None => panic!("project id {s:?}"),
    }
}

/// A cloneable bundle of contributors. Tests clone it, tweak one class, and
/// compare receipts.
#[derive(Clone)]
pub struct Parts {
    pub model: ModelCoordinates,
    pub config: Vec<ConfigLayer>,
    pub credentials: Vec<CredentialFingerprint>,
    pub services: Vec<ServiceActivation>,
    pub permissions: Vec<PermissionDecision>,
    pub tools: Vec<ToolVisibility>,
    pub environment: Vec<EnvExposure>,
}

pub fn baseline() -> Parts {
    Parts {
        model: coordinates("anthropic", "claude-fable-5", Some("2026")),
        config: vec![
            config_layer(0, ConfigSource::Default, b"default-config"),
            config_layer(10, ConfigSource::ProjectFile, b"project-config"),
        ],
        credentials: vec![CredentialFingerprint {
            provider: provider("anthropic"),
            record: match grapt_types::CredentialRecordId::new("cred-1") {
                Some(v) => v,
                None => panic!("credential record id"),
            },
            version: match grapt_types::CredentialVersion::new("1") {
                Some(v) => v,
                None => panic!("credential version"),
            },
        }],
        services: vec![ServiceActivation {
            kind: ServiceKind::Plugin,
            slot: slot("plugin.git"),
            generation: ServiceGeneration::new(3),
        }],
        permissions: vec![PermissionDecision {
            capability: capability("fs.read"),
            authority: layer("owner-floor"),
            scope: PermissionScope::Always(project("prj_main")),
        }],
        tools: vec![ToolVisibility {
            tool: tool_name("bash"),
            required: capability("shell.exec"),
            visible: true,
        }],
        environment: vec![env_exposure("PATH", b"/usr/bin:/bin")],
    }
}

impl Parts {
    pub fn snapshot(&self) -> Result<PhysicalAttemptAuthoritySnapshot, PolicyError> {
        let registry = registry(&self.model);
        let mut b = SnapshotBuilder::new();
        b.model(issue(&registry, &self.model))?;
        for layer in &self.config {
            b.config_layer(layer.clone())?;
        }
        for cred in &self.credentials {
            b.credential(cred.clone())?;
        }
        for service in &self.services {
            b.service(service.clone())?;
        }
        for perm in &self.permissions {
            b.permission(perm.clone())?;
        }
        for tool in &self.tools {
            b.tool(tool.clone())?;
        }
        for env in &self.environment {
            b.environment(env.clone())?;
        }
        b.build()
    }

    pub fn receipt_id(&self) -> Result<String, Box<dyn std::error::Error>> {
        Ok(self.snapshot()?.receipt()?.id().as_str().to_owned())
    }
}

pub fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || needle.len() > haystack.len() {
        return false;
    }
    haystack.windows(needle.len()).any(|w| w == needle)
}
