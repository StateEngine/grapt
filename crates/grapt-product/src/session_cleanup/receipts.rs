//! Read-only receipt corroboration. Never settle, reclaim or repair receipts;
//! permanent provider accounting is history, not a live settlement marker.
use std::collections::BTreeMap;
use std::num::NonZeroU32;

use grapt_store::attempt_receipt::{decode_record, AttemptRecordCeiling};
use grapt_types::{SessionId, StepNumber, Timestamp};
use serde_json::Value;

use super::evidence::{protect_session, required, SessionEvidence, Source};

pub(super) fn protect(
    source: &Source<'_>,
    sessions: &mut BTreeMap<SessionId, SessionEvidence>,
    cutoff: i64,
) -> Result<(), String> {
    let ceiling = AttemptRecordCeiling::new(NonZeroU32::new(65536).ok_or("invalid receipt bound")?);
    // Every indexed coordinate and its canonical binary counterpart come from
    // the same row visit, under the same cumulative scan/byte budgets.
    source.records("SELECT
        CASE WHEN length(CAST(r.session_id AS BLOB))<=512 THEN r.session_id END,
        CASE WHEN length(CAST(r.attempt_id AS BLOB))<=512 THEN r.attempt_id END,
        CAST(r.step AS TEXT), CAST(r.ordinal AS TEXT), CAST(r.writer_epoch AS TEXT),
        CASE WHEN length(CAST(r.terminal_event_id AS BLOB))<=512 THEN r.terminal_event_id END,
        CAST(r.terminal_created_ms AS TEXT), CAST(r.reserved_ms AS TEXT),
        CAST(r.request_sent_ms AS TEXT), CAST(r.settled_ms AS TEXT),
        CASE WHEN length(CAST(e.type AS BLOB))<=256 THEN e.type END,
        CASE WHEN length(CAST(e.data AS BLOB))<=65536 THEN e.data END,
        CASE WHEN length(r.record_bytes)<=65536 THEN r.record_bytes END
        FROM attempt_receipt r LEFT JOIN event e ON e.id=r.terminal_event_id
          AND e.aggregate_family='session' AND e.aggregate_id=r.session_id
          AND e.created=r.terminal_created_ms LIMIT 100001",
        |[session, attempt, step, ordinal, epoch, terminal, terminal_ms, reserved, sent, settled, kind, data], bytes| {
            let record = decode_record(&bytes.ok_or("missing or oversized receipt bytes")?, ceiling)?;
            let receipt = record.receipt();
            if required(session)? != receipt.session().as_str()
                || required(attempt)? != receipt.attempt().as_str()
                || required(step)? != receipt.step().value().to_string()
                || required(ordinal)? != receipt.ordinal().value().to_string()
                || required(epoch)? != record.writer_epoch().value().to_string()
                || required(terminal)? != record.terminal().event().as_str()
                || required(terminal_ms)? != record.terminal().created_ms().to_string()
                || required(reserved)? != receipt.reserved_at().as_millis().to_string()
                || sent != receipt.request_sent_at().map(|at| at.as_millis().to_string())
                || settled != receipt.settled_at().map(|at| at.as_millis().to_string()) {
                return Err("receipt columns disagree with their canonical owner record".into());
            }
            let reserved = receipt.reserved_at().as_millis();
            let sent = receipt.request_sent_at().map(Timestamp::as_millis);
            let settled = receipt.settled_at().map(Timestamp::as_millis);
            if reserved < 0 || record.terminal().created_ms() < 0
                || sent.is_some_and(|sent| sent < reserved)
                || settled.is_some_and(|settled| settled < reserved || sent.is_some_and(|sent| sent > settled)) {
                return Err("receipt marker ordering is malformed".into());
            }
            let proven = match kind {
                Some(kind) => terminal_step(&kind, &required(data)?, receipt.session())? == Some(receipt.step()),
                None => false,
            };
            if settled.is_none_or(|settled| settled >= cutoff) || !proven {
                protect_session(sessions, receipt.session().as_str().to_owned())?;
            }
            Ok(())
        })
}

fn terminal_step(kind: &str, raw: &str, session: &SessionId) -> Result<Option<StepNumber>, String> {
    let data: Value = serde_json::from_str(raw).map_err(|error| error.to_string())?;
    // Native Step terminals own no payload SessionId: the aggregate is their
    // identity. Success codecs reject additions; tolerant failure codecs must
    // not let an added session coordinate contradict the corroborated envelope.
    if data
        .get("sessionID")
        .is_some_and(|value| value.as_str() != Some(session.as_str()))
    {
        return Err("receipt terminal payload identity differs from its aggregate".into());
    }
    let step = match kind {
        "session.step.ended.2" => grapt_event::session_step_ended::decode(&data)
            .map(|fact| fact.step())
            .map_err(|error| error.to_string())?,
        "session.step.failed.2" => grapt_event::session_step_failed::decode(&data)
            .map(|fact| fact.step())
            .map_err(|error| error.to_string())?,
        "session.step.ended.3" => grapt_event::session_step_observed::decode_ended(&data)
            .map(|fact| fact.fact().step())
            .map_err(|error| error.to_string())?,
        "session.step.failed.3" => grapt_event::session_step_observed::decode_failed(&data)
            .map(|fact| fact.fact().step())
            .map_err(|error| error.to_string())?,
        _ => return Ok(None),
    };
    Ok(Some(step))
}
