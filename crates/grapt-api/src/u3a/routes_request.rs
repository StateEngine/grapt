//! Bounded decoding of authenticated HTTP requests into the typed application port.

use axum::body::Body;
use axum::http::Request;
use axum::response::Response;

use super::request_body::{body, fork_body, image_prompt_body};
use super::{invalid_request_response, launch_header, location_headers, Endpoint};
use crate::u3a::{
    self as u3a,
    application::ApplicationRequest,
    query::{self, QueryError},
    CompactRequestDto, InterruptRequestDto, PermissionCreateRequestDto, PermissionReplyRequestDto,
    SessionCreateRequestDto, WireId,
};

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn request_for(
    endpoint: Endpoint,
    request: Request<Body>,
) -> Result<ApplicationRequest, Response> {
    let raw_query = request.uri().query();
    match endpoint {
        Endpoint::HostInput => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            let input = message_id(request.uri().path()).map_err(query_error_response)?;
            Ok(ApplicationRequest::HostControl(
                u3a::HostControlRequest::Input { session, input },
            ))
        }
        Endpoint::HostNoticeBinding | Endpoint::HostNotice | Endpoint::HostNativeMailResume => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            let control = if endpoint == Endpoint::HostNoticeBinding {
                u3a::HostControlRequest::Binding { session }
            } else if endpoint == Endpoint::HostNativeMailResume {
                let request = body::<u3a::NativeMailResumeRequestDto>(request).await?;
                u3a::HostControlRequest::Resume { session, request }
            } else {
                let request = body::<u3a::NativeNoticeRequestDto>(request).await?;
                u3a::HostControlRequest::Notice { session, request }
            };
            Ok(ApplicationRequest::HostControl(control))
        }
        Endpoint::SessionCapabilityPut => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionCapabilityPutDto>(request).await?;
            Ok(ApplicationRequest::SessionCapabilityPut {
                session_id,
                request,
            })
        }
        Endpoint::InstructionEntryList => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::InstructionEntryList)
                .map_err(query_error_response)
        }
        Endpoint::InstructionInspect => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::InstructionInspect)
                .map_err(query_error_response)
        }
        Endpoint::InstructionEntryPut | Endpoint::InstructionEntryRemove => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(5)
                .ok_or_else(|| query_error_response(QueryError::InvalidValue { field: "key" }))?;
            let raw = query::percent_decode(encoded).map_err(query_error_response)?;
            let key = u3a::InstructionEntryKeyDto::parse(raw)
                .ok_or_else(|| query_error_response(QueryError::InvalidValue { field: "key" }))?;
            if endpoint == Endpoint::InstructionEntryRemove {
                Ok(ApplicationRequest::InstructionEntryRemove { session_id, key })
            } else {
                let request = body::<u3a::InstructionEntryPutDto>(request).await?;
                Ok(ApplicationRequest::InstructionEntryPut {
                    session_id,
                    key,
                    request,
                })
            }
        }
        Endpoint::IntegrationList => query::project_current(raw_query)
            .map(ApplicationRequest::IntegrationList)
            .map_err(query_error_response),
        Endpoint::PluginList => query::project_current(raw_query)
            .map(ApplicationRequest::PluginList)
            .map_err(query_error_response),
        Endpoint::WorkspaceDiff => query::project_current(raw_query)
            .map(ApplicationRequest::WorkspaceDiff)
            .map_err(query_error_response),
        Endpoint::WorkspaceWorktree => {
            let query = query::project_current(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::WorkspaceWorktreeRequestDto>(request).await?;
            Ok(ApplicationRequest::WorkspaceWorktree { query, request })
        }
        Endpoint::McpInspect => query::mcp_catalog(raw_query)
            .map(ApplicationRequest::McpInspect)
            .map_err(query_error_response),
        Endpoint::McpList => query::mcp_catalog(raw_query)
            .map(ApplicationRequest::McpList)
            .map_err(query_error_response),
        Endpoint::McpConfigurationList => query::mcp_catalog(raw_query)
            .map(ApplicationRequest::McpConfigurationList)
            .map_err(query_error_response),
        Endpoint::McpAdd => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 1_024 {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::McpAddRequestDto>(request).await?;
            Ok(ApplicationRequest::McpAdd {
                server: server.to_owned(),
                query,
                request,
            })
        }
        Endpoint::McpApproval => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            Ok(ApplicationRequest::McpApproval {
                server: server.to_owned(),
                query,
            })
        }
        Endpoint::HookImport => {
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::HookImportRequestDto>(request).await?;
            Ok(ApplicationRequest::HookImport { query, request })
        }
        Endpoint::McpConfiguration => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::McpConfigurationRequestDto>(request).await?;
            Ok(ApplicationRequest::McpConfiguration {
                server: server.to_owned(),
                query,
                request,
            })
        }
        Endpoint::McpReconnect => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            Ok(ApplicationRequest::McpReconnect {
                server: server.to_owned(),
                query,
            })
        }
        Endpoint::McpAuth => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::McpAuthRequestDto>(request).await?;
            Ok(ApplicationRequest::McpAuth {
                server: server.to_owned(),
                query,
                request,
            })
        }
        Endpoint::McpContent => {
            let encoded = request
                .uri()
                .path()
                .trim_start_matches('/')
                .split('/')
                .nth(2)
                .ok_or_else(|| {
                    query_error_response(QueryError::InvalidValue { field: "server" })
                })?;
            let server = query::percent_decode(encoded).map_err(query_error_response)?;
            if server.is_empty() || server.len() > 256 || server.chars().any(char::is_control) {
                return Err(query_error_response(QueryError::InvalidValue {
                    field: "server",
                }));
            }
            let query = query::mcp_catalog(raw_query).map_err(query_error_response)?;
            let request = body::<u3a::McpContentRequestDto>(request).await?;
            Ok(ApplicationRequest::McpContent {
                server: server.to_owned(),
                query,
                request,
            })
        }
        Endpoint::McpResources => query::mcp_catalog(raw_query)
            .map(ApplicationRequest::McpResources)
            .map_err(query_error_response),
        Endpoint::ShellList => query::project_current(raw_query)
            .map(ApplicationRequest::ShellList)
            .map_err(query_error_response),
        Endpoint::QuestionRequestList => query::project_current(raw_query)
            .map(ApplicationRequest::QuestionRequestList)
            .map_err(query_error_response),
        Endpoint::FormRequestList => query::project_current(raw_query)
            .map(ApplicationRequest::FormRequestList)
            .map_err(query_error_response),
        Endpoint::ReferenceList => query::project_current(raw_query)
            .map(ApplicationRequest::ReferenceList)
            .map_err(query_error_response),
        Endpoint::CommandInspect => query::mcp_catalog(raw_query)
            .map(ApplicationRequest::CommandInspect)
            .map_err(query_error_response),
        Endpoint::CommandList => query::project_current(raw_query)
            .map(ApplicationRequest::CommandList)
            .map_err(query_error_response),
        Endpoint::SkillList => query::project_current(raw_query)
            .map(ApplicationRequest::SkillList)
            .map_err(query_error_response),
        Endpoint::LocationGet => query::project_current(raw_query)
            .map(ApplicationRequest::LocationGet)
            .map_err(query_error_response),
        Endpoint::ProjectList => {
            no_query(raw_query).map_err(query_error_response)?;
            Ok(ApplicationRequest::ProjectList)
        }
        Endpoint::ProjectCurrent => query::project_current(raw_query)
            .map(ApplicationRequest::ProjectCurrent)
            .map_err(query_error_response),
        Endpoint::ProviderList => query::provider_list(raw_query)
            .map(ApplicationRequest::ProviderList)
            .map_err(query_error_response),
        Endpoint::ModelList => query::model_list(raw_query)
            .map(ApplicationRequest::ModelList)
            .map_err(query_error_response),
        Endpoint::ModelDefault => query::model_default(raw_query)
            .map(ApplicationRequest::ModelDefault)
            .map_err(query_error_response),
        Endpoint::AgentList => query::agent_list(raw_query)
            .map(ApplicationRequest::AgentList)
            .map_err(query_error_response),
        Endpoint::SessionList => query::session_list(raw_query)
            .map(ApplicationRequest::SessionList)
            .map_err(query_error_response),
        Endpoint::SessionActive => {
            no_query(raw_query).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionActive)
        }
        Endpoint::SessionCreate => {
            no_query(raw_query).map_err(query_error_response)?;
            // Read the launch header BEFORE the body consumes the request, and
            // refuse an unreadable one rather than defaulting it to a windowed
            // launch: a stated headless mode silently read as interactive would
            // arm the close grace against a Session that was meant to run alone.
            let launch = launch_header::launch(request.headers()).map_err(query_error_response)?;
            let request = body::<SessionCreateRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionCreate { request, launch })
        }
        Endpoint::SessionRename => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionRenameRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionRename {
                session_id,
                request,
            })
        }
        Endpoint::SessionAgentSelect => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionAgentSelectDto>(request).await?;
            Ok(ApplicationRequest::SessionAgentSelect {
                session_id,
                request,
            })
        }
        Endpoint::SessionModelSelect => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionModelSelectDto>(request).await?;
            Ok(ApplicationRequest::SessionModelSelect {
                session_id,
                request,
            })
        }
        Endpoint::SessionPresetGet => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::SessionPresetGet)
                .map_err(query_error_response)
        }
        Endpoint::SessionPresetSelect => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionPresetSelectDto>(request).await?;
            Ok(ApplicationRequest::SessionPresetSelect {
                session_id,
                request,
            })
        }
        Endpoint::SessionSandboxWorktreeConfigGet => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::SessionSandboxWorktreeConfigGet)
                .map_err(query_error_response)
        }
        Endpoint::SessionSandboxWorktreeConfigCreate => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SandboxWorktreeConfigRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionSandboxWorktreeConfigCreate {
                session_id,
                request,
            })
        }
        Endpoint::SessionSubagentModelGet | Endpoint::SessionSubagentModelClear => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            Ok(if endpoint == Endpoint::SessionSubagentModelGet {
                ApplicationRequest::SessionSubagentModelGet(session)
            } else {
                ApplicationRequest::SessionSubagentModelClear(session)
            })
        }
        Endpoint::SessionSubagentModelSet => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionModelSelectDto>(request).await?;
            Ok(ApplicationRequest::SessionSubagentModelSet {
                session_id,
                request,
            })
        }
        Endpoint::SessionGet => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::SessionGet)
                .map_err(query_error_response)
        }
        Endpoint::SideQuestion => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SideQuestionRequestDto>(request).await?;
            Ok(ApplicationRequest::SideQuestion {
                session_id,
                request,
            })
        }
        Endpoint::SessionDelete | Endpoint::SessionFork => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            Ok(if endpoint == Endpoint::SessionDelete {
                ApplicationRequest::SessionDelete(session)
            } else {
                ApplicationRequest::SessionFork {
                    session_id: session,
                    request: fork_body(request).await?,
                }
            })
        }
        Endpoint::SessionFormList | Endpoint::SessionPendingList => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            Ok(if endpoint == Endpoint::SessionFormList {
                ApplicationRequest::SessionFormList(session)
            } else {
                ApplicationRequest::SessionPendingList(session)
            })
        }
        Endpoint::SessionFormCreate => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::FormCreateRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionFormCreate {
                session_id,
                request,
            })
        }
        Endpoint::SessionFormGet | Endpoint::SessionFormState => {
            if request
                .uri()
                .path()
                .starts_with("/api/session/global/form/")
            {
                let query = location_headers::global_form(raw_query, request.headers())
                    .map_err(query_error_response)?;
                let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
                return Ok(if endpoint == Endpoint::SessionFormGet {
                    ApplicationRequest::GlobalFormGet { form_id, query }
                } else {
                    ApplicationRequest::GlobalFormState { form_id, query }
                });
            }
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
            Ok(if endpoint == Endpoint::SessionFormGet {
                ApplicationRequest::SessionFormGet {
                    session_id,
                    form_id,
                }
            } else {
                ApplicationRequest::SessionFormState {
                    session_id,
                    form_id,
                }
            })
        }
        Endpoint::SessionFormReply => {
            if request
                .uri()
                .path()
                .starts_with("/api/session/global/form/")
            {
                let query = location_headers::global_form(raw_query, request.headers())
                    .map_err(query_error_response)?;
                let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
                let request = body::<u3a::FormReplyRequestDto>(request).await?;
                return Ok(ApplicationRequest::GlobalFormReply {
                    form_id,
                    query,
                    request,
                });
            }
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::FormReplyRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionFormReply {
                session_id,
                form_id,
                request,
            })
        }
        // Cancel carries no payload in the frozen contract, so it must not
        // read a body: requiring one would refuse the only call a client makes.
        Endpoint::SessionFormCancel => {
            if request
                .uri()
                .path()
                .starts_with("/api/session/global/form/")
            {
                let query = location_headers::global_form(raw_query, request.headers())
                    .map_err(query_error_response)?;
                let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
                return Ok(ApplicationRequest::GlobalFormCancel { form_id, query });
            }
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let form_id = form_id(request.uri().path()).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionFormCancel {
                session_id,
                form_id,
            })
        }
        Endpoint::SessionMessages => {
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let (query, view) = query::message_list(raw_query).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionMessages {
                session_id,
                query,
                view,
            })
        }
        Endpoint::SessionMessage => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let message_id = message_id(request.uri().path()).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionMessage {
                session_id,
                message_id,
            })
        }
        Endpoint::SessionSnapshot => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionSnapshot { session_id })
        }
        Endpoint::SessionPrompt => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = image_prompt_body(request).await?;
            Ok(ApplicationRequest::SessionPrompt {
                session_id,
                request,
            })
        }
        Endpoint::SessionInterrupt => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<InterruptRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionInterrupt {
                session_id,
                request,
            })
        }
        Endpoint::SessionRewindPreview => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionRewindPreviewRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionRewindPreview {
                session_id,
                request,
            })
        }
        Endpoint::SessionRewind => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionRewindRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionRewind {
                session_id,
                request,
            })
        }
        Endpoint::SessionReconcile => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionReconcileRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionReconcile {
                session_id,
                request,
            })
        }
        Endpoint::SessionUndoPreview => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionUndoPreviewRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionUndoPreview {
                session_id,
                request,
            })
        }
        Endpoint::SessionUndo => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SessionUndoRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionUndo {
                session_id,
                request,
            })
        }
        Endpoint::SessionCompact => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<CompactRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionCompact {
                session_id,
                request,
            })
        }
        Endpoint::PermissionList => query::permission_list(raw_query)
            .map(ApplicationRequest::PermissionList)
            .map_err(query_error_response),
        Endpoint::SandboxGrantList => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::SandboxGrantList)
                .map_err(query_error_response)
        }
        Endpoint::SandboxGrantRevoke => {
            no_query(raw_query).map_err(query_error_response)?;
            let session = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<u3a::SandboxGrantDto>(request).await?;
            Ok(ApplicationRequest::SandboxGrantRevoke { session, request })
        }
        Endpoint::PermissionSavedList => query::saved_permission_project(raw_query)
            .map(ApplicationRequest::PermissionSavedList)
            .map_err(query_error_response),
        Endpoint::SessionPermissionCreate => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<PermissionCreateRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionPermissionCreate {
                session_id,
                request,
            })
        }
        Endpoint::SessionPermissionList => {
            no_query(raw_query).map_err(query_error_response)?;
            session_id(request.uri().path())
                .map(ApplicationRequest::SessionPermissionList)
                .map_err(query_error_response)
        }
        Endpoint::SessionPermissionGet => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request_id = permission_id(request.uri().path()).map_err(query_error_response)?;
            Ok(ApplicationRequest::SessionPermissionGet {
                session_id,
                request_id,
            })
        }
        Endpoint::SessionPermissionReply => {
            no_query(raw_query).map_err(query_error_response)?;
            let session_id = session_id(request.uri().path()).map_err(query_error_response)?;
            let request_id = permission_id(request.uri().path()).map_err(query_error_response)?;
            let request = body::<PermissionReplyRequestDto>(request).await?;
            Ok(ApplicationRequest::SessionPermissionReply {
                session_id,
                request_id,
                request,
            })
        }
    }
}

