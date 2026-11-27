//! Authenticated non-SSE Dogfood-0 routes over one typed application port.

#[path = "routes_request_body.rs"]
pub(super) mod request_body;

#[path = "routes_dispatch.rs"]
mod dispatch;
#[path = "routes_encoding.rs"]
mod encoding;
#[path = "routes_request.rs"]
mod request;
use request::request_for;
#[path = "routes_launch_header.rs"]
mod launch_header;
#[path = "routes_location_headers.rs"]
mod location_headers;
use encoding::bounded_literal;
pub(super) use encoding::json;

use super::pending_application::{ApplicationBinding, RouteState};
use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use grapt_schema::{OptionalNullable, Required};
use thiserror::Error;

use crate::{ApiRefusal, ApiRouterBuilder, RouteMethod, RoutePath, SealedPrincipal};

#[cfg(test)]
use super::application::{
    ApplicationRequest, CatalogApplicationError, FormCancelApplicationError,
    FormCreateApplicationError, FormLookupApplicationError, FormReplyApplicationError,
    PermissionApplicationError, SessionListApplicationError, SessionLookupApplicationError,
    SessionMessagesApplicationError, SessionPromptApplicationError,
};
use super::application::{ApplicationUnavailable, DogfoodApplicationPort};
#[cfg(test)]
use super::{
    InterruptRequestDto, PermissionCreateRequestDto, PermissionReplyRequestDto,
    SessionCreateRequestDto, WireId,
};
use super::{
    InvalidRequestErrorDto, InvalidRequestErrorDtoTag, ServiceUnavailableErrorDto,
    ServiceUnavailableErrorDtoTag, UnauthorizedErrorDto, UnauthorizedErrorDtoTag,
};
#[cfg(test)]
use dispatch::{
    catalog_error_response, form_cancel_error_response, form_create_error_response,
    form_lookup_error_response, form_reply_error_response, session_active_error_response,
    session_list_error_response, session_lookup_error_response, session_messages_error_response,
    session_prompt_error_response,
};

#[derive(Debug, Error)]
pub enum DogfoodRouteRegistrationError {
    #[error("Dogfood-0 route registration was refused: {0}")]
    Route(#[from] ApiRefusal),
}

macro_rules! dogfood_endpoints {
    ($( $variant:ident => $method:ident $path:literal ),+ $(,)?) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub(super) enum Endpoint {
            $( $variant, )+
        }

        impl Endpoint {
            pub(super) const ALL: &'static [Self] = &[$(Self::$variant),+];

            const fn method(self) -> RouteMethod {
                match self {
                    $( Self::$variant => RouteMethod::$method, )+
                }
            }

            const fn path(self) -> &'static str {
                match self {
                    $( Self::$variant => $path, )+
                }
            }
        }
    };
}

