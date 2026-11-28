use std::path::Path;

use grapt_api::u3a::{
    ApplicationFuture, CompactRequestDto, CompactResponseDto, InstructionEntryKeyDto,
    InstructionEntryListDto, InstructionEntryPutDto, InstructionInspectionDto, InterruptRequestDto,
    InvalidRequestErrorDto, MessageListQueryDto, MessageListView, NoContent, PromptRequestDto,
    PromptResponseDto, SandboxWorktreeConfigRequestDto, SandboxWorktreeConfigResponseDto,
    SessionActiveApplicationError, SessionActiveResponseDto, SessionAgentSelectDto,
    SessionCapabilityPutDto, SessionCompactApplicationError, SessionCreateRequestDto,
    SessionForkRequestDto, SessionListApplicationError, SessionListQueryDto,
    SessionLookupApplicationError, SessionMessageApplicationError, SessionMessageResponseDto,
    SessionMessagesApplicationError, SessionMessagesResponseDto, SessionModelSelectDto,
    SessionPendingListResponseDto, SessionPresetResponseDto, SessionPresetSelectDto,
    SessionPromptApplicationError, SessionRenameRequestDto, SessionResponseDto,
    SessionSubagentModelResponseDto, SessionsResponseDto, SideQuestionRequestDto,
    SideQuestionResponseDto, WireId,
};
use grapt_api::SealedPrincipal;
use grapt_types::{MessageId, SessionId, SessionLaunch};

use crate::dogfood::support::{active_read_failure, invalid, ready};
use crate::dogfood::{
    context, history, list, sandbox_worktree_config, selection, session_title, DogfoodApplication,
};

