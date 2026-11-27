//! Project committed execution facts through the existing event vocabulary.
use grapt_api::u3a::{
    AggregateIdDto, DogfoodEvent, DurableEvent, DurableEventRef, ExecutionInterruptedData,
    ExecutionSucceededData, FiniteF64, InterruptReason, WireId,
};
use grapt_session::execution::{Observation, Outcome};

pub(in crate::dogfood) fn project(observation: Observation) -> Result<DogfoodEvent, String> {
    let id = WireId::parse(observation.position.event.as_str().to_owned())
        .ok_or("execution event identity is invalid")?;
    let created = FiniteF64::try_new(observation.position.created_ms as f64)?;
    let aggregate = AggregateIdDto::try_new(observation.session.as_str().to_owned())?;
    let durable = DurableEventRef::new(aggregate, observation.position.seq.into());
    let session = WireId::parse(observation.session.as_str().to_owned())
        .ok_or("execution session identity is invalid")?;
    let execution = Some(
        WireId::parse(observation.execution.as_str().to_owned())
            .ok_or_else(|| "execution wire identity is invalid".to_owned())?,
    );
    Ok(match observation.outcome {
        None => DogfoodEvent::ExecutionStarted(DurableEvent::new(
            id,
            created,
            durable,
            ExecutionSucceededData::new(session, execution),
        )),
        Some(Outcome::Succeeded) => DogfoodEvent::ExecutionSucceeded(DurableEvent::new(
            id,
            created,
            durable,
            ExecutionSucceededData::new(session, execution),
        )),
        Some(Outcome::Failed) => {
            let failure = observation
                .failure
                .as_ref()
                .ok_or_else(|| "failed execution has no public failure".to_owned())?;
            let error = crate::dogfood::decode(
                serde_json::json!({"type": failure.name, "message": failure.message}),
            )?;
            DogfoodEvent::ExecutionFailed(DurableEvent::new(
                id,
                created,
                durable,
                grapt_api::u3a::ExecutionFailedData::new(session, execution, error),
            ))
        }
        Some(Outcome::Interrupted) => DogfoodEvent::ExecutionInterrupted(DurableEvent::new(
            id,
            created,
            durable,
            ExecutionInterruptedData::new(
                session,
                execution,
                match observation.interruption {
                    Some(grapt_session::execution::InterruptionReason::Shutdown) => {
                        InterruptReason::Shutdown
                    }
                    Some(grapt_session::execution::InterruptionReason::ServeWindowExpired) => {
                        InterruptReason::ServeWindowExpired
                    }
                    Some(grapt_session::execution::InterruptionReason::Superseded) => {
                        InterruptReason::Superseded
                    }
                    _ => InterruptReason::User,
                },
            ),
        )),
    })
}
