//! Shared error type for plugin supervision and generation validation.

use thiserror::Error;

#[derive(Debug, Error)]
pub enum PluginError {
    #[error("plugin runtime could not be inspected: {0}")]
    Runtime(String),
    #[error("plugin bridge could not start: {0}")]
    Spawn(String),
    #[error("plugin bridge protocol refused: {0}")]
    Protocol(String),
    #[error("plugin bridge timed out during {0}")]
    Timeout(&'static str),
    #[error("plugin bridge survived kill: {0}")]
    SurvivesKill(String),
    #[error("plugin bridge registration refused: {0}")]
    Registration(String),
    #[error("plugin generation is stale")]
    StaleGeneration,
    #[error("plugin invocation failed: {0}")]
    Invocation(String),
    #[error("plugin bridge state is poisoned")]
    Poisoned,
}
