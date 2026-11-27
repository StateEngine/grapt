//! A file undo applies only the exact native preview the user reviewed.
use super::*;

#[derive(Debug)]
pub struct UndoSelection {
    pub session: SessionWireId,
    pub input: WireId<grapt_types::MessageId>,
    pub target: String,
    pub paths: Vec<String>,
    pub incomplete_patches: u64,
    pub writes: u64,
    pub deletes: u64,
}

#[derive(Debug)]
pub struct UndoReceipt {
    pub status: grapt_api::u3a::SessionUndoStatusDto,
    pub message: String,
}

pub fn decode_undo_preview(bytes: &[u8]) -> Result<UndoSelection, WireError> {
    let dto: grapt_api::u3a::SessionUndoPreviewResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    let number = |value| -> Result<u64, WireError> {
        serde_json::to_value(value)
            .map_err(|error| WireError::Codec {
                reason: error.to_string(),
            })?
            .as_u64()
            .ok_or_else(|| WireError::Invalid {
                what: "undo count",
                value: "count is not an integer".into(),
            })
    };
    let target = data.target().get().as_str();
    if target.is_empty() || target.len() > 256 {
        return Err(WireError::Invalid {
            what: "undo target",
            value: "invalid checkpoint identity".into(),
        });
    }
    let paths = data.paths().get().as_slice();
    let writes = number(data.writes().get())?;
    let deletes = number(data.deletes().get())?;
    if paths.is_empty()
        || paths
            .iter()
            .any(|path| path.as_str().is_empty() || path.as_str().len() > 4096)
        || writes.checked_add(deletes) != Some(paths.len() as u64)
    {
        return Err(WireError::Invalid {
            what: "undo paths",
            value: "paths and counted consequences differ or exceed their bounds".into(),
        });
    }
    Ok(UndoSelection {
        session: data.session_id().get().clone(),
        input: data.input_id().get().clone(),
        target: target.to_owned(),
        paths: data
            .paths()
            .get()
            .as_slice()
            .iter()
            .map(|path| path.as_str().to_owned())
            .collect(),
        incomplete_patches: number(data.incomplete_patches().get())?,
        writes: number(data.writes().get())?,
        deletes: number(data.deletes().get())?,
    })
}

pub fn encode_undo(selection: &UndoSelection) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::to_vec(
        &serde_json::json!({"inputID":selection.input,"target":selection.target}),
    )
    .map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    let _: grapt_api::u3a::SessionUndoRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_undone(bytes: &[u8], selection: &UndoSelection) -> Result<UndoReceipt, WireError> {
    let dto: grapt_api::u3a::SessionUndoResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    if data.session_id().get() != &selection.session
        || data.input_id().get() != &selection.input
        || data.target().get().as_str() != selection.target
    {
        return Err(WireError::Invalid {
            what: "undo receipt",
            value: "receipt differs from the reviewed file undo".into(),
        });
    }
    Ok(UndoReceipt {
        status: *data.status().get(),
        message: data.message().get().as_str().to_owned(),
    })
}

/// Native refusals name why the reviewed restore could not proceed.
pub fn decode_undo_conflict_message(bytes: &[u8]) -> Result<String, WireError> {
    let dto: grapt_api::u3a::ConflictErrorDto = decode_page(bytes, limits()?)?;
    Ok(dto.message().get().as_str().to_owned())
}
