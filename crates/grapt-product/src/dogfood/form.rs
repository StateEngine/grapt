//! The frozen per-form routes over the durable form owner.
//!
//! The two list routes answer "what is still blocking" and are served by the
//! unsettled projection elsewhere. These five answer a different question — what
//! ONE form is, what it ended up as, and how it settles — so they read through
//! the settled-inclusive by-id read and write through the form service.

use grapt_api::u3a::{
    FormCancelApplicationError, FormCreateApplicationError, FormLookupApplicationError,
    FormReplyApplicationError, NoContent,
};
use grapt_api::ApplicationFuture;
use grapt_form::answer::{validate_answer, validate_fields};
use grapt_form::catalog::{form_by_id, form_id_exists, FormRecord, FormState};
use grapt_form::FormService;
use grapt_types::{CasOutcome, ClockAuthority, FormId, IdAuthority, SessionId};
use serde_json::{json, Value};

use super::context;
use super::support::{decode, invalid, not_found, static_dto};
use super::DogfoodApplication;

#[cfg(test)]
#[path = "form_tests.rs"]
mod tests;

pub(crate) fn form_missing(id: &FormId) -> grapt_api::u3a::FormNotFoundErrorDto {
    static_dto(json!({
        "_tag": "FormNotFoundError",
        "id": id.as_str(),
        "message": format!("Form not found: {}", id.as_str()),
    }))
}

pub(crate) fn form_settled(id: &FormId) -> grapt_api::u3a::FormAlreadySettledErrorDto {
    static_dto(json!({
        "_tag": "FormAlreadySettledError",
        "id": id.as_str(),
        "message": format!("Form already settled: {}", id.as_str()),
    }))
}

pub(crate) fn form_invalid_answer(
    id: &FormId,
    message: &str,
) -> grapt_api::u3a::FormInvalidAnswerErrorDto {
    static_dto(json!({
        "_tag": "FormInvalidAnswerError",
        "id": id.as_str(),
        "message": message,
    }))
}

/// Read one form's own record, or the refusal that says why it cannot be read.
///
/// A session that does not exist is a SESSION 404 and a form that does not
/// belong to it is a FORM 404: a caller must be able to tell "no such session"
/// from "no such form in this session", and the by-id read is already scoped to
/// the session so a form cannot be reached through a session that does not own
/// it.
fn record(
    application: &DogfoodApplication,
    session: &SessionId,
    id: &FormId,
) -> Result<FormRecord, FormLookupApplicationError> {
    if context::require_session(application, session).is_err() {
        return Err(FormLookupApplicationError::SessionNotFound(not_found(
            session,
        )));
    }
    let held = application.lock_inbox();
    match form_by_id(held.store(), session, id) {
        Ok(Some(record)) => Ok(record),
        Ok(None) => Err(FormLookupApplicationError::FormNotFound(form_missing(id))),
        Err(error) => Err(FormLookupApplicationError::InvalidRequest(invalid(error))),
    }
}

/// `GET .../form/{formID}` — the UNSETTLED read.
///
/// The frozen `get` restricts to pending and suspended, unlike `state`. A
/// settled form is therefore reported as absent here rather than returned: a
/// caller asking "what is still blocking" must not be handed a form that has
/// already been answered, and `state` is the route that answers the other
/// question.
pub(crate) fn get(
    application: &DogfoodApplication,
    session: SessionId,
    id: FormId,
) -> Result<grapt_api::u3a::FormResponseDto, FormLookupApplicationError> {
    let record = record(application, &session, &id)?;
    if matches!(record.state, FormState::Answered(_) | FormState::Cancelled) {
        return Err(FormLookupApplicationError::FormNotFound(form_missing(&id)));
    }
    decode(json!({"data": record.info}))
        .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))
}

/// `GET .../form/{formID}/state` — the SETTLED-INCLUSIVE read.
///
/// This is the read a client polls after replying, so an answered form has to
/// hand its answer back rather than report absence.
pub(crate) fn state(
    application: &DogfoodApplication,
    session: SessionId,
    id: FormId,
) -> Result<grapt_api::u3a::FormStateResponseDto, FormLookupApplicationError> {
    let record = record(application, &session, &id)?;
    state_response(&record.state)
}

