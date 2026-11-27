//! The plugin operator lane uses the public bounded request/response DTOs.
use super::*;
pub fn encode_claude_plugin(
    request: &grapt_api::u3a::ClaudePluginRequestDto,
) -> Result<Vec<u8>, WireError> {
    Ok(encode_page(request, limits()?)?)
}
pub fn decode_claude_plugin(bytes: &[u8]) -> Result<String, WireError> {
    let response: grapt_api::u3a::ClaudePluginResponseDto = decode_page(bytes, limits()?)?;
    Ok(response.message.as_str().to_owned())
}
