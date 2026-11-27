//! The public wire event announcing a committed compaction.
use super::*;

pub(super) fn public_ended(
    event: &EventId,
    created: i64,
    seq: u64,
    descriptor: &CompactionDescriptor,
    text: &str,
    recent: &grapt_api::u3a::Utf16Text,
) -> Result<grapt_api::u3a::DogfoodEvent, String> {
    use grapt_api::u3a::{
        AggregateIdDto, CompactionEndedData, CompactionReason, DogfoodEvent, DurableEvent,
        DurableEventRef, FiniteF64, WireId,
    };
    let session = descriptor.source_session();
    let reason = match descriptor.trigger() {
        CompactionTrigger::UserRequested => CompactionReason::Manual,
        _ => CompactionReason::Auto,
    };
    Ok(DogfoodEvent::CompactionEnded(DurableEvent::new(
        WireId::parse(event.as_str().to_owned()).ok_or("compaction event identity is invalid")?,
        FiniteF64::try_new(created as f64).map_err(str::to_owned)?,
        DurableEventRef::new(
            AggregateIdDto::try_new(session.as_str()).map_err(str::to_owned)?,
            seq.into(),
        ),
        CompactionEndedData::new(
            WireId::parse(session.as_str().to_owned())
                .ok_or("compaction session identity is invalid")?,
            reason,
            text.to_owned(),
            recent.to_owned(),
        ),
    )))
}
