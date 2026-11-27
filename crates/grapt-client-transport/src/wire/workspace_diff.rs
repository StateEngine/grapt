//! Workspace inspection uses the backend's bounded public DTO.
use super::*;
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceDiff {
    pub is_git: bool,
    pub text: String,
}
pub fn decode_workspace_diff(bytes: &[u8]) -> Result<WorkspaceDiff, WireError> {
    let dto: grapt_api::u3a::WorkspaceDiffResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    let is_git = *data.is_git().get();
    let text = data.text().get().as_str().to_owned();
    if !is_git && !text.is_empty() {
        return Err(WireError::Invalid {
            what: "non-repository diff",
            value: "unexpected diff content".to_owned(),
        });
    }
    Ok(WorkspaceDiff { is_git, text })
}
pub fn encode_workspace_diff(project: &ProjectRow, text: &str) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(&serde_json::json!({"location": super::catalogs::location_value(project), "data":{"isGit":true,"text":text}})).map_err(|error| WireError::Codec { reason:error.to_string() })?;
    let _validated: grapt_api::u3a::WorkspaceDiffResponseDto = decode_page(&bytes, limits()?)?;
    Ok(bytes)
}
