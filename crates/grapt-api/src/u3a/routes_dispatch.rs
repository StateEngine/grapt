//! Typed application dispatch and error mapping for the authenticated route set.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};

use crate::SealedPrincipal;

use super::super::application::{
    ApplicationRequest, CatalogApplicationError, DogfoodApplicationPort,
    FormCancelApplicationError, FormCreateApplicationError, FormLookupApplicationError,
    FormReplyApplicationError, PermissionApplicationError, SessionActiveApplicationError,
    SessionCompactApplicationError, SessionListApplicationError, SessionLookupApplicationError,
    SessionMessageApplicationError, SessionMessagesApplicationError, SessionPromptApplicationError,
    SessionReconcileApplicationError, SessionRewindApplicationError, SessionUndoApplicationError,
};

use super::{json, unavailable_response, ApplicationUnavailable, Endpoint};

#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) async fn execute_application(
    application: &dyn DogfoodApplicationPort,
    principal: SealedPrincipal,
    request: ApplicationRequest,
) -> Response {
    match request {
        ApplicationRequest::HostControl(request) => {
            match application.host_control(principal, request).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_prompt_error_response(error),
            }
        }
        ApplicationRequest::GlobalFormGet { form_id, query } => {
            match application.global_form_get(principal, form_id, query).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => form_lookup_error_response(error),
            }
        }
        ApplicationRequest::GlobalFormState { form_id, query } => match application
            .global_form_state(principal, form_id, query)
            .await
        {
            Ok(value) => json(StatusCode::OK, &value),
            Err(error) => form_lookup_error_response(error),
        },
        ApplicationRequest::GlobalFormReply {
            form_id,
            query,
            request,
        } => match application
            .global_form_reply(principal, form_id, query, request)
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => form_reply_error_response(error),
        },
        ApplicationRequest::GlobalFormCancel { form_id, query } => match application
            .global_form_cancel(principal, form_id, query)
            .await
        {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => form_cancel_error_response(error),
        },
        ApplicationRequest::SessionRename {
            session_id,
            request,
        } => {
            match application
                .session_rename(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionAgentSelect {
            session_id,
            request,
        } => {
            match application
                .session_agent_select(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionPresetGet(session) => {
            match application.session_preset_get(principal, session).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionPresetSelect {
            session_id,
            request,
        } => {
            match application
                .session_preset_select(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionModelSelect {
            session_id,
            request,
        } => {
            match application
                .session_model_select(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionSandboxWorktreeConfigGet(session) => {
            match application
                .session_sandbox_worktree_config_get(principal, session)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionSandboxWorktreeConfigCreate {
            session_id,
            request,
        } => {
            match application
                .session_sandbox_worktree_config_create(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionSubagentModelGet(session) => {
            match application
                .session_subagent_model_get(principal, session)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionSubagentModelSet {
            session_id,
            request,
        } => {
            match application
                .session_subagent_model_set(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionSubagentModelClear(session) => {
            match application
                .session_subagent_model_clear(principal, session)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SandboxGrantList(session) => {
            match application.sandbox_grant_list(principal, session).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::SandboxGrantRevoke { session, request } => {
            match application
                .sandbox_grant_revoke(principal, session, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::PermissionSavedList(project) => {
            match application.permission_saved_list(principal, project).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::SessionFormList(session) => {
            match application.session_form_list(principal, session).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionFormCreate {
            session_id,
            request,
        } => {
            match application
                .session_form_create(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => form_create_error_response(error),
            }
        }
        ApplicationRequest::SessionFormGet {
            session_id,
            form_id,
        } => {
            match application
                .session_form_get(principal, session_id, form_id)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => form_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionFormState {
            session_id,
            form_id,
        } => {
            match application
                .session_form_state(principal, session_id, form_id)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => form_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionFormReply {
            session_id,
            form_id,
            request,
        } => {
            match application
                .session_form_reply(principal, session_id, form_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => form_reply_error_response(error),
            }
        }
        ApplicationRequest::SessionFormCancel {
            session_id,
            form_id,
        } => {
            match application
                .session_form_cancel(principal, session_id, form_id)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => form_cancel_error_response(error),
            }
        }
        ApplicationRequest::SessionPendingList(session) => {
            match application.session_pending_list(principal, session).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionCapabilityPut {
            session_id,
            request,
        } => {
            match application
                .session_capability_put(principal, session_id, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::InstructionEntryList(session_id) => {
            match application
                .instruction_entry_list(principal, session_id)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::InstructionInspect(session_id) => {
            match application.instruction_inspect(principal, session_id).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::InstructionEntryPut {
            session_id,
            key,
            request,
        } => {
            match application
                .instruction_entry_put(principal, session_id, key, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::InstructionEntryRemove { session_id, key } => {
            match application
                .instruction_entry_remove(principal, session_id, key)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::IntegrationList(query) => {
            match application.integration_list(principal, query).await {
                Ok(value) => json(Endpoint::IntegrationList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::PluginList(query) => {
            match application.plugin_list(principal, query).await {
                Ok(value) => json(Endpoint::PluginList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::WorkspaceDiff(query) => {
            match application.workspace_diff(principal, query).await {
                Ok(value) => json(Endpoint::WorkspaceDiff.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::WorkspaceWorktree { query, request } => {
            match application
                .workspace_worktree(principal, query, request)
                .await
            {
                Ok(value) => json(Endpoint::WorkspaceWorktree.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::HookImport { query, request } => {
            match application.hook_import(principal, query, request).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpInspect(query) => {
            match application.mcp_inspect(principal, query).await {
                Ok(value) => json(Endpoint::McpInspect.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpList(query) => match application.mcp_list(principal, query).await {
            Ok(value) => json(Endpoint::McpList.success_status(), &value),
            Err(error) => json(StatusCode::BAD_REQUEST, &error),
        },
        ApplicationRequest::McpConfigurationList(query) => {
            match application.mcp_configuration_list(principal, query).await {
                Ok(value) => json(Endpoint::McpConfigurationList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpAdd {
            server,
            query,
            request,
        } => match application.mcp_add(principal, server, query, request).await {
            Ok(_) => StatusCode::NO_CONTENT.into_response(),
            Err(error) => json(StatusCode::BAD_REQUEST, &error),
        },
        ApplicationRequest::McpApproval { server, query } => {
            match application.mcp_approval(principal, server, query).await {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpConfiguration {
            server,
            query,
            request,
        } => {
            match application
                .mcp_configuration(principal, server, query, request)
                .await
            {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpReconnect { server, query } => {
            match application.mcp_reconnect(principal, server, query).await {
                Ok(_) => StatusCode::NO_CONTENT.into_response(),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::McpAuth {
            server,
            query,
            request,
        } => match application
            .mcp_auth(principal, server, query, request)
            .await
        {
            Ok(value) => json(Endpoint::McpAuth.success_status(), &value),
            Err(error) => json(StatusCode::BAD_REQUEST, &error),
        },
        ApplicationRequest::McpContent {
            server,
            query,
            request,
        } => match application
            .mcp_content(principal, server, query, request)
            .await
        {
            Ok(value) => json(Endpoint::McpContent.success_status(), &value),
            Err(error) => json(StatusCode::BAD_REQUEST, &error),
        },
        ApplicationRequest::McpResources(query) => {
            match application.mcp_resources(principal, query).await {
                Ok(value) => json(Endpoint::McpResources.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::ShellList(query) => {
            match application.shell_list(principal, query).await {
                Ok(value) => json(Endpoint::ShellList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::QuestionRequestList(query) => {
            match application.question_request_list(principal, query).await {
                Ok(value) => json(Endpoint::QuestionRequestList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::FormRequestList(query) => {
            match application.form_request_list(principal, query).await {
                Ok(value) => json(Endpoint::FormRequestList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::ReferenceList(query) => {
            match application.reference_list(principal, query).await {
                Ok(value) => json(Endpoint::ReferenceList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::CommandInspect(query) => {
            match application.command_inspect(principal, query).await {
                Ok(value) => json(Endpoint::CommandInspect.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::CommandList(query) => {
            match application.command_list(principal, query).await {
                Ok(value) => json(Endpoint::CommandList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::SkillList(query) => {
            match application.skill_list(principal, query).await {
                Ok(value) => json(Endpoint::SkillList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::LocationGet(query) => {
            match application.location_get(principal, query).await {
                Ok(value) => json(Endpoint::LocationGet.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::ProjectList => match application.project_list(principal).await {
            Ok(value) => json(Endpoint::ProjectList.success_status(), &value),
            Err(error) => json(StatusCode::BAD_REQUEST, &error),
        },
        ApplicationRequest::ProjectCurrent(query) => {
            match application.project_current(principal, query).await {
                Ok(value) => json(Endpoint::ProjectCurrent.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::ProviderList(query) => {
            match application.provider_list(principal, query).await {
                Ok(value) => json(Endpoint::ProviderList.success_status(), &value),
                Err(error) => catalog_error_response(error),
            }
        }
        ApplicationRequest::ModelList(query) => {
            match application.model_list(principal, query).await {
                Ok(value) => json(Endpoint::ModelList.success_status(), &value),
                Err(error) => catalog_error_response(error),
            }
        }
        ApplicationRequest::ModelDefault(query) => {
            match application.model_default(principal, query).await {
                Ok(value) => json(Endpoint::ModelDefault.success_status(), &value),
                Err(error) => catalog_error_response(error),
            }
        }
        ApplicationRequest::AgentList(query) => {
            match application.agent_list(principal, query).await {
                Ok(value) => json(Endpoint::AgentList.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::SessionList(query) => {
            match application.session_list(principal, query).await {
                Ok(value) => json(Endpoint::SessionList.success_status(), &value),
                Err(error) => session_list_error_response(error),
            }
        }
        ApplicationRequest::SessionActive => match application.session_active(principal).await {
            Ok(value) => json(Endpoint::SessionActive.success_status(), &value),
            Err(error) => session_active_error_response(error),
        },
        ApplicationRequest::SessionCreate { request, launch } => {
            match application.session_create(principal, request, launch).await {
                Ok(value) => json(Endpoint::SessionCreate.success_status(), &value),
                Err(error) => json(StatusCode::BAD_REQUEST, &error),
            }
        }
        ApplicationRequest::SessionGet(session_id) => {
            match application.session_get(principal, session_id).await {
                Ok(value) => json(Endpoint::SessionGet.success_status(), &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionDelete(session_id) => {
            match application.session_delete(principal, session_id).await {
                Ok(_) => Endpoint::SessionDelete.success_status().into_response(),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SideQuestion {
            session_id,
            request,
        } => {
            match application
                .side_question(principal, session_id, request)
                .await
            {
                Ok(value) => json(Endpoint::SideQuestion.success_status(), &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionFork {
            session_id,
            request,
        } => {
            match application
                .session_fork(principal, session_id, request)
                .await
            {
                Ok(value) => json(Endpoint::SessionFork.success_status(), &value),
                Err(error) => session_lookup_error_response(error),
            }
        }
        ApplicationRequest::SessionMessages {
            session_id,
            query,
            view,
        } => {
            match application
                .session_messages_view(principal, session_id, query, view)
                .await
            {
                Ok(value) => json(Endpoint::SessionMessages.success_status(), &value),
                Err(error) => session_messages_error_response(error),
            }
        }
        ApplicationRequest::SessionSnapshot { session_id } => {
            match application.session_snapshot(principal, session_id).await {
                Ok(value) => json(Endpoint::SessionSnapshot.success_status(), &value),
                Err(error) => session_messages_error_response(error),
            }
        }
        ApplicationRequest::SessionMessage {
            session_id,
            message_id,
        } => {
            match application
                .session_message(principal, session_id, message_id)
                .await
            {
                Ok(value) => json(Endpoint::SessionMessage.success_status(), &value),
                Err(SessionMessageApplicationError::Unavailable) => history_read_unavailable(),
                Err(SessionMessageApplicationError::InvalidRequest(error)) => {
                    json(StatusCode::BAD_REQUEST, &error)
                }
                Err(SessionMessageApplicationError::SessionNotFound(error)) => {
                    json(StatusCode::NOT_FOUND, &error)
                }
                Err(SessionMessageApplicationError::MessageNotFound(error)) => {
                    json(StatusCode::NOT_FOUND, &error)
                }
                Err(SessionMessageApplicationError::Unknown(error)) => {
                    json(StatusCode::INTERNAL_SERVER_ERROR, &error)
                }
            }
        }
        ApplicationRequest::SessionPrompt {
            session_id,
            request,
        } => match application
            .session_prompt(principal, session_id, request)
            .await
        {
            Ok(value) => json(Endpoint::SessionPrompt.success_status(), &value),
            Err(error) => session_prompt_error_response(error),
        },
        ApplicationRequest::SessionInterrupt {
            session_id,
            request,
        } => match application
            .session_interrupt(principal, session_id, request)
            .await
        {
            Ok(_) => Endpoint::SessionInterrupt.success_status().into_response(),
            Err(error) => session_lookup_error_response(error),
        },
        ApplicationRequest::SessionRewindPreview {
            session_id,
            request,
        } => {
            match application
                .session_rewind_preview(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_rewind_error_response(error),
            }
        }
        ApplicationRequest::SessionRewind {
            session_id,
            request,
        } => {
            match application
                .session_rewind(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_rewind_error_response(error),
            }
        }
        ApplicationRequest::SessionReconcile {
            session_id,
            request,
        } => {
            match application
                .session_reconcile(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_reconcile_error_response(error),
            }
        }
        ApplicationRequest::SessionUndoPreview {
            session_id,
            request,
        } => {
            match application
                .session_undo_preview(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_undo_error_response(error),
            }
        }
        ApplicationRequest::SessionUndo {
            session_id,
            request,
        } => {
            match application
                .session_undo(principal, session_id, request)
                .await
            {
                Ok(value) => json(StatusCode::OK, &value),
                Err(error) => session_undo_error_response(error),
            }
        }
        ApplicationRequest::SessionCompact {
            session_id,
            request,
        } => match application
            .session_compact(principal, session_id, request)
            .await
        {
            Ok(value) => json(Endpoint::SessionCompact.success_status(), &value),
            Err(error) => session_compact_error_response(error),
        },
        ApplicationRequest::PermissionList(query) => {
            match application.permission_list(principal, query).await {
                Ok(value) => json(Endpoint::PermissionList.success_status(), &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::SessionPermissionCreate {
            session_id,
            request,
        } => {
            match application
                .session_permission_create(principal, session_id, request)
                .await
            {
                Ok(value) => json(Endpoint::SessionPermissionCreate.success_status(), &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::SessionPermissionList(session_id) => {
            match application
                .session_permission_list(principal, session_id)
                .await
            {
                Ok(value) => json(Endpoint::SessionPermissionList.success_status(), &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::SessionPermissionGet {
            session_id,
            request_id,
        } => {
            match application
                .session_permission_get(principal, session_id, request_id)
                .await
            {
                Ok(value) => json(Endpoint::SessionPermissionGet.success_status(), &value),
                Err(error) => permission_error_response(error),
            }
        }
        ApplicationRequest::SessionPermissionReply {
            session_id,
            request_id,
            request,
        } => match application
            .session_permission_reply(principal, session_id, request_id, request)
            .await
        {
            Ok(_) => Endpoint::SessionPermissionReply
                .success_status()
                .into_response(),
            Err(error) => permission_error_response(error),
        },
    }
}

pub(super) fn catalog_error_response(error: CatalogApplicationError) -> Response {
    match error {
        CatalogApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        CatalogApplicationError::Unavailable(value) => unavailable_response(Ok(value)),
    }
}

fn history_read_unavailable() -> Response {
    let mut response = unavailable_response(ApplicationUnavailable::try_new("history-read"));
    response.headers_mut().insert(
        axum::http::header::RETRY_AFTER,
        axum::http::HeaderValue::from_static("1"),
    );
    response
}

/// A reader that was busy is a transient wait, never a bad request.
///
/// This is the active-session twin of [`history_read_unavailable`]: the same 503
/// and the same `Retry-After`, because a caller that retries on one must retry on
/// the other. Only a genuinely unacceptable request stays a 400.
pub(super) fn session_active_error_response(error: SessionActiveApplicationError) -> Response {
    match error {
        SessionActiveApplicationError::Busy => history_read_unavailable(),
        SessionActiveApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
    }
}

pub(super) fn session_list_error_response(error: SessionListApplicationError) -> Response {
    match error {
        SessionListApplicationError::Unavailable => history_read_unavailable(),
        SessionListApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        SessionListApplicationError::InvalidCursor(value) => json(StatusCode::BAD_REQUEST, &value),
    }
}

pub(super) fn session_lookup_error_response(error: SessionLookupApplicationError) -> Response {
    match error {
        SessionLookupApplicationError::Unavailable => history_read_unavailable(),
        SessionLookupApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionLookupApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
    }
}

pub(super) fn session_messages_error_response(error: SessionMessagesApplicationError) -> Response {
    match error {
        SessionMessagesApplicationError::Unavailable => history_read_unavailable(),
        SessionMessagesApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionMessagesApplicationError::InvalidCursor(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionMessagesApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        SessionMessagesApplicationError::Unknown(value) => {
            json(StatusCode::INTERNAL_SERVER_ERROR, &value)
        }
    }
}

pub(super) fn session_prompt_error_response(error: SessionPromptApplicationError) -> Response {
    match error {
        SessionPromptApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionPromptApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        SessionPromptApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
    }
}

pub(super) fn session_compact_error_response(error: SessionCompactApplicationError) -> Response {
    match error {
        SessionCompactApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionCompactApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        SessionCompactApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
    }
}

// Each form mapper carries only the statuses its own frozen route admits.
// FormAlreadySettled is 409 and never 404: the form EXISTS, the caller is late,
// and a client that saw 404 would conclude the form was gone rather than
// settled. FormInvalidAnswer is 400 and never 409: the answer can be corrected
// and retried, which a settled form can never be.

pub(super) fn form_create_error_response(error: FormCreateApplicationError) -> Response {
    match error {
        FormCreateApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        FormCreateApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormCreateApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
    }
}

pub(super) fn form_lookup_error_response(error: FormLookupApplicationError) -> Response {
    match error {
        FormLookupApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        FormLookupApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormLookupApplicationError::FormNotFound(value) => json(StatusCode::NOT_FOUND, &value),
    }
}

pub(super) fn form_reply_error_response(error: FormReplyApplicationError) -> Response {
    match error {
        FormReplyApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        FormReplyApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormReplyApplicationError::FormNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormReplyApplicationError::FormAlreadySettled(value) => json(StatusCode::CONFLICT, &value),
        FormReplyApplicationError::FormInvalidAnswer(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
    }
}

pub(super) fn form_cancel_error_response(error: FormCancelApplicationError) -> Response {
    match error {
        FormCancelApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        FormCancelApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormCancelApplicationError::FormNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        FormCancelApplicationError::FormAlreadySettled(value) => json(StatusCode::CONFLICT, &value),
    }
}

fn permission_error_response(error: PermissionApplicationError) -> Response {
    match error {
        PermissionApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        PermissionApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        PermissionApplicationError::PermissionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        PermissionApplicationError::PermissionNotLive(value) => json(StatusCode::CONFLICT, &value),
        PermissionApplicationError::Unavailable(value) => unavailable_response(Ok(value)),
    }
}

fn session_rewind_error_response(error: SessionRewindApplicationError) -> Response {
    match error {
        SessionRewindApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionRewindApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        SessionRewindApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
        SessionRewindApplicationError::Unknown(value) => {
            json(StatusCode::INTERNAL_SERVER_ERROR, &value)
        }
    }
}

fn session_undo_error_response(error: SessionUndoApplicationError) -> Response {
    match error {
        SessionUndoApplicationError::InvalidRequest(value) => json(StatusCode::BAD_REQUEST, &value),
        SessionUndoApplicationError::SessionNotFound(value) => json(StatusCode::NOT_FOUND, &value),
        SessionUndoApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
        SessionUndoApplicationError::Unknown(value) => {
            json(StatusCode::INTERNAL_SERVER_ERROR, &value)
        }
    }
}

fn session_reconcile_error_response(error: SessionReconcileApplicationError) -> Response {
    match error {
        SessionReconcileApplicationError::InvalidRequest(value) => {
            json(StatusCode::BAD_REQUEST, &value)
        }
        SessionReconcileApplicationError::SessionNotFound(value) => {
            json(StatusCode::NOT_FOUND, &value)
        }
        SessionReconcileApplicationError::Conflict(value) => json(StatusCode::CONFLICT, &value),
        SessionReconcileApplicationError::Unknown(value) => {
            json(StatusCode::INTERNAL_SERVER_ERROR, &value)
        }
    }
}
