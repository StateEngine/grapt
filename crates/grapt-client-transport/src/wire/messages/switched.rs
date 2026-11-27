//! A durable row this client decodes and does not draw, for the unsupported
//! path's witnesses.
use super::*;

/// A `model-switched` row: a contract member this client reads and does NOT
/// draw.
///
/// It exists so the "unsupported" path has a witness that stays honest. It used
/// to be exercised with a TOOL row, and a tool is now rendered — so a fault
/// still built from one would have been asserting a gap this client no longer
/// has, and the count it checks would have quietly gone to zero.
pub fn model_switched_message(
    id: &str,
) -> Result<grapt_api::u3a::SessionMessageInfoDto, WireError> {
    use grapt_api::u3a::{
        ModelRefDto, SessionMessageInfoDto, SessionMessageModelSelectedDto,
        SessionMessageModelSelectedDtoTime, SessionMessageModelSelectedDtoType,
    };
    let message_id = WireId::parse(id.to_owned()).ok_or_else(|| WireError::Invalid {
        what: "message id",
        value: id.to_owned(),
    })?;
    let model_id = WireId::parse("m1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "model id",
        value: "m1".to_owned(),
    })?;
    let provider_id = WireId::parse("p1".to_owned()).ok_or_else(|| WireError::Invalid {
        what: "provider id",
        value: "p1".to_owned(),
    })?;
    Ok(SessionMessageInfoDto::ModelSelected(Box::new(
        SessionMessageModelSelectedDto::new(
            Required::new(SessionMessageModelSelectedDtoType::ModelSwitched),
            Required::new(message_id),
            Optional::absent(),
            Required::new(SessionMessageModelSelectedDtoTime::new(Required::new(
                finite(1_700_000_000_000.0)?,
            ))),
            Required::new(ModelRefDto::new(
                Required::new(model_id),
                Required::new(provider_id),
                Optional::absent(),
            )),
            Optional::absent(),
        ),
    )))
}
