//! The optional linked StateEngine a private composition root may supply.
//! The open build supplies none and links no engine.
use std::sync::Arc;

use grapt_stateengine_api::{HostConfig, HostEpoch, HostFactory};

/// Builds the supplied engine's host factory on the server's runtime.
pub type EmbeddedFactoryBuilder = fn(tokio::runtime::Handle) -> Arc<dyn HostFactory>;

/// A linked engine with trusted composition values, never model tool arguments.
#[derive(Clone, Debug)]
pub struct StateEngine {
    pub factory: EmbeddedFactoryBuilder,
    pub config: HostConfig,
    /// Reads the engine's epoch once, after read-only preparation and before activation.
    pub epoch: fn(&HostConfig) -> Result<HostEpoch, String>,
}
