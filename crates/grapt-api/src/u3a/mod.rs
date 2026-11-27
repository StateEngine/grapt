//! Handwritten bounded Dogfood-0 API contract.

pub(crate) use routes::unauthorized_response;

mod application;
mod capture;
mod capture_wire;
#[path = "vocabulary/catalog.rs"]
mod catalog;
pub mod codec;
pub use codec::ToolInputObject;
#[path = "vocabulary/connections.rs"]
mod connections;
mod integration_auth;
pub use integration_auth::*;
mod integration_routes;
pub use integration_routes::{
    register_integration_routes, IntegrationApplicationPort, INTEGRATION_ROUTE_COUNT,
};
mod pending_application;
pub use pending_application::{
    register_pending_capture_event_route, register_pending_dogfood_routes, ApplicationSlot,
};
mod claude_plugin;
pub use claude_plugin::{
    register_claude_plugin_route, ClaudePluginApplicationPort, ClaudePluginRequestDto,
    ClaudePluginResponseDto, CLAUDE_PLUGIN_ALLOWANCE,
};
mod hook_import;
pub use hook_import::{HookImportRequestDto, HookImportResponseDto};
mod host_control;
pub use host_control::{
    HostControlApplicationPort, HostControlRequest, HostControlResponse, HostInputAcceptanceDto,
    HostInputKindDto, HostInputReadbackDto, HostNoticeBindingDto, HostNoticeBindingStatusDto,
    HostServiceIdentityDto, NativeMailResumeRequestDto, NativeNoticeRequestDto,
};
mod mcp_configuration;
pub use mcp_configuration::McpConfigurationRequestDto;
mod session_snapshot;
pub use session_snapshot::{
    SessionSnapshotApplicationPort, SessionSnapshotExecutionDto, SessionSnapshotProvisionalDto,
    SessionSnapshotProvisionalKindDto, SessionSnapshotResponseDto, SessionSnapshotRevisionDto,
    MAX_PROVISIONAL_BLOCKS,
};
mod mcp_approval;
pub use mcp_approval::McpApprovalPreviewDto;
mod mcp_content;
pub use mcp_content::{
    McpContentRequestDto, McpContentResponseDto, McpResourceWatchDto, McpUserContentDto,
};
mod mcp_auth;
pub use mcp_auth::{McpAuthRequestDto, McpAuthResponseDto, McpAuthState};
pub mod dto;
mod events;
mod tool_progress;
mod utf16_text;
mod workspace_worktree;
pub use tool_progress::{
    ToolCalledData, ToolFailedData, ToolInputStartedData, ToolProgressData, ToolProgressEntry,
    ToolProgressMetadata, ToolProgressStatus, ToolResultProjection, ToolSuccessData,
};
pub use workspace_worktree::{
    WorkspaceWorktreeDataDto, WorkspaceWorktreeEffectDto, WorkspaceWorktreeRequestDto,
    WorkspaceWorktreeResponseDto,
};
#[path = "vocabulary/form.rs"]
mod form;
pub mod limits;
#[path = "vocabulary/operations.rs"]
mod operations;
#[path = "vocabulary/permission.rs"]
mod permission;
#[path = "vocabulary/permission_preview.rs"]
mod permission_preview;
pub use grapt_window_lease::{
    CloseRegistry, WindowId, WindowWatch, DEFAULT_CLOSE_GRACE, MAX_RETAINED_SESSIONS, STALL_TICK,
};
mod query;
mod routes;
#[path = "vocabulary/shell.rs"]
mod shell;
mod sse;
mod prelude {
    pub(crate) use std::fmt;

    pub(crate) use crate::u3a::dto::{
        bounded_id, closed_object, encoding_receipt, field_type, finish_field, named_newtype,
        presence_row, serialize_field, serialized_field_count, string_enum, tagged_union,
        Base64Dto, BoundedMap, BoundedString, BoundedVec, DtoString, FiniteF64,
        FiniteOrSentinelF64, NamedSchema, NonNegativeU64, OpenObject, PositiveU64, PromptText,
        SafeTokenCount, TrueValue, WireI64, WireId, MAX_CATALOG_ROWS, MAX_MESSAGE_ROWS,
        MAX_METADATA_KEYS, MAX_MODEL_ROWS, MAX_NESTED_ITEMS, MAX_PROMPT_AGENTS, MAX_PROMPT_FILES,
        MAX_SESSION_ROWS,
    };
}
#[path = "vocabulary/registry.rs"]
mod registry;
#[path = "vocabulary/session.rs"]
mod session;
#[path = "vocabulary/session_state.rs"]
mod session_state;
pub use session_state::*;
#[path = "vocabulary/session_context.rs"]
mod session_context;
pub use session_context::*;

