//! Publish compatibility payloads from committed Form aggregate facts.
use grapt_api::{
    u3a::{AggregateIdDto, DogfoodEvent, DurableEvent, DurableEventRef, FiniteF64},
    DogfoodEventHub,
};
use grapt_inbox::Inbox;
use grapt_types::{EventId, FormId};
use serde_json::{json, Value};

pub(super) fn publish(inbox: &Inbox, hub: &DogfoodEventHub, form: &FormId, event: &EventId) {
    if let Err(error) = project(inbox, form, event).and_then(|event| {
        let record = grapt_form::global_catalog::global_form(inbox.store(), form)
            .map_err(|error| error.to_string())?
            .ok_or("global form projection absent during publication")?;
        hub.publish_at(&event, &record.binding.location)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }) {
        hub.invalidate_because(&format!(
            "global form {form}: event {event} could not be published: {error}"
        ));
    }
}

fn project(inbox: &Inbox, form: &FormId, event: &EventId) -> Result<DogfoodEvent, String> {
    let grapt_store::event_publication::FormFact {
        seq,
        created,
        kind,
        data: raw,
    } = grapt_store::event_publication::form_fact(inbox.store(), form, event)?;
    let value: Value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    if created < 0 || value["formID"].as_str() != Some(form.as_str()) {
        return Err("global form fact identity or time".into());
    }
    let seq = u64::try_from(seq).map_err(|error| error.to_string())?;
    let reference =
        DurableEventRef::version_two(AggregateIdDto::try_new(form.as_str())?, seq.into());
    let id = super::decode(json!(event.as_str()))?;
    let created = FiniteF64::try_new(created as f64)?;
    match kind.as_str() {
        "form.created.2" => Ok(DogfoodEvent::FormCreated(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(json!({"sessionID":"global","form":value["form"]}))?,
        ))),
        "form.replied.2" => Ok(DogfoodEvent::FormReplied(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(
                json!({"sessionID":"global","id":form.as_str(),"answer":value["answer"]}),
            )?,
        ))),
        "form.cancelled.2" => Ok(DogfoodEvent::FormCancelled(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(json!({"sessionID":"global","id":form.as_str()}))?,
        ))),
        "form.suspended.2" => Ok(DogfoodEvent::FormSuspended(DurableEvent::new(
            id,
            created,
            reference,
            super::decode(
                json!({"sessionID":"global","formID":form.as_str(),"reason":value["reason"]}),
            )?,
        ))),
        _ => Err("unexpected global form event kind".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_form::{global::GlobalFormBinding, FormService};
    use grapt_store::{AggregateId, EventStore};
    use grapt_types::{FormSuspensionReason, LocationRef, McpElicitationId};

    #[test]
    fn committed_global_facts_encode_the_retained_event_payloads(
    ) -> Result<(), Box<dyn std::error::Error>> {
        for cancelled in [false, true] {
            let mut store = EventStore::open_in_memory()?;
            FormService::initialize(&mut store)?;
            let form: FormId = "frm_feed".parse()?;
            let binding = GlobalFormBinding {
                // CARRIED HERE ON PURPOSE. The wire assertions below have to run
                // against a binding that HAS a dispatch, or "the dispatch does
                // not reach the wire" would hold trivially because there was
                // never one to leak.
                call: Some(grapt_form::global::CallDispatch {
                    call: grapt_types::ToolCallId::new("call_published").ok_or("call identity")?,
                    generation: 5,
                    attempt: Some("pat_published".to_owned()),
                }),
                elicitation: McpElicitationId::new("elicitation").ok_or("elicitation")?,
                connection: McpElicitationId::new("connection").ok_or("connection")?,
                location: LocationRef::new("/form-feed", None)?,
            };
            store.elect_writer(&AggregateId::Form(form.clone()))?;
            let created: EventId = "evt_created".parse()?;
            let suspended: EventId = "evt_suspended".parse()?;
            let terminal: EventId = "evt_terminal".parse()?;
            FormService::create_global_on(
                &mut store,
                &created,
                &form,
                &binding,
                &json!({"id":form.as_str(),"sessionID":"global","title":"Input","fields":[{"key":"name","type":"string","required":true}]}),
                1,
            )?;
            FormService::suspend_on(
                &mut store,
                &suspended,
                &form,
                FormSuspensionReason::ProcessLoss,
                2,
            )?;
            if cancelled {
                FormService::cancel_global_on(&mut store, &terminal, &form, &binding, 3)?;
            } else {
                FormService::answer_global_on(
                    &mut store,
                    &terminal,
                    &form,
                    &binding,
                    &json!({"name":"Ada"}),
                    3,
                )?;
            }
            let inbox = Inbox::new(store)?;
            for (event, expected) in [
                (&created, "form.created"),
                (&suspended, "form.suspended"),
                (
                    &terminal,
                    if cancelled {
                        "form.cancelled"
                    } else {
                        "form.replied"
                    },
                ),
            ] {
                let projected = project(&inbox, &form, event)?;
                let bytes = grapt_api::u3a::encode_page(
                    &projected,
                    grapt_api::u3a::U3aLimits::production()?,
                )?;
                let wire: Value = serde_json::from_slice(&bytes)?;
                assert_eq!(wire["type"], expected);
                assert_eq!(wire["durable"]["version"], 2);
                assert_eq!(wire["data"]["sessionID"], "global");
                if expected == "form.replied" {
                    assert_eq!(wire["data"]["answer"]["name"], "Ada");
                }
                if expected == "form.suspended" {
                    assert_eq!(wire["data"]["reason"], "process-loss");
                }
                // SURFACE IMPACT, TRACED RATHER THAN GREPPED. Absence of the
                // field from `grapt-api` source is not evidence that nothing
                // reaches the wire; this asserts against the ENCODED page the
                // client actually receives.
                //
                // The projector rebuilds each payload field by field and never
                // forwards `owner`, so the dispatch is durable-only. That is a
                // property of `project` and not a guarantee of the shape, which
                // is why it is pinned here: the created payload carries
                // `form` VERBATIM, so a future author who put the dispatch
                // inside `form` instead of inside `owner` WOULD publish it, and
                // this row is what would tell them.
                let text = String::from_utf8(bytes.clone())?;
                assert!(
                    !text.contains("call_published"),
                    "the dispatch identity reached the published page: {text}"
                );
                assert!(
                    wire["data"].get("owner").is_none(),
                    "the owner object reached the published page: {wire}"
                );
            }
        }
        Ok(())
    }
}