dogfood_endpoints! {
    LocationGet => Get "/api/location",
    ProjectList => Get "/api/project",
    ProjectCurrent => Get "/api/project/current",
    ProviderList => Get "/api/provider",
    ModelList => Get "/api/model",
    ModelDefault => Get "/api/model/default",
    AgentList => Get "/api/agent",
    CommandList => Get "/api/command",
    CommandInspect => Get "/api/command/inspect",
    SkillList => Get "/api/skill",
    ReferenceList => Get "/api/reference",
    IntegrationList => Get "/api/integration",
    PluginList => Get "/api/plugin",
    WorkspaceDiff => Get "/api/workspace/diff",
    WorkspaceWorktree => Post "/api/workspace/worktree",

    HookImport => Post "/api/config/import-hooks",
    McpInspect => Get "/api/mcp/inspect",
    McpList => Get "/api/mcp",
    McpConfigurationList => Get "/api/mcp/configuration",
    McpAdd => Put "/api/mcp/{server}",
    McpConfiguration => Post "/api/mcp/{server}/configuration",
    McpReconnect => Post "/api/mcp/{server}/reconnect",
    McpApproval => Get "/api/mcp/{server}/approval",
    McpAuth => Post "/api/mcp/{server}/auth",
    McpContent => Post "/api/mcp/{server}/content",
    McpResources => Get "/api/mcp/resource",
    FormRequestList => Get "/api/form/request",
    QuestionRequestList => Get "/api/question/request",
    ShellList => Get "/api/shell",
    SessionFormList => Get "/api/session/{sessionID}/form",
    SessionFormCreate => Post "/api/session/{sessionID}/form",
    SessionFormGet => Get "/api/session/{sessionID}/form/{formID}",
    SessionFormState => Get "/api/session/{sessionID}/form/{formID}/state",
    SessionFormReply => Post "/api/session/{sessionID}/form/{formID}/reply",
    SessionFormCancel => Post "/api/session/{sessionID}/form/{formID}/cancel",
    SessionPendingList => Get "/api/session/{sessionID}/pending",
    SessionList => Get "/api/session",
    SessionActive => Get "/api/session/active",
    SessionCreate => Post "/api/session",
    SessionGet => Get "/api/session/{sessionID}",
    SessionDelete => Delete "/api/session/{sessionID}",
    SessionFork => Post "/api/session/{sessionID}/fork",
    SideQuestion => Post "/api/session/{sessionID}/side",
    SessionRename => Post "/api/session/{sessionID}/rename",
    SessionAgentSelect => Post "/api/session/{sessionID}/agent",
    SessionModelSelect => Post "/api/session/{sessionID}/model",
    SessionPresetSelect => Post "/api/session/{sessionID}/preset",
    SessionPresetGet => Get "/api/session/{sessionID}/preset",
    SessionSandboxWorktreeConfigGet => Get "/api/session/{sessionID}/sandbox/worktree-config",
    SessionSandboxWorktreeConfigCreate => Post "/api/session/{sessionID}/sandbox/worktree-config",
    SessionSubagentModelGet => Get "/api/session/{sessionID}/subagent-model",
    SessionSubagentModelSet => Put "/api/session/{sessionID}/subagent-model",
    SessionSubagentModelClear => Delete "/api/session/{sessionID}/subagent-model",
    SessionCapabilityPut => Put "/api/session/{sessionID}/stateengine/capability",
    HostInput => Get "/api/session/{sessionID}/input/{messageID}",
    HostNoticeBinding => Get "/api/session/{sessionID}/stateengine/notice",
    HostNotice => Post "/api/session/{sessionID}/stateengine/notice",
    HostNativeMailResume => Post "/api/session/{sessionID}/stateengine/resume",
    InstructionEntryList => Get "/api/session/{sessionID}/instructions/entries",
    InstructionInspect => Get "/api/session/{sessionID}/instructions/inspect",
    InstructionEntryPut => Put "/api/session/{sessionID}/instructions/entries/{key}",
    InstructionEntryRemove => Delete "/api/session/{sessionID}/instructions/entries/{key}",
    SessionMessages => Get "/api/session/{sessionID}/message",
    SessionMessage => Get "/api/session/{sessionID}/message/{messageID}",
    SessionSnapshot => Get "/api/session/{sessionID}/snapshot",
    SessionPrompt => Post "/api/session/{sessionID}/prompt",
    SessionInterrupt => Post "/api/session/{sessionID}/interrupt",
    SessionCompact => Post "/api/session/{sessionID}/compact",
    SessionRewindPreview => Post "/api/session/{sessionID}/rewind/preview",
    SessionRewind => Post "/api/session/{sessionID}/rewind",
    SessionUndoPreview => Post "/api/session/{sessionID}/undo/preview",
    SessionUndo => Post "/api/session/{sessionID}/undo",
    SessionReconcile => Post "/api/session/{sessionID}/reconcile",
    PermissionList => Get "/api/permission/request",
    PermissionSavedList => Get "/api/permission/saved",
    SandboxGrantList => Get "/api/session/{sessionID}/sandbox-grants",
    SandboxGrantRevoke => Post "/api/session/{sessionID}/sandbox-grants/revoke",
    SessionPermissionCreate => Post "/api/session/{sessionID}/permission",
    SessionPermissionList => Get "/api/session/{sessionID}/permission",
    SessionPermissionGet => Get "/api/session/{sessionID}/permission/{requestID}",
    SessionPermissionReply => Post "/api/session/{sessionID}/permission/{requestID}/reply",
}

pub const DOGFOOD_NON_SSE_ROUTE_COUNT: usize = Endpoint::ALL.len();

impl Endpoint {
    const fn success_status(self) -> StatusCode {
        match self {
            Self::SessionInterrupt
            | Self::McpReconnect
            | Self::McpConfiguration
            | Self::McpAdd
            | Self::SessionDelete
            | Self::SessionRename
            | Self::SessionAgentSelect
            | Self::SessionModelSelect
            | Self::SessionPresetSelect
            | Self::SessionSubagentModelSet
            | Self::SessionSubagentModelClear
            | Self::SessionPermissionReply
            | Self::SessionFormReply
            | Self::SessionFormCancel
            | Self::SessionCapabilityPut
            | Self::InstructionEntryPut
            | Self::InstructionEntryRemove => StatusCode::NO_CONTENT,
            _ => StatusCode::OK,
        }
    }
}

/// Registers the exact non-SSE Dogfood-0 route set.
///
/// `/api/health` remains owned by `product_health`; this function neither
/// replaces nor wraps it. When no application owner is supplied, all thirteen
/// routes remain present and answer with the typed unavailable vocabulary.
pub fn register_dogfood_routes(
    builder: &mut ApiRouterBuilder,
    application: Arc<dyn DogfoodApplicationPort>,
) -> Result<(), DogfoodRouteRegistrationError> {
    register_selected(builder, application, Endpoint::ALL.iter().copied())
}

