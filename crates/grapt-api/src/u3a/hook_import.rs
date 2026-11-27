//! Explicit operator migration; hook command text never enters diagnostics.
use super::{BoundedString, BoundedVec};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookImportRequestDto {
    pub hooks: BoundedString<65536>,
    pub apply: bool,
}
impl std::fmt::Debug for HookImportRequestDto {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("HookImportRequestDto")
            .field("apply", &self.apply)
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HookImportResponseDto {
    pub imported: BoundedVec<BoundedString<128>, 32>,
    pub skipped: BoundedVec<BoundedString<128>, 32>,
}
