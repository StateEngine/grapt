//! Authenticated, explicit Git worktree mutations.
use super::prelude::*;
use super::LocationInfoDto;

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum WorkspaceWorktreeRequestDto {
    Create {
        destination: BoundedString<4096>,
        /// Explicit repository-relative paths, even when the source cwd is nested.
        #[serde(default)]
        setup_files: Vec<BoundedString<4096>>,
    },
    Merge {
        directory: BoundedString<4096>,
    },
    Remove {
        directory: BoundedString<4096>,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceWorktreeEffectDto {
    Completed,
    NotStarted,
    MayHaveChanged,
}

closed_object! { pub struct WorkspaceWorktreeDataDto => "inline:workspace-worktree-data" {
    required directory: BoundedString<4096> => "directory",
    required message: BoundedString<4096> => "message",
    required effect: WorkspaceWorktreeEffectDto => "effect",
} }
// Keep the authenticated source separate from the resulting checkout/cwd.
closed_object! { pub struct WorkspaceWorktreeResponseDto => "inline:workspace-worktree-response" {
    required location: LocationInfoDto => "location",
    required data: WorkspaceWorktreeDataDto => "data",
} }
