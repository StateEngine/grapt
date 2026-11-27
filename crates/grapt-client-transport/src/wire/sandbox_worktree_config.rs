//! Bounded Windows Git compatibility previews cross the public DTO boundary.
use super::*;

#[derive(Debug)]
pub struct SandboxWorktreeConfig {
    pub missing: bool,
    pub path: Option<String>,
    pub text: String,
}

pub fn decode_sandbox_worktree_config(bytes: &[u8]) -> Result<SandboxWorktreeConfig, WireError> {
    let dto: grapt_api::u3a::SandboxWorktreeConfigResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    let missing = *data.missing().get();
    let path = data
        .path()
        .as_present()
        .map(|path| path.as_str().to_owned());
    if missing && path.is_none()
        || path
            .as_ref()
            .is_some_and(|path| path.is_empty() || path.chars().any(char::is_control))
    {
        return Err(WireError::Invalid {
            what: "sandbox worktree-config preview",
            value: "missing or invalid exact backend path".into(),
        });
    }
    Ok(SandboxWorktreeConfig {
        missing,
        path,
        text: data.text().get().as_str().to_owned(),
    })
}

pub fn encode_sandbox_worktree_config_consent(path: &str) -> Result<Vec<u8>, WireError> {
    let bytes =
        serde_json::to_vec(&serde_json::json!({"consent":true,"path":path})).map_err(|error| {
            WireError::Codec {
                reason: error.to_string(),
            }
        })?;
    let _: grapt_api::u3a::SandboxWorktreeConfigRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}
