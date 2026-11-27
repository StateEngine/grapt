//! Pure compatibility projection of facts delivered by the committing owner.
//! No lookup by normalized block number, byte length, or newest Session row.
use super::*;
use grapt_event::session_message::{ContentAppendedFact, ContentBlockOpenedFact};
use grapt_session::CommittedPosition;

pub(super) fn started(
    position: &CommittedPosition,
    fact: &ContentBlockOpenedFact,
    session: &SessionId,
    ordinal: u64,
) -> Result<DogfoodEvent, ExecutionRefusal> {
    let event = DurableEvent::new(
        wire_event(&position.event)?,
        finite(position.created_ms)?,
        durable_ref(session, position.seq)?,
        SliceStartedData::new(
            wire_session(session)?,
            wire_message(fact.message())?,
            ordinal.into(),
        ),
    );
    Ok(match fact.kind() {
        grapt_event::session_message::ContentBlockKind::Text => DogfoodEvent::TextStarted(event),
        grapt_event::session_message::ContentBlockKind::Reasoning => {
            DogfoodEvent::ReasoningStarted(event)
        }
    })
}

pub(super) fn ended(
    position: &CommittedPosition,
    fact: &ContentAppendedFact,
    session: &SessionId,
    block: &LiveBlock,
) -> Result<DogfoodEvent, ExecutionRefusal> {
    ended_as(position, fact, session, block, |ordinal| {
        Ok(SliceEndedData::new(
            wire_session(session)?,
            wire_message(fact.message())?,
            ordinal.into(),
            dto_string(&block.text)?,
        ))
    })
}

/// [`ended`] for a block whose whole text would not fit the live event
/// bound: the frame carries the text's length and SHA-256 instead, which a
/// client checks against the deltas it already holds. Overflow only; an
/// accepted deviation from the frozen record (see
/// fixtures/rust-reconstruction/compat/openapi/DEVIATIONS.md).
pub(super) fn ended_elided(
    position: &CommittedPosition,
    fact: &ContentAppendedFact,
    session: &SessionId,
    block: &LiveBlock,
) -> Result<DogfoodEvent, ExecutionRefusal> {
    ended_as(position, fact, session, block, |ordinal| {
        Ok(SliceEndedData::elided(
            wire_session(session)?,
            wire_message(fact.message())?,
            ordinal.into(),
            (block.text.len() as u64).into(),
            dto_string(&sha256(block.text.as_bytes()))?,
        ))
    })
}

fn sha256(bytes: &[u8]) -> String {
    ring::digest::digest(&ring::digest::SHA256, bytes)
        .as_ref()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn ended_as(
    position: &CommittedPosition,
    fact: &ContentAppendedFact,
    session: &SessionId,
    block: &LiveBlock,
    data: impl FnOnce(u64) -> Result<SliceEndedData, ExecutionRefusal>,
) -> Result<DogfoodEvent, ExecutionRefusal> {
    let (_, ordinal) = block
        .public
        .ok_or_else(|| ExecutionRefusal::Run("block was never published".into()))?;
    if fact.message() != block.opening.message()
        || fact.item().kind() != &block.kind
        || fact.item().payload().length() != block.text.len() as u64
        || fact.disposition() != grapt_types::CaptureDisposition::Complete
    {
        return Err(ExecutionRefusal::Run(
            "committed capture disagrees with public content; hydration required".into(),
        ));
    }
    let event = DurableEvent::new(
        wire_event(&position.event)?,
        finite(position.created_ms)?,
        durable_ref(session, position.seq)?,
        data(ordinal)?,
    );
    Ok(match block.kind {
        ContentKind::Text => DogfoodEvent::TextEnded(event),
        ContentKind::Reasoning => DogfoodEvent::ReasoningEnded(event),
        _ => return Err(ExecutionRefusal::Run("unsupported streaming kind".into())),
    })
}

#[cfg(test)]
mod tests;
