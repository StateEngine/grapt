//! Original patch receipts are canonical facts, independent of plugin output.
use crate::SessionStore;
use grapt_event::session_patch::{self, PatchReceiptFact};
use grapt_store::{AggregateId, AppendInput, EventStore};
use grapt_types::{DeterministicAuthorities, EventId, EventVersion};

pub fn record(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    fact: &PatchReceiptFact,
) -> Result<(), String> {
    SessionStore::over(store)
        .record(fact.session())
        .map_err(|error| error.to_string())?;
    let aggregate = AggregateId::Session(fact.session().clone());
    store
        .elect_writer(&aggregate)
        .map_err(|error| error.to_string())?;
    let writer = store
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let event: EventId = format!("evt_{}", authorities.mint().as_str())
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let data = session_patch::encode(fact);
    let (_, notices) = store
        .append(
            AppendInput {
                id: &event,
                aggregate,
                writer,
                kind_name: "session.patch.receipt",
                version: EventVersion::new(1).ok_or("patch receipt version refused")?,
                created_ms: authorities.now().as_millis(),
                data: &data,
            },
            &mut [],
        )
        .map_err(|error| error.to_string())?;
    notices.discard();
    Ok(())
}
