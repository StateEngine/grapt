//! Authenticated compatibility routes for location-scoped MCP forms.
use super::{
    form, locations,
    support::{decode, invalid},
    DogfoodApplication,
};
use grapt_api::u3a::*;
use grapt_form::{
    catalog::FormState,
    global_catalog::{global_form, GlobalFormRecord},
    FormService,
};
use grapt_types::{CasOutcome, ClockAuthority, FormId};
use serde_json::json;

#[cfg(test)]
#[path = "global_form_tests.rs"]
mod tests;

async fn record(
    app: &DogfoodApplication,
    id: &FormId,
    query: &ProjectCurrentLocationQueryDto,
) -> Result<GlobalFormRecord, FormLookupApplicationError> {
    let location = locations::resolve(&app.catalog_snapshot(), query)
        .await
        .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))?;
    let selected = grapt_types::LocationRef::new(&location.directory, None)
        .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))?;
    let record = global_form(app.lock_inbox().store(), id)
        .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))?
        .ok_or_else(|| FormLookupApplicationError::FormNotFound(form::form_missing(id)))?;
    if record.binding.location.canonical_id() != selected.canonical_id() {
        return Err(FormLookupApplicationError::FormNotFound(
            form::form_missing(id),
        ));
    }
    Ok(record)
}

pub(crate) fn projected_info(mut record: GlobalFormRecord) -> serde_json::Value {
    if record.info["metadata"]["startup"] == true {
        // Keep list and reply-preflight reads identical without persisting a
        // transient state hint in the form's origin metadata.
        record.info["metadata"]["startupPending"] = json!(record.state == FormState::Pending);
    }
    record.info
}

pub(crate) async fn get(
    app: DogfoodApplication,
    id: FormId,
    query: ProjectCurrentLocationQueryDto,
) -> Result<FormResponseDto, FormLookupApplicationError> {
    let record = record(&app, &id, &query).await?;
    if matches!(record.state, FormState::Answered(_) | FormState::Cancelled) {
        return Err(FormLookupApplicationError::FormNotFound(
            form::form_missing(&id),
        ));
    }
    decode(json!({"data":projected_info(record)}))
        .map_err(|error| FormLookupApplicationError::InvalidRequest(invalid(error)))
}

pub(crate) async fn state(
    app: DogfoodApplication,
    id: FormId,
    query: ProjectCurrentLocationQueryDto,
) -> Result<FormStateResponseDto, FormLookupApplicationError> {
    let record = record(&app, &id, &query).await?;
    form::state_response(&record.state)
}

pub(crate) async fn reply(
    app: DogfoodApplication,
    id: FormId,
    query: ProjectCurrentLocationQueryDto,
    request: FormReplyRequestDto,
) -> Result<NoContent, FormReplyApplicationError> {
    let record = record(&app, &id, &query)
        .await
        .map_err(form::reply_lookup)?;
    let value = serde_json::to_value(request)
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    let answer = value
        .get("answer")
        .ok_or_else(|| FormReplyApplicationError::InvalidRequest(invalid("answer absent")))?;
    if let Some(error) = grapt_form::answer::validate_answer(&record.info["fields"], answer) {
        return Err(FormReplyApplicationError::FormInvalidAnswer(
            form::form_invalid_answer(&id, &error),
        ));
    }
    let event = app
        .mint_event()
        .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    let outcome = FormService::answer_global_on(
        app.lock_inbox().store_mut(),
        &event,
        &id,
        &record.binding,
        answer,
        app.clock.now().as_millis(),
    )
    .map_err(|error| FormReplyApplicationError::InvalidRequest(invalid(error)))?;
    match outcome {
        CasOutcome::Applied => {
            super::global_form_events::publish(&app.lock_inbox(), &app.hub, &id, &event);
            Ok(NoContent)
        }
        CasOutcome::AlreadyTerminal => Err(FormReplyApplicationError::FormAlreadySettled(
            form::form_settled(&id),
        )),
        CasOutcome::NotLive => Err(FormReplyApplicationError::FormNotFound(form::form_missing(
            &id,
        ))),
    }
}

pub(crate) async fn cancel(
    app: DogfoodApplication,
    id: FormId,
    query: ProjectCurrentLocationQueryDto,
) -> Result<NoContent, FormCancelApplicationError> {
    let record = record(&app, &id, &query)
        .await
        .map_err(form::cancel_lookup)?;
    let event = app
        .mint_event()
        .map_err(|error| FormCancelApplicationError::InvalidRequest(invalid(error)))?;
    let outcome = FormService::cancel_global_on(
        app.lock_inbox().store_mut(),
        &event,
        &id,
        &record.binding,
        app.clock.now().as_millis(),
    )
    .map_err(|error| FormCancelApplicationError::InvalidRequest(invalid(error)))?;
    match outcome {
        CasOutcome::Applied => {
            super::global_form_events::publish(&app.lock_inbox(), &app.hub, &id, &event);
            Ok(NoContent)
        }
        CasOutcome::AlreadyTerminal => Err(FormCancelApplicationError::FormAlreadySettled(
            form::form_settled(&id),
        )),
        CasOutcome::NotLive => Err(FormCancelApplicationError::FormNotFound(
            form::form_missing(&id),
        )),
    }
}
