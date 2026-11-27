//! Advisory input and selection events project exact committed aggregate rows, never request
//! bytes or invented sequence anchors. A failed projection invalidates streams;
//! it cannot turn a successful durable admission into a retryable write failure.
use grapt_api::u3a::{AggregateIdDto, DogfoodEvent, DurableEvent, DurableEventRef, FiniteF64};
use grapt_api::DogfoodEventHub;
use grapt_inbox::Inbox;
use grapt_types::SessionId;

#[cfg(test)]
mod tests;

pub(super) fn publish_promoted(
    inbox: &Inbox,
    hub: &DogfoodEventHub,
    session: &SessionId,
    input: &grapt_types::MessageId,
) {
    match inbox.promoted_cursor(session, input) {
        Ok(Some(seq)) => publish(inbox, hub, session, seq),
        Ok(None) | Err(_) => hub.invalidate_because(&format!(
            "session {session}: promoted input cursor unavailable"
        )),
    }
}

pub(super) fn publish_id(
    inbox: &Inbox,
    hub: &DogfoodEventHub,
    session: &SessionId,
    event: &grapt_types::EventId,
) {
    let seq = grapt_store::event_publication::input_sequence(inbox.store(), session, event);
    match seq {
        Ok(Some(seq)) => match u64::try_from(seq) {
            Ok(seq) => publish(inbox, hub, session, seq),
            Err(_) => hub.invalidate_because(&format!(
                "session {session}: input event {event} has a negative sequence {seq}"
            )),
        },
        // Configured/saved permission may grant without emitting a new ask.
        Ok(None) => {}
        Err(error) => hub.invalidate_because(&format!(
            "session {session}: input event {event} sequence unreadable: {error}"
        )),
    }
}

