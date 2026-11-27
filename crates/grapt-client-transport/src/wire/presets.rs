//! Preset operations use the same validated DTO codec as other session operations.
use super::*;
use crate::preset::{name, Preset};

#[derive(Debug)]
pub struct PresetSnapshot {
    pub selected: Option<Preset>,
    pub effective: Preset,
}

pub fn decode_preset(bytes: &[u8]) -> Result<PresetSnapshot, WireError> {
    let dto: grapt_api::u3a::SessionPresetResponseDto =
        grapt_api::u3a::decode_request(bytes, limits()?)?;
    let data = dto.data().get();
    Ok(PresetSnapshot {
        selected: data.selected().as_value().copied(),
        effective: *data.effective().get(),
    })
}

pub fn encode_preset(preset: Preset) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::json!({"preset":name(preset)})
        .to_string()
        .into_bytes();
    let _: grapt_api::u3a::SessionPresetSelectDto =
        grapt_api::u3a::decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_preset_selection(bytes: &[u8]) -> Result<Preset, WireError> {
    let dto: grapt_api::u3a::SessionPresetSelectDto =
        grapt_api::u3a::decode_request(bytes, limits()?)?;
    if dto.preset().get().as_str() == "bypass" {
        return Ok(Preset::Bypass);
    }
    crate::preset::from_flag(&format!("--{}", dto.preset().get().as_str())).ok_or_else(|| {
        WireError::Invalid {
            what: "preset",
            value: dto.preset().get().as_str().to_owned(),
        }
    })
}
