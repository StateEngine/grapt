use grapt_api::u3a::{
    ApplicationFuture, FormCancelApplicationError, FormCreateApplicationError,
    FormCreateRequestDto, FormLookupApplicationError, FormReplyApplicationError,
    FormReplyRequestDto, FormResponseDto, FormStateResponseDto, NoContent,
    ProjectCurrentLocationQueryDto, SessionFormListResponseDto, SessionLookupApplicationError,
    WireId,
};
use grapt_types::{FormId, SessionId};
use serde_json::json;

use crate::dogfood::support::{decode, invalid, ready};
use crate::dogfood::{context, form, global_form, DogfoodApplication};

pub(super) fn session_form_list(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
) -> ApplicationFuture<SessionFormListResponseDto, SessionLookupApplicationError> {
    ready((|| {
        let session = session_id.into_inner();
        context::require_session(application, &session)?;
        let forms = application
            .pending_forms_for_session(&session)
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))?;
        decode(json!({"data": forms.into_iter().map(|form| form.info).collect::<Vec<_>>()}))
            .map_err(|error| SessionLookupApplicationError::InvalidRequest(invalid(error)))
    })())
}

pub(super) fn global_form_get(
    application: &DogfoodApplication,
    form_id: WireId<FormId>,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<FormResponseDto, FormLookupApplicationError> {
    Box::pin(global_form::get(
        application.clone(),
        form_id.into_inner(),
        query,
    ))
}

pub(super) fn global_form_state(
    application: &DogfoodApplication,
    form_id: WireId<FormId>,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<FormStateResponseDto, FormLookupApplicationError> {
    Box::pin(global_form::state(
        application.clone(),
        form_id.into_inner(),
        query,
    ))
}

pub(super) fn global_form_reply(
    application: &DogfoodApplication,
    form_id: WireId<FormId>,
    query: ProjectCurrentLocationQueryDto,
    request: FormReplyRequestDto,
) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
    Box::pin(global_form::reply(
        application.clone(),
        form_id.into_inner(),
        query,
        request,
    ))
}

pub(super) fn global_form_cancel(
    application: &DogfoodApplication,
    form_id: WireId<FormId>,
    query: ProjectCurrentLocationQueryDto,
) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
    Box::pin(global_form::cancel(
        application.clone(),
        form_id.into_inner(),
        query,
    ))
}

pub(super) fn session_form_create(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    request: FormCreateRequestDto,
) -> ApplicationFuture<FormResponseDto, FormCreateApplicationError> {
    form::create_and_notify(application, session_id.into_inner(), request)
}

pub(super) fn session_form_get(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    form_id: WireId<FormId>,
) -> ApplicationFuture<FormResponseDto, FormLookupApplicationError> {
    ready(form::get(
        application,
        session_id.into_inner(),
        form_id.into_inner(),
    ))
}

pub(super) fn session_form_state(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    form_id: WireId<FormId>,
) -> ApplicationFuture<FormStateResponseDto, FormLookupApplicationError> {
    ready(form::state(
        application,
        session_id.into_inner(),
        form_id.into_inner(),
    ))
}

pub(super) fn session_form_reply(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    form_id: WireId<FormId>,
    request: FormReplyRequestDto,
) -> ApplicationFuture<NoContent, FormReplyApplicationError> {
    ready(form::reply(
        application,
        session_id.into_inner(),
        form_id.into_inner(),
        request,
    ))
}

pub(super) fn session_form_cancel(
    application: &DogfoodApplication,
    session_id: WireId<SessionId>,
    form_id: WireId<FormId>,
) -> ApplicationFuture<NoContent, FormCancelApplicationError> {
    ready(form::cancel(
        application,
        session_id.into_inner(),
        form_id.into_inner(),
    ))
}