pub use application::{
    ApplicationFuture, ApplicationRequest, ApplicationUnavailable, ApplicationUnavailableError,
    CaptureApplicationPort, CatalogApplicationError, DogfoodApplicationPort,
    FormCancelApplicationError, FormCreateApplicationError, FormLookupApplicationError,
    FormReplyApplicationError, MessageListView, PermissionApplicationError,
    SessionActiveApplicationError, SessionCompactApplicationError, SessionEditApplicationPort,
    SessionListApplicationError, SessionLookupApplicationError, SessionMessageApplicationError,
    SessionMessagesApplicationError, SessionPromptApplicationError,
    SessionReconcileApplicationError, SessionRewindApplicationError, SessionUndoApplicationError,
};
pub use capture::{
    register_capture_event_route, CapturePage, CapturePageError, CapturePageRow,
    CaptureStreamTerminal, CAPTURE_EVENT_PATH, CAPTURE_PROTOCOL_VERSION,
};
pub use catalog::*;
pub use codec::{
    decode_image_prompt, decode_page, decode_request, encode_page, Base64Dto, BoundedMap,
    BoundedString, BoundedVec, DtoCodecError, DtoString, FiniteF64, FiniteOrSentinelF64,
    OpenObject, OpenValueView, PromptText, SafeTokenCount, WireId,
};
pub use connections::*;
pub use dto::{
    NamedSchema, TriStateEncodingReceipt, FROZEN_OPENAPI_SHA256, FROZEN_SCHEMA_BYTES,
    FROZEN_SCHEMA_COUNT, MAX_CATALOG_ROWS, MAX_MODEL_ROWS, U3A_DTO_VERSION,
};
pub use events::{
    AggregateIdDto, CompactionEndedData, CompactionReason, ContinuityAttentionData,
    ContinuityAttentionInfo, ContinuityDegradedReason, DogfoodEvent, DurableEvent, DurableEventRef,
    ExecutionFailedData, ExecutionInterruptedData, ExecutionSucceededData, InterruptReason,
    LiveEvent, MailAttentionData, MailAttentionInfo, MailPendingCount, McpCatalogChangedData,
    ServerConnectedData, ServerConnectedEvent, ServerStoppingData, SessionStatusAction,
    SessionStatusData, SessionStatusInfo, SessionStatusPhase, SessionTitleChangedData,
    SkillsChangedData, SliceDeltaData, SliceEndedData, SliceStartedData, StepStartedData,
    UsageUpdatedData, WebSearchUpdatedData,
};
pub use form::*;
pub use limits::{U3aLimitValues, U3aLimits, U3aLimitsError, U3A_PRODUCTION_LIMITS};
pub use operations::*;
pub use shell::*;
pub use utf16_text::{Utf16Text, Utf16TextPublic};
/// The frozen permission list consumes the same closed `LocationQuery` shape
/// as the other location-scoped catalog endpoints. The alias deliberately
/// shares that generic wire shape while the application request keeps a
/// permission-specific type name, so the query cannot disappear into a
/// query-less dispatch variant again.
pub type PermissionLocationQueryDto = ProjectCurrentLocationQueryDto;
pub use permission::*;
pub use permission_preview::*;
pub use query::{MAX_QUERY_BYTES, MAX_QUERY_PAIRS};
pub use registry::{
    NAMED_SCHEMA_CODECS, NAMED_SCHEMA_MANIFEST, OPTIONAL_NULLABLE_CARRIERS,
    OPTIONAL_NULLABLE_ENCODING_CARRIERS,
};
pub use routes::{
    register_dogfood_routes, register_ephemeral_dogfood_routes, DogfoodRouteRegistrationError,
    DOGFOOD_NON_SSE_ROUTE_COUNT,
};
pub use session::*;
pub use sse::{
    register_dogfood_event_route, ApprovalWatch, DogfoodEventHub, DogfoodEventSubscription,
    ProvisionalBlock, ProvisionalChange, ProvisionalKind, ProvisionalText, PublishReceipt,
    ReconcileCounts, ReconcileReason, SseError, SseLimitValues, SseLimits, StatusHydrator,
    StreamEnd, StreamEndCause, StreamEndObserver, DOGFOOD_EVENT_PATH, MAX_POSITIONED_FRAME_BYTES,
    MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES, MAX_SSE_PENDING_EVENTS, MAX_SSE_SUBSCRIBERS,
};