pub(super) fn session_rename(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SessionRenameRequestDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(session_title::manual(
        application,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn session_agent_select(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SessionAgentSelectDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(selection::agent(
        application,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn session_model_select(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SessionModelSelectDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(selection::model(
        application,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn session_preset_get(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<SessionPresetResponseDto, SessionLookupApplicationError> {
    ready(selection::preset_get(application, session_id.into_inner()))
}

pub(super) fn session_preset_select(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SessionPresetSelectDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(selection::preset(
        application,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn session_subagent_model_get(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<SessionSubagentModelResponseDto, SessionLookupApplicationError> {
    ready(selection::subagent_get(
        application,
        session_id.into_inner(),
    ))
}

pub(super) fn session_subagent_model_set(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: Option<SessionModelSelectDto>,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(selection::subagent_set(
        application,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn session_pending_list(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<SessionPendingListResponseDto, SessionLookupApplicationError> {
    ready(application.pending_list(session_id))
}

pub(super) fn session_capability_put(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request: SessionCapabilityPutDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(context::capability(
        application,
        principal,
        session_id.into_inner(),
        request,
    ))
}

pub(super) fn instruction_entry_list(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<InstructionEntryListDto, SessionLookupApplicationError> {
    ready(context::entries(application, session_id.into_inner()))
}

pub(super) fn instruction_inspect(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<InstructionInspectionDto, SessionLookupApplicationError> {
    ready(context::inspect(application, session_id.into_inner()))
}

pub(super) fn instruction_entry_put(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    key: InstructionEntryKeyDto,
    request: InstructionEntryPutDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(context::put(
        application,
        session_id.into_inner(),
        key,
        request,
    ))
}

pub(super) fn instruction_entry_remove(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    key: InstructionEntryKeyDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(context::remove(application, session_id.into_inner(), key))
}

pub(super) fn session_list(
    application: &DogfoodApplication,
    query: SessionListQueryDto,
) -> ApplicationFuture<SessionsResponseDto, SessionListApplicationError> {
    application.read_projection(
        move |application| list::response(application, query),
        |_| SessionListApplicationError::Unavailable,
    )
}

pub(super) fn session_active(
    application: &DogfoodApplication,
) -> ApplicationFuture<SessionActiveResponseDto, SessionActiveApplicationError> {
    application.read_projection(
        // Both the projection's own refusal and the reader wait go through one
        // classifier, so a busy wait cannot become a bad request on either path.
        |application| {
            application
                .active_session_response()
                .map_err(|detail| active_read_failure(&detail))
        },
        |detail| active_read_failure(&detail),
    )
}

pub(super) fn session_create(
    application: &DogfoodApplication,
    request: SessionCreateRequestDto,
    launch: Option<SessionLaunch>,
) -> ApplicationFuture<SessionResponseDto, InvalidRequestErrorDto> {
    let application = application.clone();
    Box::pin(async move {
        let requested_id = request.id().as_value().map(|id| id.as_inner().clone());
        let request = serde_json::to_value(request).map_err(invalid)?;
        let prepared = application
            .prepare_create(&request, requested_id, launch)
            .await?;
        let session = application
            .create_session_record(&prepared.seed)
            .map_err(invalid)?;
        let record = application.get_record(&session).map_err(invalid)?;
        sandbox_worktree_config::observe(
            &application,
            &session,
            Path::new(record.placement.directory()),
        )
        .await;
        application.session_response(&record)
    })
}

pub(super) fn session_get(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
    application.read_session(session_id.into_inner())
}

pub(super) fn session_sandbox_worktree_config_get(
    application: &DogfoodApplication,
    session: WireId<SessionId>,
) -> ApplicationFuture<SandboxWorktreeConfigResponseDto, SessionLookupApplicationError> {
    sandbox_worktree_config::get(application.clone(), session.into_inner())
}

pub(super) fn session_sandbox_worktree_config_create(
    application: &DogfoodApplication,
    session: WireId<SessionId>,
    request: SandboxWorktreeConfigRequestDto,
) -> ApplicationFuture<SandboxWorktreeConfigResponseDto, SessionLookupApplicationError> {
    sandbox_worktree_config::create(application.clone(), session.into_inner(), request)
}

pub(super) fn session_delete(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    ready(context::delete_session(
        application,
        session_id.into_inner(),
    ))
}

pub(super) fn side_question(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SideQuestionRequestDto,
) -> ApplicationFuture<SideQuestionResponseDto, SessionLookupApplicationError> {
    application.answer_side_question(session_id.into_inner(), request)
}

pub(super) fn session_fork(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: SessionForkRequestDto,
) -> ApplicationFuture<SessionResponseDto, SessionLookupApplicationError> {
    ready(application.fork_session_request(session_id.into_inner(), request))
}

pub(super) fn session_messages_view(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    query: MessageListQueryDto,
    view: MessageListView,
) -> ApplicationFuture<SessionMessagesResponseDto, SessionMessagesApplicationError> {
    application.read_projection(
        move |application| {
            history::messages_get(application, &session_id.into_inner(), query, view)
        },
        |_| SessionMessagesApplicationError::Unavailable,
    )
}

pub(super) fn session_message(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    message_id: WireId<MessageId>,
) -> ApplicationFuture<SessionMessageResponseDto, SessionMessageApplicationError> {
    application.read_projection(
        move |application| {
            history::message_get(
                application,
                &session_id.into_inner(),
                &message_id.into_inner(),
            )
        },
        |_| SessionMessageApplicationError::Unavailable,
    )
}

pub(super) fn session_prompt(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request: PromptRequestDto,
) -> ApplicationFuture<PromptResponseDto, SessionPromptApplicationError> {
    application.submit_prompt(principal, session_id, request)
}

pub(super) fn session_interrupt(
    application: &DogfoodApplication,
    principal: SealedPrincipal,
    session_id: WireId<SessionId>,
    request: InterruptRequestDto,
) -> ApplicationFuture<NoContent, SessionLookupApplicationError> {
    application.interrupt_with_pending(principal, session_id, request)
}

pub(super) fn session_compact(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: CompactRequestDto,
) -> ApplicationFuture<CompactResponseDto, SessionCompactApplicationError> {
    ready(application.submit_compact(session_id, request))
}