pub(crate) fn state_response(
    state: &FormState,
) -> Result<grapt_api::u3a::FormStateResponseDto, FormLookupApplicationError> {
    use grapt_api::u3a::*;
    use grapt_schema::Required;
    let state = match state {
        FormState::Pending => FormStateDto::Pending(Box::new(FormStatePendingDto::new(
            Required::new(FormPendingStatus::Pending),
        ))),
        FormState::Answered(answer) => FormStateDto::Answered(Box::new(FormStateAnsweredDto::new(
            Required::new(FormAnsweredStatus::Answered),
            Required::new(
                decode(answer.clone())
                    .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))?,
            ),
        ))),
        FormState::Cancelled => FormStateDto::Cancelled(Box::new(FormStateCancelledDto::new(
            Required::new(FormCancelledStatus::Cancelled),
        ))),
        FormState::Suspended(reason) => {
            FormStateDto::Suspended(Box::new(FormStateSuspendedDto::new(
                Required::new(FormSuspendedStatus::Suspended),
                Required::new(match reason {
                    grapt_types::FormSuspensionReason::Shutdown => FormSuspendedReasonDto::Shutdown,
                    grapt_types::FormSuspensionReason::ProcessLoss => {
                        FormSuspendedReasonDto::ProcessLoss
                    }
                }),
            )))
        }
    };
    Ok(FormStateResponseDto::new(Required::new(state)))
}

pub(crate) fn create_and_notify(
    application: &DogfoodApplication,
    session: SessionId,
    request: grapt_api::u3a::FormCreateRequestDto,
) -> ApplicationFuture<grapt_api::u3a::FormResponseDto, FormCreateApplicationError> {
    let wake = application
        .wake_slots
        .reserve(&session)
        .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)));
    let application = application.clone();
    Box::pin(async move {
        let wake = wake?;
        let response = create(&application, session.clone(), request)?;
        if let Err(reason) = wake
            .run_activity(application.executor.waiting_for_input(&session))
            .await
        {
            // The durable form still needs an answer; a notifier cannot cancel or replay it.
            crate::diagnostics::report(format!(
                "session {session}: WaitingForInput hook failed after form creation: {reason}"
            ));
        }
        Ok(response)
    })
}

pub(crate) fn create(
    application: &DogfoodApplication,
    session: SessionId,
    request: grapt_api::u3a::FormCreateRequestDto,
) -> Result<grapt_api::u3a::FormResponseDto, FormCreateApplicationError> {
    if context::require_session(application, &session).is_err() {
        return Err(FormCreateApplicationError::SessionNotFound(not_found(
            &session,
        )));
    }
    let value = serde_json::to_value(request)
        .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)))?;
    let id: FormId = match value.get("id").and_then(Value::as_str) {
        Some(raw) => raw.parse().map_err(|_| {
            FormCreateApplicationError::InvalidRequest(invalid("form id is invalid"))
        })?,
        None => application
            .mint_form()
            .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)))?,
    };
    let Some(title) = value.get("title").and_then(Value::as_str) else {
        return Err(FormCreateApplicationError::InvalidRequest(invalid(
            "form title is absent",
        )));
    };
    let Some(fields) = value.get("fields") else {
        return Err(FormCreateApplicationError::InvalidRequest(invalid(
            "form fields are absent",
        )));
    };
    // The field SPEC is checked before anything is minted: a `when` that points
    // forward would mint a field that can never become active, and therefore a
    // form that can never be answered.
    if let Some(message) = validate_fields(fields) {
        return Err(FormCreateApplicationError::InvalidRequest(invalid(message)));
    }
    let event = application
        .mint_event()
        .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)))?;
    let created_ms = application.clock.now().as_millis();

    // A caller-supplied id that already names a form is a CONFLICT, not a
    // second creation: creating over it would append a repeated creation event
    // that the by-id read refuses as corruption ever after. The check is by ID
    // ALONE, because a form id is unique across the whole projection — a
    // session-scoped check would miss a collision and then fail inside the
    // durable insert, reporting a malformed request instead of a taken id.
    let mut held = application.lock_inbox();
    match form_id_exists(held.store(), &id) {
        Ok(true) => {
            return Err(FormCreateApplicationError::Conflict(static_dto(json!({
                "_tag": "ConflictError",
                "message": format!("Form already exists: {}", id.as_str()),
            }))))
        }
        Ok(false) => {}
        Err(error) => return Err(FormCreateApplicationError::InvalidRequest(invalid(error))),
    }
    FormService::create_on(
        held.store_mut(),
        &event,
        &id,
        &session,
        title,
        fields,
        created_ms,
    )
    .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)))?;
    super::input_events::publish_id(&held, &application.hub, &session, &event);
    let info = json!({
        "id": id.as_str(),
        "sessionID": session.as_str(),
        "title": title,
        "fields": fields,
    });
    decode(json!({"data": info}))
        .map_err(|error| FormCreateApplicationError::InvalidRequest(invalid(error)))
}

