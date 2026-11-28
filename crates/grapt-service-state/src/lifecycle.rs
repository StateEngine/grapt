//! Commit execution facts and their service state in one governed unit of work.
use crate::{journal, Change as StatusChange, State};
use grapt_session::execution::{self, Change, Failure, InterruptionReason, Observation, Outcome};
use grapt_store::{AggregateId, EventStore, FencedWriter, GovernedTransaction};
use grapt_types::{DeterministicAuthorities, ExecutionId, SessionId};

pub fn begin(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
) -> Result<Observation, String> {
    record(store, authorities, session, execution, Change::Start)
}

pub fn settle(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    outcome: Outcome,
) -> Result<Observation, String> {
    let change = match outcome {
        Outcome::Succeeded => Change::Succeeded,
        Outcome::Failed => Change::Failed(Failure::Generic),
        Outcome::Interrupted => Change::Interrupted(InterruptionReason::User),
    };
    record(store, authorities, session, execution, change)
}

pub fn settle_failed(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    failure: Failure,
) -> Result<Observation, String> {
    record(
        store,
        authorities,
        session,
        execution,
        Change::Failed(failure),
    )
}

pub fn settle_interrupted(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    reason: InterruptionReason,
) -> Result<Observation, String> {
    record(
        store,
        authorities,
        session,
        execution,
        Change::Interrupted(reason),
    )
}

fn record(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    change: Change,
) -> Result<Observation, String> {
    record_with(
        store,
        authorities,
        session,
        execution,
        change,
        |_, _| Ok(()),
    )
}

/// Commit an execution lifecycle fact and its service state together with a
/// caller-owned transactional hook in the same governed transaction.
pub fn record_with<F>(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    execution: &ExecutionId,
    change: Change,
    on_commit: F,
) -> Result<Observation, String>
where
    F: FnOnce(&GovernedTransaction<'_>, FencedWriter) -> Result<(), String>,
{
    let state = match &change {
        Change::Start => State::observed(None, None),
        Change::Succeeded => State::observed(Some(&(Outcome::Succeeded, Failure::Generic)), None),
        Change::Failed(failure) => State::failed(failure),
        Change::Interrupted(reason) => State::observed(
            Some(&(Outcome::Interrupted, Failure::Generic)),
            Some(*reason),
        ),
    };
    let writer = store
        .fencing_token(&AggregateId::Session(session.clone()))
        .map_err(|error| error.to_string())?;
    let tx = store
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    let prior = execution::read(&tx, session, execution)?;
    let observation = change.append_within(&tx, writer, authorities, session, execution)?;
    if prior.last().map(|prior| &prior.position) != Some(&observation.position) {
        journal::record_within(
            &tx,
            writer,
            authorities,
            session,
            execution,
            state,
            StatusChange::Finish,
        )?;
    }
    on_commit(&tx, writer)?;
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(observation)
}
