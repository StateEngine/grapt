//! External plugin compatibility without importing JavaScript ownership into Rust.
//!
//! A plugin is admitted from an explicit, provenance-bearing declaration and a
//! pinned source snapshot, loaded into its own supervised bridge process, and
//! published with its peers as an immutable generation.
//! Rust retains domain, permission, session, provider, and durable-event ownership.

mod access;
pub mod claude;
mod declaration;
mod generation;
mod protocol;
mod source;
mod supervisor;

pub use access::PluginAccess;
pub use declaration::{
    resolve_declarations, ConfigDocument, DeclarationError, PluginDeclaration, PluginOrigin,
    PluginSource, ResolvedDeclarations,
};
pub use generation::{
    ActivePlugin, PluginCallback, PluginGeneration, PluginGenerationId, PluginRegistration,
    PluginTool, PluginToolRoute,
};
pub use protocol::{
    BridgeLimits, HostOperation, SupervisorBudget, DEFAULT_BRIDGE_LIMITS,
    DEFAULT_SUPERVISOR_BUDGET, MEMORY_MAX_HEADROOM_BYTES, PROTOCOL_VERSION,
};
pub use source::{PinnedFile, SnapshotError, SourceSnapshot};
pub use supervisor::{
    ActiveGrant, BridgeRuntime, CallAuthority, CandidateGeneration, ContainmentKind, GrantScope,
    PluginError, PluginHost, PluginHostContext, PluginSupervisor, RecoveryJournal, RuntimeIdentity,
    RuntimePin,
};

/// Exists only for this compile-time witness (SC-MAC-02b): the authority to
/// signal a crashed supervisor's bridge group cannot be forged. The type can
/// be named, but outside the journal no code can construct it.
///
/// ```compile_fail,E0451
/// let _ = grapt_plugin::reclaim_witness::RecordedGroup {
///     group: 2,
///     sentinel: unimplemented!(),
///     owner: 3,
/// };
/// ```
///
/// ```
/// fn named(_: Option<grapt_plugin::reclaim_witness::RecordedGroup>) {}
/// named(None);
/// ```
#[cfg(unix)]
#[doc(hidden)]
pub mod reclaim_witness {
    pub use crate::supervisor::RecordedGroup;
}