pub(super) fn publish(inbox: &Inbox, hub: &DogfoodEventHub, session: &SessionId, seq: u64) {
    if let Err(error) = project(inbox, session, seq).and_then(|event| {
        hub.publish(&event)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        hub.invalidate_because(&format!(
            "session {session}: input event at sequence {seq} could not be published: {error}"
        ));
    }
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(super) fn project(
    inbox: &Inbox,
    session: &SessionId,
    seq: u64,
) -> Result<DogfoodEvent, String> {
    let grapt_store::event_publication::InputFact {
        id,
        kind,
        created,
        data,
    } = grapt_store::event_publication::input_fact(inbox.store(), session, seq)?;
    let value: serde_json::Value =
        serde_json::from_str(&data).map_err(|error| error.to_string())?;
    if value.get("sessionID").and_then(serde_json::Value::as_str) != Some(session.as_str())
        || created < 0
    {
        return Err("committed input event has invalid identity or time".into());
    }
    let id = super::decode(serde_json::json!(id))?;
    let created = FiniteF64::try_new(created as f64)?;
    let aggregate = AggregateIdDto::try_new(session.as_str())?;
    let reference = match kind.as_str() {
        "permission.asked.2" | "permission.replied.2" => {
            DurableEventRef::version_two(aggregate, seq.into())
        }
        _ => DurableEventRef::new(aggregate, seq.into()),
    };
    match kind.as_str() {
        "permission.asked.2" => {
            let Some(grapt_permission::PermissionEvent::Asked {
                id: permission,
                session: owner,
                action,
                coordinates: Some(coordinates),
            }) = grapt_permission::PermissionEvent::decode("permission.asked", 2, &value)
                .map_err(|error| error.to_string())?
            else {
                return Err("permission ask has no replay coordinates".into());
            };
            let mut data = serde_json::json!({"id":permission.as_str(),"sessionID":owner.as_str(),"action":action,"resources":coordinates.resources});
            if let Some(save) = coordinates.save {
                data["save"] = serde_json::json!(save);
            }
            if let Some(metadata) = coordinates.metadata {
                data["metadata"] = metadata;
            }
            if let Some(source) = coordinates.source {
                data["source"] = source;
            }
            super::permission::include_preview(&mut data);
            Ok(DogfoodEvent::PermissionAsked(DurableEvent::new(
                id,
                created,
                reference,
                super::decode(data)?,
            )))
        }
        "permission.replied.2" => {
            let Some(grapt_permission::PermissionEvent::Replied {
                request,
                session: owner,
                reply,
                ..
            }) = grapt_permission::PermissionEvent::decode("permission.replied", 2, &value)
                .map_err(|error| error.to_string())?
            else {
                return Err("permission reply has no replay coordinates".into());
            };
            let reply = match reply {
                grapt_types::PermissionReply::Once => "once",
                grapt_types::PermissionReply::Always => "always",
                grapt_types::PermissionReply::Session => "session",
                grapt_types::PermissionReply::Reject => "reject",
            };
            Ok(DogfoodEvent::PermissionReplied(DurableEvent::new(
                id,
                created,
                reference,
                super::decode(
                    serde_json::json!({"sessionID":owner.as_str(),"requestID":request.as_str(),"reply":reply}),
                )?,
            )))
        }
        "permission.cancelled.1" => {
            if !matches!(
                grapt_permission::PermissionEvent::decode("permission.cancelled", 1, &value)
                    .map_err(|error| error.to_string())?,
                Some(grapt_permission::PermissionEvent::Cancelled { .. })
            ) {
                return Err("permission cancellation could not be decoded".into());
            }
            Ok(DogfoodEvent::PermissionCancelled(DurableEvent::new(
                id,
                created,
                reference,
                super::decode(value)?,
            )))
        }
        "form.created.1" => Ok(DogfoodEvent::FormCreated(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "form.replied.1" => Ok(DogfoodEvent::FormReplied(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "form.cancelled.1" => Ok(DogfoodEvent::FormCancelled(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "form.suspended.1" => Ok(DogfoodEvent::FormSuspended(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "session.input.admitted.1" => Ok(DogfoodEvent::InputAdmitted(DurableEvent::new(
            id,
            created,
            reference,
            admitted_data(value)?,
        ))),
        "session.input.promoted.1" => Ok(DogfoodEvent::InputPromoted(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "session.subagent-model.selected.1" => Ok(DogfoodEvent::SubagentModelSelected(
            DurableEvent::new(id, created, reference, super::decode(value)?),
        )),
        "session.model.selected.1" => Ok(DogfoodEvent::ModelSelected(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "session.agent.selected.1" => Ok(DogfoodEvent::AgentSelected(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(value)?,
        ))),
        "session.subagent-model.cleared.1" => Ok(DogfoodEvent::SubagentModelCleared(
            DurableEvent::new(id, created, reference, super::decode(value)?),
        )),
        _ => Err("committed notification cursor names another event kind".into()),
    }
}

pub(super) fn admitted_data(
    value: serde_json::Value,
) -> Result<grapt_api::u3a::InputAdmittedData, String> {
    use grapt_api::u3a::{InputAdmittedData, InputAdmittedMessageDto};
    use grapt_schema::Required;
    if value
        .as_object()
        .ok_or("input admission is not an object")?
        .keys()
        .any(|name| !matches!(name.as_str(), "sessionID" | "inputID" | "input"))
    {
        return Err("input admission has an unknown field".into());
    }
    let raw = value.get("input").ok_or("input admission has no input")?;
    let input = if raw.get("type").and_then(serde_json::Value::as_str) == Some("synthetic") {
        if !matches!(
            grapt_inbox::InboxEvent::decode("session.input.admitted", 1, &value)
                .map_err(|error| error.to_string())?,
            Some(grapt_inbox::InboxEvent::SyntheticAdmitted { .. })
        ) {
            return Err("synthetic admission has no Rust authorization".into());
        }
        let mut public = raw.clone();
        let fields = public
            .as_object_mut()
            .ok_or("synthetic input is not an object")?;
        // Internal admission markers do not belong in the public wire DTO.
        for name in ["rustAuthorized", "nativeMailWake", "nativeMailTarget"] {
            fields.remove(name);
        }
        let data = fields
            .get_mut("data")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or("synthetic data is not an object")?;
        for name in ["description", "metadata"] {
            if data.get(name).is_some_and(serde_json::Value::is_null) {
                data.remove(name);
            }
        }
        if let Some(metadata) = data.get_mut("metadata") {
            *metadata = super::input_metadata::public_metadata(std::mem::take(metadata));
        }
        InputAdmittedMessageDto::Synthetic(Box::new(super::decode(public)?))
    } else {
        InputAdmittedMessageDto::User(Box::new(super::decode(raw.clone())?))
    };
    Ok(InputAdmittedData::new(
        Required::new(super::decode(
            value.get("sessionID").cloned().ok_or("session absent")?,
        )?),
        Required::new(super::decode(
            value
                .get("inputID")
                .cloned()
                .ok_or("input identity absent")?,
        )?),
        Required::new(input),
    ))
}
