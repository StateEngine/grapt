//! Current-owner selection precedes revision ordering, including after restart.
use super::*;
use grapt_session::execution as observations;
use grapt_types::SessionId;
use rusqlite::{OptionalExtension, ToSql};

// One statement fixes the owner and its observation at the same SQLite snapshot.
const CURRENT: &str = "WITH owner AS (
    SELECT id, seq, created, type, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END AS data FROM event
    WHERE aggregate_family = 'session' AND aggregate_id = ?1
    AND type IN ('session.status.started.1', 'session.execution.started.1')
    ORDER BY seq DESC LIMIT 1)
    SELECT CASE WHEN length(CAST(e.id AS BLOB)) <= 256 THEN e.id END,
    e.seq, e.created, e.type,
    CASE WHEN length(CAST(e.data AS BLOB)) <= 4096 THEN e.data END,
    CASE WHEN length(CAST(owner.id AS BLOB)) <= 256 THEN owner.id END,
    owner.seq, owner.created, owner.type,
    CASE WHEN length(CAST(owner.data AS BLOB)) <= 4096 THEN owner.data END
    FROM owner LEFT JOIN event e ON e.aggregate_family = 'session' AND e.aggregate_id = ?1
    AND e.seq >= owner.seq AND e.type IN
    ('session.status.started.1', 'session.status.changed.1', 'session.execution.started.1',
     'session.execution.succeeded.1', 'session.execution.failed.1', 'session.execution.interrupted.1')
    AND (length(CAST(e.data AS BLOB)) > 4096 OR
         json_extract(CASE WHEN length(CAST(e.data AS BLOB)) <= 4096 THEN e.data END, '$.executionID') = json_extract(owner.data, '$.executionID'))
    ORDER BY e.seq DESC LIMIT 1";
const UNSTARTED: &str = "SELECT CASE WHEN length(CAST(id AS BLOB)) <= 256 THEN id END,
    seq, created, type, CASE WHEN type = 'session.created.1' THEN '{}'
      WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
    FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
    AND type IN ('session.created.1', 'session.status.changed.1',
      'session.execution.succeeded.1', 'session.execution.failed.1',
      'session.execution.interrupted.1') ORDER BY seq DESC LIMIT 1";

struct Row {
    position: CommittedPosition,
    kind: String,
    raw: String,
}
type RawRow = (String, i64, i64, String, String);

fn decode_row((event, seq, created_ms, kind, raw): RawRow) -> Result<Row, String> {
    let seq = u64::try_from(seq).map_err(|error| error.to_string())?;
    if created_ms < 0 || seq > 9_007_199_254_740_991 {
        return Err("session status revision or time is invalid".into());
    }
    let event = event
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    Ok(Row {
        position: CommittedPosition {
            event,
            seq,
            created_ms,
        },
        kind,
        raw,
    })
}

fn current(
    connection: &rusqlite::Connection,
    session: &SessionId,
) -> Result<Option<(Row, Row)>, String> {
    let raw: Option<(RawRow, RawRow)> = connection
        .query_row(CURRENT, [session.as_str()], |row| {
            Ok((
                (
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ),
                (
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ),
            ))
        })
        .optional()
        .map_err(|error| format!("session status unreadable: {error}"))?;
    raw.map(|(owner, observation)| Ok((decode_row(owner)?, decode_row(observation)?)))
        .transpose()
}

fn unstarted(connection: &rusqlite::Connection, session: &SessionId) -> Result<Snapshot, String> {
    let parameters: &[&dyn ToSql] = &[&session.as_str()];
    let raw: RawRow = connection
        .query_row(UNSTARTED, parameters, |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })
        .map_err(|error| format!("session status source is unavailable: {error}"))?;
    let row = decode_row(raw)?;
    if row.kind != "session.created.1" {
        return Err("session status has no owner start record".into());
    }
    Ok(Snapshot {
        position: row.position,
        execution: None,
        state: State::plain(Kind::Idle),
    })
}

pub fn read(connection: &rusqlite::Connection, session: &SessionId) -> Result<Snapshot, String> {
    let Some((owner, row)) = current(connection, session)? else {
        return unstarted(connection, session);
    };
    let execution = if owner.kind == "session.status.started.1" {
        journal::decode(&owner.raw, session, owner.position)?
            .execution
            .ok_or("session status owner is absent")?
    } else {
        let data: serde_json::Value =
            serde_json::from_str(&owner.raw).map_err(|error| error.to_string())?;
        let execution = data
            .get("executionID")
            .and_then(serde_json::Value::as_str)
            .and_then(ExecutionId::new)
            .ok_or("session status execution is missing")?;
        let observations = observations::read(connection, session, &execution)?;
        if observations.first().map(|row| &row.position) != Some(&owner.position) {
            return Err("session status owner disagrees with its lifecycle".into());
        }
        execution
    };
    if matches!(
        row.kind.as_str(),
        "session.status.started.1" | "session.status.changed.1"
    ) {
        return journal::decode(&row.raw, session, row.position);
    }
    lifecycle(connection, session, &execution, row)
}

fn lifecycle(
    connection: &rusqlite::Connection,
    session: &SessionId,
    execution: &ExecutionId,
    row: Row,
) -> Result<Snapshot, String> {
    let observations = observations::read(connection, session, execution)?;
    let observation = observations
        .iter()
        .find(|observation| observation.position == row.position)
        .ok_or("session status source disagrees with execution lifecycle")?;
    let state = match observation.outcome {
        None => State::plain(Kind::Running),
        Some(execution::Outcome::Succeeded) => State::plain(Kind::Completed),
        Some(execution::Outcome::Interrupted) => State::plain(Kind::Interrupted),
        Some(execution::Outcome::Failed) => State::described(
            Kind::Failed,
            Reason::Execution,
            &observation
                .failure
                .as_ref()
                .ok_or("session status failure is missing")?
                .message,
            Some(Action::InspectSession),
        ),
    };
    Ok(Snapshot {
        position: row.position,
        execution: Some(execution.clone()),
        state,
    })
}
