//! Typed native rewind requests and the prompt a committed cutoff reopens.
use super::*;

#[derive(Debug)]
pub struct RewindSelection {
    pub session: SessionWireId,
    pub before: WireId<grapt_types::MessageId>,
    pub tip: grapt_api::u3a::dto::NonNegativeU64,
    pub prompt: String,
}

pub fn encode_rewind_preview(prompts: u64) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::json!({"prompts":prompts})
        .to_string()
        .into_bytes();
    let _: grapt_api::u3a::SessionRewindPreviewRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_rewind_preview(bytes: &[u8]) -> Result<RewindSelection, WireError> {
    let dto: grapt_api::u3a::SessionRewindPreviewResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    Ok(RewindSelection {
        session: data.session_id().get().clone(),
        before: data.message_id().get().clone(),
        tip: *data.tip().get(),
        prompt: data.prompt().get().as_str().to_owned(),
    })
}

pub fn encode_rewind(selection: &RewindSelection) -> Result<Vec<u8>, WireError> {
    let bytes = serde_json::json!({"messageID":selection.before, "tip":selection.tip})
        .to_string()
        .into_bytes();
    let _: grapt_api::u3a::SessionRewindRequestDto = decode_request(&bytes, limits()?)?;
    Ok(bytes)
}

pub fn decode_rewound(bytes: &[u8], selected: &RewindSelection) -> Result<(), WireError> {
    let dto: grapt_api::u3a::SessionRewindResponseDto = decode_page(bytes, limits()?)?;
    let data = dto.data().get();
    if data.session_id().get() != &selected.session
        || data.message_id().get() != &selected.before
        || data.prompt().get().as_str() != selected.prompt
        || Some(number(*data.committed_seq().get())?) != number(selected.tip)?.checked_add(1)
    {
        return Err(WireError::Invalid {
            what: "rewind receipt",
            value: "receipt differs from the selected prompt".into(),
        });
    }
    Ok(())
}

fn number(value: grapt_api::u3a::dto::NonNegativeU64) -> Result<u64, WireError> {
    serde_json::to_value(value)
        .map_err(|error| WireError::Codec {
            reason: error.to_string(),
        })?
        .as_u64()
        .ok_or_else(|| WireError::Invalid {
            what: "rewind sequence",
            value: "sequence is not an unsigned integer".into(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rw_a_receipt_must_match_the_exact_selected_cut() {
        let selected = decode_rewind_preview(br#"{"data":{"sessionID":"ses_rewind","messageID":"msg_before","tip":7,"prompt":"reopen this"}}"#)
            .unwrap_or_else(|error| panic!("preview: {error}"));
        let receipt = serde_json::json!({"data":{"sessionID":"ses_rewind","messageID":"msg_before","committedSeq":8,"prompt":"reopen this"}});
        assert!(decode_rewound(receipt.to_string().as_bytes(), &selected).is_ok());
        for (field, value) in [
            ("sessionID", serde_json::json!("ses_foreign")),
            ("messageID", serde_json::json!("msg_other")),
            ("committedSeq", serde_json::json!(9)),
            ("prompt", serde_json::json!("another prompt")),
        ] {
            let mut changed = receipt.clone();
            changed["data"][field] = value;
            assert!(
                decode_rewound(changed.to_string().as_bytes(), &selected).is_err(),
                "accepted foreign {field}"
            );
        }
    }
}
