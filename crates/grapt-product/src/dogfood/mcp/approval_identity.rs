//! The immutable declaration and resolved program covered by an MCP grant.
use super::Declaration;
use std::sync::Arc;

/// A grant survives a reconnect of the same declared and resolved server.
/// Incarnations still distinguish channels; they do not invalidate consent
/// when the registry has connected that same server again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LaunchIdentity {
    pub(in crate::dogfood) program: String,
    pub(in crate::dogfood) cwd: String,
    pub(in crate::dogfood) arguments: Vec<String>,
    pub(in crate::dogfood) incarnation: u64,
    /// Shared by published tools, so retaining a grant does not copy each
    /// environment/header map per tool. Declaration's Debug redacts values.
    pub(in crate::dogfood) declaration: Arc<Declaration>,
}

impl LaunchIdentity {
    pub(in crate::dogfood) fn matches_approval(&self, approved: &Self) -> bool {
        self.declaration == approved.declaration
            && self.program == approved.program
            && self.cwd == approved.cwd
            && self.arguments == approved.arguments
    }
}

pub(in crate::dogfood) fn describe_launch(launch: &LaunchIdentity) -> String {
    format!(
        "{} in {} (connection {})",
        launch.program, launch.cwd, launch.incarnation
    )
}
