//! Closed external-host controls; product owns admission and binding validation.

use super::prelude::*;
use super::{InstructionJsonDto, SessionPendingUserDtoDelivery};
use grapt_types::{EventId, MessageId, SessionId};

closed_object! { pub struct HostServiceIdentityDto => "inline:host-service-identity" {
    required incarnation: BoundedString<512> => "incarnation",
    required service_instance: BoundedString<512> => "serviceInstance",
    required generation: NonNegativeU64 => "generation",
    required registration_version: BoundedString<512> => "registrationVersion",
} }

closed_object! { pub struct NativeNoticeRequestDto => "inline:native-notice-request" {
    required id: WireId<MessageId> => "id",
    required target_id: WireId<EventId> => "targetID",
    required binding: BoundedString<64> => "binding",
    required messages: BoundedVec<BoundedString<256>, 32> => "messages",
    required incarnation: BoundedString<512> => "incarnation",
    required service_instance: BoundedString<512> => "serviceInstance",
    required generation: NonNegativeU64 => "generation",
    required registration_version: BoundedString<512> => "registrationVersion",
} }

closed_object! { pub struct NativeMailResumeRequestDto => "inline:native-mail-resume-request" {
    required hold_id: WireId<EventId> => "holdID",
    required target_id: WireId<EventId> => "targetID",
    required binding: BoundedString<64> => "binding",
    required service: HostServiceIdentityDto => "service",
} }

string_enum! { pub enum HostInputKindDto {
    User => "user",
    Synthetic => "synthetic",
} }

closed_object! { pub struct HostInputAcceptanceDto => "inline:host-input-acceptance" {
    required id: WireId<MessageId> => "id",
    required session_id: WireId<SessionId> => "sessionID",
    required admitted_seq: NonNegativeU64 => "admittedSeq",
    required time_created: WireI64 => "timeCreated",
    required kind: HostInputKindDto => "kind",
    required text: BoundedString<262144> => "text",
    required delivery: SessionPendingUserDtoDelivery => "delivery",
    nullable metadata: InstructionJsonDto => "metadata",
} }

closed_object! { pub struct HostInputReadbackDto => "inline:host-input-readback" {
    required service: HostServiceIdentityDto => "service",
    nullable data: HostInputAcceptanceDto => "data",
} }

string_enum! { pub enum HostNoticeBindingStatusDto {
    Ready => "ready",
    Held => "held",
    RebindRequired => "rebind-required",
} }

closed_object! { pub struct HostNoticeBindingDto => "inline:host-notice-binding" {
    required service: HostServiceIdentityDto => "service",
    required session_id: WireId<SessionId> => "sessionID",
    nullable target_id: WireId<EventId> => "targetID",
    nullable binding: BoundedString<64> => "binding",
    nullable hold_id: WireId<EventId> => "holdID",
    required status: HostNoticeBindingStatusDto => "status",
} }

#[derive(Debug)]
pub enum HostControlRequest {
    Input {
        session: WireId<SessionId>,
        input: WireId<MessageId>,
    },
    Binding {
        session: WireId<SessionId>,
    },
    Notice {
        session: WireId<SessionId>,
        request: NativeNoticeRequestDto,
    },
    Resume {
        session: WireId<SessionId>,
        request: NativeMailResumeRequestDto,
    },
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(untagged)]
pub enum HostControlResponse {
    Binding(HostNoticeBindingDto),
    Acceptance(HostInputReadbackDto),
}

/// Object-safe host controls, implemented separately from ordinary session operations.
pub trait HostControlApplicationPort: Send + Sync + 'static {
    fn host_control(
        &self,
        _principal: crate::SealedPrincipal,
        _request: HostControlRequest,
    ) -> super::ApplicationFuture<HostControlResponse, super::SessionPromptApplicationError> {
        Box::pin(async { Err(unsupported()) })
    }
}

fn unsupported() -> super::SessionPromptApplicationError {
    let message = match DtoString::try_new("external host controls are unsupported") {
        Ok(message) => message,
        Err(error) => panic!("static host-control diagnostic exceeds its bound: {error}"),
    };
    super::SessionPromptApplicationError::InvalidRequest(super::InvalidRequestErrorDto::new(
        grapt_schema::Required::new(super::InvalidRequestErrorDtoTag::InvalidRequestError),
        grapt_schema::Required::new(message),
        grapt_schema::OptionalNullable::absent(),
        grapt_schema::OptionalNullable::absent(),
    ))
}