/// Foreground volatile runs expose only memory-owned session operations.
/// Configuration, external-service login, provisioning and restore routes are
/// deliberately absent rather than delegated to persistent product handlers.
pub fn register_ephemeral_dogfood_routes(
    builder: &mut ApiRouterBuilder,
    application: Arc<dyn DogfoodApplicationPort>,
) -> Result<(), DogfoodRouteRegistrationError> {
    register_selected(
        builder,
        application,
        Endpoint::ALL.iter().copied().filter(|endpoint| {
            matches!(
                endpoint,
                Endpoint::LocationGet
                    | Endpoint::ProjectCurrent
                    | Endpoint::ProjectList
                    | Endpoint::ProviderList
                    | Endpoint::ModelList
                    | Endpoint::ModelDefault
                    | Endpoint::SessionCreate
                    | Endpoint::SessionGet
                    | Endpoint::SessionList
                    | Endpoint::SessionActive
                    | Endpoint::SessionPendingList
                    | Endpoint::SessionRename
                    | Endpoint::SessionModelSelect
                    | Endpoint::SessionPresetSelect
                    | Endpoint::SessionPresetGet
                    | Endpoint::SessionMessages
                    | Endpoint::SessionMessage
                    | Endpoint::SessionPrompt
                    | Endpoint::SessionInterrupt
                    | Endpoint::PermissionList
                    | Endpoint::SessionPermissionList
                    | Endpoint::SessionPermissionGet
                    | Endpoint::SessionPermissionReply
                    | Endpoint::SessionFormList
                    | Endpoint::SessionFormGet
                    | Endpoint::SessionFormState
                    | Endpoint::SessionFormReply
                    | Endpoint::SessionFormCancel
            )
        }),
    )
}

fn register_selected(
    builder: &mut ApiRouterBuilder,
    application: Arc<dyn DogfoodApplicationPort>,
    endpoints: impl Iterator<Item = Endpoint>,
) -> Result<(), DogfoodRouteRegistrationError> {
    register_binding(builder, ApplicationBinding::Fixed(application), endpoints)
}

pub(super) fn register_binding(
    builder: &mut ApiRouterBuilder,
    application: ApplicationBinding,
    endpoints: impl Iterator<Item = Endpoint>,
) -> Result<(), DogfoodRouteRegistrationError> {
    let state = Arc::new(RouteState { application });
    for endpoint in endpoints {
        let handler_state = Arc::clone(&state);
        builder.register(
            endpoint.method(),
            RoutePath::try_new(endpoint.path())?,
            move |request: Request<Body>| {
                let handler_state = Arc::clone(&handler_state);
                async move { handle(handler_state, endpoint, request).await }
            },
        )?;
    }
    Ok(())
}

async fn handle(state: Arc<RouteState>, endpoint: Endpoint, request: Request<Body>) -> Response {
    let Some(principal) = request.extensions().get::<SealedPrincipal>().cloned() else {
        return unauthorized_response();
    };
    let application_request = match request_for(endpoint, request).await {
        Ok(request) => request,
        Err(response) => return response,
    };
    let application = match &state.application {
        ApplicationBinding::Fixed(application) => Arc::clone(application),
        ApplicationBinding::Pending(slot, setup) => match slot.application() {
            Some(application) => application,
            None => {
                return super::pending_application::dispatch_setup(
                    setup.as_deref(),
                    principal,
                    application_request,
                )
                .await
            }
        },
    };
    dispatch::execute_application(application.as_ref(), principal, application_request).await
}

pub(super) fn invalid_request_response(message: &'static str) -> Response {
    let Some(message) = bounded_literal(message) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let body = InvalidRequestErrorDto::new(
        Required::new(InvalidRequestErrorDtoTag::InvalidRequestError),
        Required::new(message),
        OptionalNullable::absent(),
        OptionalNullable::absent(),
    );
    json(StatusCode::BAD_REQUEST, &body)
}

pub(crate) fn unauthorized_response() -> Response {
    let Some(message) = bounded_literal("authentication required") else {
        return StatusCode::UNAUTHORIZED.into_response();
    };
    let body = UnauthorizedErrorDto::new(
        Required::new(UnauthorizedErrorDtoTag::UnauthorizedError),
        Required::new(message),
    );
    json(StatusCode::UNAUTHORIZED, &body)
}

pub(super) fn unavailable_response(
    unavailable: Result<ApplicationUnavailable, super::ApplicationUnavailableError>,
) -> Response {
    let Ok(unavailable) = unavailable else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(message) = bounded_literal("service unavailable") else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let Some(service) = bounded_literal(unavailable.service()) else {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    };
    let body = ServiceUnavailableErrorDto::new(
        Required::new(ServiceUnavailableErrorDtoTag::ServiceUnavailableError),
        Required::new(message),
        OptionalNullable::value(service),
    );
    json(StatusCode::SERVICE_UNAVAILABLE, &body)
}

#[cfg(test)]
#[path = "routes_tests.rs"]
mod tests;