pub(crate) fn reply(
    application: &DogfoodApplication,
    session: SessionId,
    id: FormId,
    request: grapt_api::u3a::FormReplyRequestDto,
) -> Result<NoContent, FormReplyApplicationError> {
    let value = serde_json::to_value(request)
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    let Some(answer) = value.get("answer") else {
        return Err(FormReplyApplicationError::InvalidRequest(invalid(
            "form answer is absent",
        )));
    };
    // The answer is validated against THIS form's own fields before anything is
    // appended, so a refused answer leaves the form pending and answerable.
    let record = record(application, &session, &id).map_err(reply_lookup)?;
    if matches!(record.state, FormState::Answered(_) | FormState::Cancelled) {
        return Err(FormReplyApplicationError::FormAlreadySettled(form_settled(
            &id,
        )));
    }
    let Some(fields) = record.info.get("fields") else {
        return Err(FormReplyApplicationError::InvalidRequest(invalid(
            "form fields are absent",
        )));
    };
    if let Some(message) = validate_answer(fields, answer) {
        return Err(FormReplyApplicationError::FormInvalidAnswer(
            form_invalid_answer(&id, &message),
        ));
    }
    // Reuse the Session owner after restart; a cached stale epoch stays fenced.
    application
        .ensure_session_writer(&session)
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    let event = application
        .mint_event()
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    let created_ms = application.clock.now().as_millis();
    let mut held = application.lock_inbox();
    let outcome = FormService::answer_on(held.store_mut(), &event, &id, answer, created_ms)
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    match outcome {
        CasOutcome::Applied => {
            super::input_events::publish_id(&held, &application.hub, &session, &event);
            Ok(NoContent)
        }
        // The row settled between the read above and the write. The answer is
        // still terminal — this is a late caller, not a missing form.
        CasOutcome::AlreadyTerminal => Err(FormReplyApplicationError::FormAlreadySettled(
            form_settled(&id),
        )),
        CasOutcome::NotLive => Err(FormReplyApplicationError::FormNotFound(form_missing(&id))),
    }
}

pub(crate) fn cancel(
    application: &DogfoodApplication,
    session: SessionId,
    id: FormId,
) -> Result<NoContent, FormCancelApplicationError> {
    let record = record(application, &session, &id).map_err(cancel_lookup)?;
    if matches!(record.state, FormState::Answered(_) | FormState::Cancelled) {
        return Err(FormCancelApplicationError::FormAlreadySettled(
            form_settled(&id),
        ));
    }
    application
        .ensure_session_writer(&session)
        .map_err(|error| FormCancelApplicationError::InvalidRequest(invalid(error)))?;
    let event = application
        .mint_event()
        .map_err(|error| FormCancelApplicationError::InvalidRequest(invalid(error)))?;
    let created_ms = application.clock.now().as_millis();
    let mut held = application.lock_inbox();
    let outcome = FormService::cancel_on(held.store_mut(), &event, &id, created_ms)
        .map_err(|error| FormCancelApplicationError::InvalidRequest(invalid(error)))?;
    match outcome {
        CasOutcome::Applied => {
            super::input_events::publish_id(&held, &application.hub, &session, &event);
            Ok(NoContent)
        }
        CasOutcome::AlreadyTerminal => Err(FormCancelApplicationError::FormAlreadySettled(
            form_settled(&id),
        )),
        CasOutcome::NotLive => Err(FormCancelApplicationError::FormNotFound(form_missing(&id))),
    }
}

// A lookup refusal carries into a settlement route unchanged: the same absence
// must not become a different answer because the caller was writing.
pub(crate) fn reply_lookup(error: FormLookupApplicationError) -> FormReplyApplicationError {
    match error {
        FormLookupApplicationError::InvalidRequest(value) => {
            FormReplyApplicationError::InvalidRequest(value)
        }
        FormLookupApplicationError::SessionNotFound(value) => {
            FormReplyApplicationError::SessionNotFound(value)
        }
        FormLookupApplicationError::FormNotFound(value) => {
            FormReplyApplicationError::FormNotFound(value)
        }
    }
}

pub(crate) fn cancel_lookup(error: FormLookupApplicationError) -> FormCancelApplicationError {
    match error {
        FormLookupApplicationError::InvalidRequest(value) => {
            FormCancelApplicationError::InvalidRequest(value)
        }
        FormLookupApplicationError::SessionNotFound(value) => {
            FormCancelApplicationError::SessionNotFound(value)
        }
        FormLookupApplicationError::FormNotFound(value) => {
            FormCancelApplicationError::FormNotFound(value)
        }
    }
}

impl DogfoodApplication {
    fn mint_form(&self) -> Result<FormId, String> {
        format!("frm_{}", self.ids.mint().as_str())
            .parse()
            .map_err(|_| "form identity was invalid".to_owned())
    }
}
