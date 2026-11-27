use super::catalog::{LocationInfoDto, PermissionEffectDto};
use super::prelude::*;
use super::PermissionPreviewDto;

pub const MAX_PERMISSION_ROWS: usize = 128;
pub const MAX_PERMISSION_RESOURCES: usize = 128;
/// Session and project consent each hold at most 128 targets; the terminal
/// owner admits at most 64 live commands, each with one additional target.
pub const MAX_SANDBOX_GRANTS: usize = 320;

string_enum! {
    pub enum PermissionReplyDto {
        Once => "once",
        Session => "session",
        Always => "always",
        Reject => "reject",
    }
}

string_enum! {
    pub enum PermissionSuspendedReasonDto {
        Shutdown => "shutdown",
        ProcessLoss => "process-loss",
    }
}

string_enum! {
    pub enum PermissionSourceDtoType {
        Tool => "tool",
    }
}

closed_object! {
    pub struct PermissionSourceDto => "Permission.Source" {
        required kind: PermissionSourceDtoType => "type",
        required message_id: WireId<grapt_types::MessageId> => "messageID",
        required call_id: WireId<grapt_types::ToolCallId> => "callID",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn source_and_agent_are_closed_typed_inputs() -> Result<(), Box<dyn std::error::Error>> {
        let valid: PermissionCreateRequestDto = serde_json::from_value(json!({
            "action": "write",
            "resources": ["/work/file"],
            "source": {"type": "tool", "messageID": "msg_source", "callID": "call-source"},
            "agent": "agent-build",
        }))?;
        assert!(valid.source().as_present().is_some());
        assert!(valid.agent().as_present().is_some());

        for invalid in [
            json!({
                "action": "write", "resources": [],
                "source": {"type": "prompt", "messageID": "msg_source", "callID": "call-source"}
            }),
            json!({
                "action": "write", "resources": [],
                "source": {"type": "tool", "messageID": "msg_source", "callID": "call-source", "extra": true}
            }),
            json!({"action": "write", "resources": [], "agent": {"id": "agent-build"}}),
        ] {
            assert!(
                serde_json::from_value::<PermissionCreateRequestDto>(invalid).is_err(),
                "open source or raw agent shape must refuse"
            );
        }
        Ok(())
    }

    #[test]
    fn request_state_cannot_represent_impossible_lifecycle_pairs(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let base = json!({
            "id": "per_state",
            "sessionID": "ses_state",
            "action": "write",
            "resources": [],
        });
        let mut pending = base.clone();
        pending["status"] = json!("pending");
        serde_json::from_value::<PermissionRequestStateDto>(pending.clone())?;
        pending["suspendedReason"] = json!("shutdown");
        assert!(serde_json::from_value::<PermissionRequestStateDto>(pending).is_err());

        let mut suspended = base.clone();
        suspended["status"] = json!("suspended");
        assert!(
            serde_json::from_value::<PermissionRequestStateDto>(suspended.clone()).is_err(),
            "suspended state without reason must refuse"
        );
        suspended["suspendedReason"] = json!("process-loss");
        serde_json::from_value::<PermissionRequestStateDto>(suspended)?;

        let mut terminal = base;
        terminal["status"] = json!("replied");
        assert!(
            serde_json::from_value::<PermissionRequestStateDto>(terminal).is_err(),
            "terminal state must not inhabit open request DTO"
        );
        Ok(())
    }

    #[test]
    fn reply_preserves_supplied_identities_and_accepts_the_legacy_body(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let valid = json!({"reply": "once", "requestID": "req-1", "nonce": "nonce-1"});
        let decoded = serde_json::from_value::<PermissionReplyRequestDto>(valid.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, valid);
        let legacy = json!({"reply":"once"});
        let decoded = serde_json::from_value::<PermissionReplyRequestDto>(legacy.clone())?;
        assert_eq!(serde_json::to_value(decoded)?, legacy);
        for field in ["requestID", "nonce"] {
            let mut missing = valid.clone();
            missing
                .as_object_mut()
                .ok_or("reply is an object")?
                .remove(field);
            assert!(serde_json::from_value::<PermissionReplyRequestDto>(missing).is_ok());
            let mut invalid = valid.clone();
            invalid[field] = json!(null);
            assert!(serde_json::from_value::<PermissionReplyRequestDto>(invalid).is_err());
        }
        Ok(())
    }
}

closed_object! {
    pub struct PermissionCreateRequestDto => "Permission.Request" {
        optional id: WireId<grapt_types::PermissionId> => "id",
        required action: DtoString => "action",
        required resources: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "resources",
        optional save: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "save",
        optional metadata: OpenObject => "metadata",
        optional source: PermissionSourceDto => "source",
        optional agent: WireId<grapt_types::AgentId> => "agent",
    }
}

string_enum! {
    pub enum PermissionPendingStatusDto {
        Pending => "pending",
    }
}

string_enum! {
    pub enum PermissionSuspendedStatusDto {
        Suspended => "suspended",
    }
}

closed_object! {
    pub struct PermissionPendingRequestStateDto => "inline:PermissionPendingRequestState" {
        required id: WireId<grapt_types::PermissionId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required action: DtoString => "action",
        required resources: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "resources",
        required status: PermissionPendingStatusDto => "status",
        optional save: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "save",
        optional metadata: OpenObject => "metadata",
        optional source: PermissionSourceDto => "source",
        optional preview: PermissionPreviewDto => "preview",
    }
}

closed_object! {
    pub struct PermissionSuspendedRequestStateDto => "inline:PermissionSuspendedRequestState" {
        required id: WireId<grapt_types::PermissionId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required action: DtoString => "action",
        required resources: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "resources",
        required status: PermissionSuspendedStatusDto => "status",
        required suspended_reason: PermissionSuspendedReasonDto => "suspendedReason",
        optional save: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "save",
        optional metadata: OpenObject => "metadata",
        optional source: PermissionSourceDto => "source",
        optional preview: PermissionPreviewDto => "preview",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum PermissionRequestStateDto {
    Pending(Box<PermissionPendingRequestStateDto>),
    Suspended(Box<PermissionSuspendedRequestStateDto>),
}

impl serde::Serialize for PermissionRequestStateDto {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        match self {
            Self::Pending(value) => serde::Serialize::serialize(value, serializer),
            Self::Suspended(value) => serde::Serialize::serialize(value, serializer),
        }
    }
}

impl<'de> serde::Deserialize<'de> for PermissionRequestStateDto {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = <serde_json::Value as serde::Deserialize>::deserialize(deserializer)?;
        match value.get("status").and_then(serde_json::Value::as_str) {
            Some("pending") => serde_json::from_value(value)
                .map(Box::new)
                .map(Self::Pending)
                .map_err(serde::de::Error::custom),
            Some("suspended") => serde_json::from_value(value)
                .map(Box::new)
                .map(Self::Suspended)
                .map_err(serde::de::Error::custom),
            Some(other) => Err(serde::de::Error::unknown_variant(
                other,
                &["pending", "suspended"],
            )),
            None => Err(serde::de::Error::missing_field("status")),
        }
    }
}

impl NamedSchema for PermissionRequestStateDto {
    const OPENAPI_NAME: &'static str = "Permission.RequestState";
}

closed_object! {
    pub struct PermissionCreateResponseDataDto => "inline:PermissionCreateResponseData" {
        required id: WireId<grapt_types::PermissionId> => "id",
        required effect: PermissionEffectDto => "effect",
    }
}

closed_object! {
    pub struct PermissionCreateResponseDto => "inline:PermissionCreateResponse" {
        required data: PermissionCreateResponseDataDto => "data",
    }
}

closed_object! {
    pub struct PermissionListResponseDto => "inline:PermissionListResponse" {
        required data: BoundedVec<PermissionRequestStateDto, MAX_PERMISSION_ROWS> => "data",
        required has_more: bool => "hasMore",
    }
}

closed_object! {
    pub struct PermissionGlobalListResponseDto => "inline:PermissionGlobalListResponse" {
        required location: LocationInfoDto => "location",
        required data: BoundedVec<PermissionRequestStateDto, MAX_PERMISSION_ROWS> => "data",
        required has_more: bool => "hasMore",
    }
}

closed_object! {
    pub struct PermissionResponseDto => "inline:PermissionResponse" {
        required data: PermissionRequestStateDto => "data",
    }
}

closed_object! {
    pub struct PermissionAskedData => "inline:PermissionAskedData" {
        required id: WireId<grapt_types::PermissionId> => "id",
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required action: DtoString => "action",
        required resources: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "resources",
        optional save: BoundedVec<DtoString, MAX_PERMISSION_RESOURCES> => "save",
        optional metadata: OpenObject => "metadata",
        optional source: PermissionSourceDto => "source",
        optional preview: PermissionPreviewDto => "preview",
    }
}

closed_object! {
    pub struct PermissionRepliedData => "inline:PermissionRepliedData" {
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required request_id: WireId<grapt_types::PermissionId> => "requestID",
        required reply: PermissionReplyDto => "reply",
    }
}

string_enum! { pub enum PermissionCancelReasonDto {
    Interrupted => "interrupted",
    ApprovalUnavailable => "approval-unavailable",
} }
closed_object! {
    pub struct PermissionCancelledData => "inline:PermissionCancelledData" {
        required session_id: WireId<grapt_types::SessionId> => "sessionID",
        required request_id: WireId<grapt_types::PermissionId> => "requestID",
        required reason: PermissionCancelReasonDto => "reason",
    }
}

closed_object! {
    pub struct PermissionReplyRequestDto => "inline:PermissionReplyRequest" {
        required reply: PermissionReplyDto => "reply",
        optional request_id: WireId<grapt_types::RequestId> => "requestID",
        optional nonce: WireId<grapt_types::DecisionNonce> => "nonce",
        optional message: DtoString => "message",
    }
}

string_enum! {
    pub enum PermissionNotFoundErrorDtoTag {
        PermissionNotFoundError => "PermissionNotFoundError",
    }
}

closed_object! {
    pub struct PermissionNotFoundErrorDto => "PermissionNotFoundError" {
        required tag: PermissionNotFoundErrorDtoTag => "_tag",
        required request_id: WireId<grapt_types::PermissionId> => "requestID",
        required message: DtoString => "message",
    }
}

string_enum! {
    pub enum PermissionNotLiveErrorDtoTag {
        PermissionNotLiveError => "PermissionNotLiveError",
    }
}

closed_object! {
    pub struct PermissionNotLiveErrorDto => "PermissionNotLiveError" {
        required tag: PermissionNotLiveErrorDtoTag => "_tag",
        required request_id: WireId<grapt_types::PermissionId> => "requestID",
        required reason: PermissionSuspendedReasonDto => "reason",
        required message: DtoString => "message",
    }
}
// Public saved-rule identity is opaque; Rust derives it from the replayable rule tuple.
closed_object! { pub struct PermissionSavedInfoDto => "PermissionSaved.Info" {
    required id: DtoString => "id",
    required project_id: WireId<grapt_types::ProjectId> => "projectID",
    required action: DtoString => "action",
    required resource: DtoString => "resource",
} }
closed_object! { pub struct PermissionSavedListResponseDto => "inline:PermissionSaved.ListResponse" {
    required data: BoundedVec<PermissionSavedInfoDto, 128> => "data",
} }

string_enum! { pub enum SandboxGrantScopeDto {
    Run => "run",
    Session => "session",
    Project => "project",
} }
string_enum! { pub enum SandboxGrantRevokeStateDto {
    Pending => "pending",
    Complete => "complete",
} }
closed_object! { pub struct SandboxGrantDto => "inline:SandboxGrant" {
    required scope: SandboxGrantScopeDto => "scope",
    required resource: DtoString => "resource",
    optional operation: WireId<grapt_types::OperationId> => "operation",
} }
closed_object! { pub struct SandboxGrantListResponseDto => "inline:SandboxGrantList" {
    required data: BoundedVec<SandboxGrantDto, MAX_SANDBOX_GRANTS> => "data",
} }
closed_object! { pub struct SandboxGrantRevokeResponseDto => "inline:SandboxGrantRevokeResponse" {
    required state: SandboxGrantRevokeStateDto => "state",
    required message: DtoString => "message",
} }
