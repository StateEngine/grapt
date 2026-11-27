//! Explicit operator edits to the user-owned MCP configuration.
use super::{BoundedString, OpenObject};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum McpConfigurationRequestDto {
    Add { config: OpenObject },
    Remove {},
    SetEnabled { enabled: bool },
    ApproveProject { fingerprint: BoundedString<64> },
    RevokeProjectApproval {},
}
