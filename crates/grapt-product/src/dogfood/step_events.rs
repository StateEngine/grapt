//! Compatibility Step opening projected from its committed assistant fact.
//! Selection is captured by the executor, not invented by the event publisher.
use grapt_api::u3a::{
    AggregateIdDto, DogfoodEvent, DurableEvent, DurableEventRef, FiniteF64, ModelRefDto,
    StepStartedData, WireId,
};
use grapt_inbox::Inbox;
use grapt_types::{AgentId, MessageId, SessionId, StepNumber};

pub(super) fn opening(
    inbox: &Inbox,
    session: &SessionId,
    step: StepNumber,
    agent: WireId<AgentId>,
    model: ModelRefDto,
) -> Result<(MessageId, DogfoodEvent), String> {
    let grapt_store::event_publication::AssistantOpening {
        id,
        created,
        seq,
        data,
        kind,
    } = grapt_store::event_publication::assistant_opening(inbox.store(), session, step)?;
    if created < 0 || seq < 0 {
        return Err("assistant opening has invalid time or sequence".into());
    }
    let value = serde_json::from_str(&data).map_err(|error| error.to_string())?;
    let fact = if kind == "session.assistant-message.opened.2" {
        grapt_event::session_message::decode_opened_v2(&value)
    } else {
        grapt_event::session_message::decode_opened(&value)
    }
    .map_err(|error| error.to_string())?;
    if let Some(selection) = fact.selection() {
        let expected_variant = model
            .variant()
            .as_present()
            .map(|variant| variant.as_inner().as_str());
        if selection.agent().as_str() != agent.as_inner().as_str()
            || selection.provider().as_str() != model.provider_id().get().as_inner().as_str()
            || selection.model().as_str() != model.id().get().as_inner().as_str()
            || selection.variant().map(|variant| variant.as_str())
                != expected_variant.filter(|value| *value != "default")
        {
            return Err("live Step selection disagrees with its durable opening".into());
        }
    }
    if fact.step() != step {
        return Err("assistant opening names another Step".into());
    }
    let event = DogfoodEvent::StepStarted(DurableEvent::new(
        super::decode(serde_json::json!(id))?,
        FiniteF64::try_new(created as f64)?,
        DurableEventRef::new(
            AggregateIdDto::try_new(session.as_str())?,
            (seq as u64).into(),
        ),
        StepStartedData::new(
            super::decode(serde_json::json!(session.as_str()))?,
            super::decode(serde_json::json!(fact.message().as_str()))?,
            agent,
            model,
        ),
    ));
    Ok((fact.message().clone(), event))
}