fn no_query(raw: Option<&str>) -> Result<(), QueryError> {
    if raw.is_none_or(str::is_empty) {
        Ok(())
    } else {
        Err(QueryError::UnknownField)
    }
}

fn session_id(path: &str) -> Result<WireId<grapt_types::SessionId>, QueryError> {
    let Some(encoded) = path.trim_start_matches('/').split('/').nth(2) else {
        return Err(QueryError::InvalidValue { field: "sessionID" });
    };
    let decoded = query::percent_decode(encoded)?;
    WireId::parse(decoded).ok_or(QueryError::InvalidValue { field: "sessionID" })
}

fn form_id(path: &str) -> Result<WireId<grapt_types::FormId>, QueryError> {
    let Some(encoded) = path.trim_start_matches('/').split('/').nth(4) else {
        return Err(QueryError::InvalidValue { field: "formID" });
    };
    let decoded = query::percent_decode(encoded)?;
    WireId::parse(decoded).ok_or(QueryError::InvalidValue { field: "formID" })
}

fn message_id(path: &str) -> Result<WireId<grapt_types::MessageId>, QueryError> {
    let Some(encoded) = path.trim_start_matches('/').split('/').nth(4) else {
        return Err(QueryError::InvalidValue { field: "messageID" });
    };
    let decoded = query::percent_decode(encoded)?;
    WireId::parse(decoded).ok_or(QueryError::InvalidValue { field: "messageID" })
}

fn permission_id(path: &str) -> Result<WireId<grapt_types::PermissionId>, QueryError> {
    let Some(encoded) = path.trim_start_matches('/').split('/').nth(4) else {
        return Err(QueryError::InvalidValue { field: "requestID" });
    };
    let decoded = query::percent_decode(encoded)?;
    WireId::parse(decoded).ok_or(QueryError::InvalidValue { field: "requestID" })
}

fn query_error_response(_error: QueryError) -> Response {
    invalid_request_response("invalid query")
}
