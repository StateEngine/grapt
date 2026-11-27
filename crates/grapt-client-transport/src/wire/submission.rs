//! Positive admission evidence, decoded through the real bounded DTOs.
use super::*;
use grapt_api::u3a::{
    SessionMessageInfoDto, SessionMessagesResponseDto, SessionPendingInfoDto,
    SessionPendingListResponseDto, SessionPendingUserDtoDelivery,
};
use grapt_types::MessageId;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmissionOrigin {
    Composer,
    SeatStartup,
    Followup,
}

impl SubmissionOrigin {
    pub fn startup(self) -> bool {
        self == Self::SeatStartup
    }
}

/// Delivery-aware presentation of admitted human input from the server inbox.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct QueuedInputPreview {
    pub total: usize,
    pub messages: Vec<String>,
    pub steering_total: usize,
    pub steering_messages: Vec<String>,
    pub unavailable: Option<String>,
}

pub fn queued_inputs(
    bytes: &[u8],
    session: &SessionWireId,
) -> Result<QueuedInputPreview, WireError> {
    let dto: SessionPendingListResponseDto = decode_page(bytes, limits()?)?;
    let mut preview = QueuedInputPreview::default();
    for row in dto.data().get().as_slice() {
        if let SessionPendingInfoDto::User(user) = row {
            if user.session_id().get() != session {
                return Err(WireError::Invalid {
                    what: "pending input session",
                    value: "response belongs to another session".into(),
                });
            }
            if is_seat_startup(user.data().get().metadata().as_present())? {
                continue;
            }
            let messages = match user.delivery().get() {
                SessionPendingUserDtoDelivery::Steer => {
                    preview.steering_total += 1;
                    &mut preview.steering_messages
                }
                SessionPendingUserDtoDelivery::Queue => {
                    preview.total += 1;
                    &mut preview.messages
                }
            };
            if messages.len() < 2 {
                let text = user.data().get().text().get().as_str();
                let mut characters = text.chars();
                let mut message: String = characters.by_ref().take(256).collect();
                if characters.next().is_some() {
                    message.push('…');
                }
                messages.push(message);
            }
        }
    }
    Ok(preview)
}

pub fn pending_holds_input(
    bytes: &[u8],
    session: &SessionWireId,
    id: &WireId<MessageId>,
    text: &str,
    origin: SubmissionOrigin,
) -> Result<bool, WireError> {
    let dto: SessionPendingListResponseDto = decode_page(bytes, limits()?)?;
    let delivery = if origin == SubmissionOrigin::Composer {
        grapt_api::u3a::SessionPendingUserDtoDelivery::Steer
    } else {
        grapt_api::u3a::SessionPendingUserDtoDelivery::Queue
    };
    for row in dto.data().get().as_slice() {
        if let SessionPendingInfoDto::User(user) = row {
            if user.id().get() == id
                && user.session_id().get() == session
                && user.delivery().get() == &delivery
                && user.data().get().text().get().as_str() == text
                && is_seat_startup(user.data().get().metadata().as_present())? == origin.startup()
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub fn history_holds_input(
    bytes: &[u8],
    id: &WireId<MessageId>,
    text: &str,
    seat_startup: bool,
) -> Result<bool, WireError> {
    let dto: SessionMessagesResponseDto = decode_page(bytes, limits()?)?;
    for row in dto.data().get().as_slice() {
        if let SessionMessageInfoDto::User(user) = row {
            if user.id().get() == id
                && user.text().get().as_str() == text
                && is_seat_startup(user.metadata().as_present())? == seat_startup
            {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

pub(super) fn is_seat_startup(
    metadata: Option<&grapt_api::u3a::OpenObject>,
) -> Result<bool, WireError> {
    let value = serde_json::to_value(metadata).map_err(|error| WireError::Codec {
        reason: error.to_string(),
    })?;
    Ok(value.get("origin").and_then(serde_json::Value::as_str) == Some("seat-startup"))
}
