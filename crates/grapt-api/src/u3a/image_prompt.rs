//! The larger wire bound belongs only to explicit inline image prompts.

use grapt_schema::DecodeLimits;

use super::super::{limits, PromptRequestDto};
use super::DtoCodecError;

/// Ordinary requests retain the 64 KiB host limit; text keeps its own bound.
pub fn decode_image_prompt(bytes: &[u8]) -> Result<PromptRequestDto, DtoCodecError> {
    let limits = DecodeLimits::new(4 * 1024 * 1024, limits::MAX_NESTED_ITEMS)
        .map_err(|_| DtoCodecError::Limits)?;
    let dto: PromptRequestDto = grapt_schema::decode_json(bytes, limits)?;
    if bytes.len() > super::super::limits::MAX_REQUEST_DTO_BYTES {
        let has_images = dto.files().as_present().is_some_and(|files| {
            files
                .as_slice()
                .iter()
                .any(|file| file.uri().get().as_str().starts_with("data:image/"))
        });
        if !has_images {
            return Err(DtoCodecError::PageBytes);
        }
    }
    Ok(dto)
}
