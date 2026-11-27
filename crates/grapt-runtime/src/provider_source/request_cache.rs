//! The diagnostic snapshot shares the send-permit transaction, never its lossy feed.
use grapt_event::request_cache::{self, RequestCacheFact};
use grapt_store::{AggregateId, AppendInput, EventStore, FencedWriter, GovernedTransaction};
use grapt_types::{EventId, EventVersion, IdAuthority};

pub(super) fn record(
    tx: &GovernedTransaction<'_>,
    aggregate: &AggregateId,
    writer: FencedWriter,
    ids: &dyn IdAuthority,
    clock: &dyn crate::application::Clock,
    fact: RequestCacheFact,
) -> Result<(), String> {
    if fact.attempt.as_str().len() > 128 {
        return Err("request cache attempt exceeds 128 bytes".into());
    }
    let id = format!("evt_{}", ids.mint().as_str())
        .parse::<EventId>()
        .map_err(|_| "invalid request cache event identity")?;
    let data = request_cache::encode(&fact);
    if data.to_string().len() > request_cache::MAX_RECORD_BYTES {
        return Err("request cache record exceeds byte bound".into());
    }
    EventStore::append_within(
        tx,
        AppendInput {
            id: &id,
            aggregate: aggregate.clone(),
            writer,
            kind_name: request_cache::KIND,
            version: EventVersion::new(1).ok_or("event version")?,
            created_ms: clock.now_ms(),
            data: &data,
        },
        &mut [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}
