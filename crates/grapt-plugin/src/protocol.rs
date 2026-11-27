use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 2;

/// Gap between the observed RSS termination threshold (`rss_bytes`) and the
/// hard cgroup memory ceiling the unit is given. Invariant: the threshold
/// always sits strictly below the ceiling (threshold < MemoryMax), the gap
/// is probe-derived, and the ceiling is a bounded backstop, not unlimited
/// memory. At the base, MemoryMax equaled the threshold and the watchdog
/// evaluates its OOM branch before its threshold branch, so a ceiling hit
/// became the first attributable cause of death and preempted the observed
/// threshold (field: CCM battery run 20260926000830-85b6f235c07245de, the
/// rss witness at its explicit 128 MB limit failed with the boundary
/// message; the PSF battery at loadavg ~10 hit the default 256 MB
/// threshold-as-ceiling on 22/27 spawns, and the rerun showed the crossing
/// is a load transient, not steady state). Derived from the 28-parallel
/// Node v24.21.0 startup-peak probe (receipt
/// docs/plugin-linux-reap/receipts/plrrss-28.json: 28/28 units peaked at
/// 11.8-13.4 MiB, 0 OOM events, under a non-interfering 1 GiB ceiling)
/// plus the stated 128 MiB margin, rounded up to a 16 MiB multiple:
/// 13.4 + 128 = 141.4 -> 144 MiB. The margin keeps the observed threshold
/// the first attributable trigger at the witness's paced allocation rate
/// and bounds a runaway bridge at threshold + 144 MiB even if the watchdog
/// stalls; PLR103 zeroes the gap and turns the boundary message back into
/// the first cause of death. See docs/plugin-linux-reap/CONTRACT.md.
pub const MEMORY_MAX_HEADROOM_BYTES: u64 = 144 * 1024 * 1024;

/// Bounds enforced for one plugin bridge process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct BridgeLimits {
    pub frame_bytes: usize,
    pub pending_responses: usize,
    pub registrations: usize,
    pub schema_bytes: usize,
    pub startup_millis: u64,
    pub call_millis: u64,
    pub cleanup_millis: u64,
    /// Post-kill bound: after the grace expires and the launcher is killed,
    /// wait this long for the reap before reporting the distinct
    /// survives-kill outcome. Total termination bound is cleanup + reap.
    pub reap_millis: u64,
    pub rss_bytes: u64,
    pub cpu_percent: u16,
    pub cpu_violation_samples: u16,
    pub sample_millis: u64,
    pub tasks: u16,
}

pub const DEFAULT_BRIDGE_LIMITS: BridgeLimits = BridgeLimits {
    frame_bytes: 1024 * 1024,
    pending_responses: 8,
    registrations: 128,
    schema_bytes: 256 * 1024,
    startup_millis: 10_000,
    call_millis: 120_000,
    cleanup_millis: 5_000,
    reap_millis: 5_000,
    rss_bytes: 256 * 1024 * 1024,
    cpu_percent: 90,
    cpu_violation_samples: 20,
    sample_millis: 100,
    tasks: 64,
};

/// Bounds shared by every bridge process one supervisor owns, including a
/// candidate generation starting beside the generation it may replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorBudget {
    pub processes: usize,
    pub startup_concurrency: usize,
    pub rss_bytes: u64,
    pub cpu_percent: u16,
    pub tasks: u16,
}

pub const DEFAULT_SUPERVISOR_BUDGET: SupervisorBudget = SupervisorBudget {
    processes: 16,
    startup_concurrency: 4,
    rss_bytes: 2 * 1024 * 1024 * 1024,
    cpu_percent: 200,
    tasks: 512,
};

#[derive(Debug, Serialize)]
pub(crate) struct Request<'a> {
    pub version: u16,
    pub generation: u64,
    pub correlation: u64,
    pub operation: &'a str,
    pub payload: serde_json::Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Response {
    pub version: u16,
    pub generation: u64,
    pub correlation: u64,
    pub outcome: String,
    #[serde(default)]
    pub payload: serde_json::Value,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HostOperation {
    NetworkFetch,
    AgentList,
    AgentGet,
    AgentReload,
    CatalogProviderList,
    CatalogProviderGet,
    CatalogModelList,
    CatalogModelDefault,
    CatalogReload,
    CommandList,
    CommandReload,
    EventSubscribe,
    IntegrationList,
    IntegrationGet,
    IntegrationConnectKey,
    IntegrationOauthConnect,
    IntegrationOauthStatus,
    IntegrationOauthComplete,
    IntegrationOauthCancel,
    IntegrationCommandConnect,
    IntegrationCommandStatus,
    IntegrationCommandCancel,
    IntegrationReload,
    IntegrationConnectionActive,
    IntegrationConnectionResolve,
    PluginList,
    ReferenceList,
    ReferenceReload,
    SessionCreate,
    SessionGet,
    SessionPrompt,
    SessionGenerate,
    SessionCommand,
    SessionSynthetic,
    SessionInterrupt,
    SkillList,
    SkillReload,
    WebsearchProviders,
    WebsearchSelectionGet,
    WebsearchSelectionPut,
    WebsearchQuery,
    WebsearchReload,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct HostRequest {
    pub kind: HostFrameKind,
    pub version: u16,
    pub generation: u64,
    pub correlation: u64,
    pub operation: HostOperation,
    #[serde(default)]
    pub payload: serde_json::Value,
    /// The host-issued grant the bridge propagated into this request. It is a
    /// claim until the supervisor proves the grant is live on this process.
    #[serde(default)]
    pub authority: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum HostFrameKind {
    HostRequest,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum BridgeFrame {
    Response(Response),
    HostRequest(HostRequest),
}

#[derive(Debug, Serialize)]
pub(crate) struct HostResponse<'a> {
    pub version: u16,
    pub generation: u64,
    pub correlation: u64,
    pub operation: &'a str,
    pub payload: HostResponsePayload,
}

#[derive(Debug, Serialize)]
pub(crate) struct HostResponsePayload {
    pub outcome: &'static str,
    pub payload: serde_json::Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}
