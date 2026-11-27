// Copyright 2023-2026 SpaceXAI. Apache-2.0.
// Modified by Grapt: derive historical advisories from existing durable facts.
//! Reconstruct recovery advisories from the Steps that actually consumed them.
//! The cue is request context, never a synthetic human admission.
use super::*;
use grapt_session::history::AssistantMessage;
use grapt_types::{EventSeq, ProviderCompletionDisposition, StepNumber};

pub(super) fn rows(
    inbox: &Inbox,
    session: &SessionId,
    messages: &[AssistantMessage],
    from: Option<EventSeq>,
    ceiling: u64,
) -> Result<Vec<(u64, CodexInputItem)>, String> {
    let by_step: HashMap<_, _> = messages
        .iter()
        .map(|message| (message.step, message))
        .collect();
    let mut cues = Vec::new();
    for message in messages.iter().filter(|message| {
        message.opened_at_seq < ceiling
            && message
                .content
                .as_ref()
                .is_some_and(|content| !content.items().is_empty())
    }) {
        let Some(previous) = message
            .step
            .value()
            .checked_sub(1)
            .and_then(StepNumber::new)
        else {
            continue;
        };
        let predecessor = by_step.get(&previous);
        let opening = if let Some(predecessor) = predecessor {
            matches!(
                predecessor.terminal,
                Some(grapt_session::StepTerminal::Ended {
                    completion: ProviderCompletionDisposition::Truncated
                })
            )
            .then_some(predecessor.opened_at_seq)
        } else {
            // Compaction may have summarized the truncated response. Its facts
            // still prove why the retained response received the recovery cue.
            truncated_before(inbox.store(), session, previous, message.opened_at_seq)?
        };
        let Some(opening) = opening else { continue };
        let before = inbox
            .promoted_input_before(session, EventSeq::new(opening))
            .map_err(|error| error.to_string())?;
        let issuing = inbox
            .promoted_input_before(session, EventSeq::new(message.opened_at_seq))
            .map_err(|error| error.to_string())?;
        let restarted = grapt_store::session_event_queries::bounds::execution_started_between(
            inbox.store(),
            session,
            grapt_types::EventSeq::new(opening),
            grapt_types::EventSeq::new(message.opened_at_seq),
        )?;
        if before.is_none() || before != issuing || restarted {
            // A newer human promotion or execution clears automatic recovery.
            // Neither may acquire a cue merely because its predecessor truncated.
            continue;
        }
        let seq = issuing_position(inbox.store(), session, message)?;
        if from.is_none_or(|from| seq >= from.value()) {
            cues.push((seq, crate::dogfood::production::output_continuation::item()));
        }
    }
    Ok(cues)
}

fn truncated_before(
    store: &EventStore,
    session: &SessionId,
    step: StepNumber,
    before: u64,
) -> Result<Option<u64>, String> {
    let visible =
        grapt_store::history_visibility::HistoryVisibility::read(store.connection(), session)
            .map_err(|error| error.to_string())?
            .sql("e.seq");
    let mut query = store.connection().prepare(&format!(
        "SELECT e.seq, e.type, CASE WHEN length(CAST(e.data AS BLOB)) <= 4096 THEN e.data ELSE NULL END
         FROM event e WHERE e.aggregate_family='session' AND e.aggregate_id=?1 AND {visible}
         AND e.type IN ('session.step.ended.2','session.step.ended.3',
                       'session.assistant-message.opened.1','session.assistant-message.opened.2')
         AND e.seq < ?3 AND CASE WHEN json_valid(e.data)=0 THEN 1
                  WHEN json_type(e.data,'$.step') IS NOT 'integer' THEN 1
                  ELSE json_extract(e.data,'$.step')=?2 END ORDER BY e.seq LIMIT 3"
    )).map_err(|error| error.to_string())?;
    let mut cursor = query
        .query((
            session.as_str(),
            step.value(),
            i64::try_from(before).map_err(|_| "response position exceeds SQLite range")?,
        ))
        .map_err(|error| error.to_string())?;
    let mut opening = None;
    let mut completion = None;
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|error| error.to_string())?)
            .map_err(|_| "negative recovery predecessor position")?;
        let kind: String = row.get(1).map_err(|error| error.to_string())?;
        let raw: Option<String> = row.get(2).map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&raw.ok_or("recovery predecessor exceeds codec bound")?)
            .map_err(|error| error.to_string())?;
        if kind.starts_with("session.assistant-message.opened.") {
            if opening.is_some() {
                return Err("recovery predecessor has duplicate openings".into());
            }
            let fact = if kind.ends_with(".2") {
                grapt_event::session_message::decode_opened_v2(&value)
            } else {
                grapt_event::session_message::decode_opened(&value)
            }
            .map_err(|error| error.to_string())?;
            if fact.step() != step {
                return Err("recovery opening names another Step".into());
            }
            opening = Some(seq);
        } else {
            if completion.is_some() {
                return Err("recovery predecessor has duplicate terminals".into());
            }
            let fact = if kind == "session.step.ended.3" {
                *grapt_event::session_step_observed::decode_ended(&value)
                    .map_err(|error| error.to_string())?
                    .fact()
            } else {
                grapt_event::session_step_ended::decode(&value)
                    .map_err(|error| error.to_string())?
            };
            if fact.step() != step {
                return Err("recovery predecessor names another Step".into());
            }
            completion = Some(fact.completion());
        }
    }
    if completion == Some(ProviderCompletionDisposition::Truncated) {
        Ok(Some(
            opening.ok_or("truncated recovery predecessor has no opening")?,
        ))
    } else {
        Ok(None)
    }
}

fn issuing_position(
    store: &EventStore,
    session: &SessionId,
    message: &AssistantMessage,
) -> Result<u64, String> {
    // A pre-visibility retry can reuse an earlier message opening. Anchor the
    // cue to the attempt that actually supplied its content, before that
    // request's captured mail, rather than to the discarded attempt's prefix.
    let before = message
        .content
        .as_ref()
        .into_iter()
        .flat_map(|content| content.items())
        .filter_map(|item| item.committed.as_ref().map(|position| position.seq))
        .min()
        .ok_or("recovery response has no committed content")?;
    let mut query = store
        .connection()
        .prepare(
            "SELECT seq, CASE WHEN length(CAST(data AS BLOB))<=?4 THEN data ELSE NULL END
         FROM event WHERE aggregate_family='session' AND aggregate_id=?1
         AND type='session.request.cache.1' AND seq<?3
         AND CASE WHEN json_valid(data)=0 THEN 1
                  WHEN json_type(data,'$.attempt') IS NOT 'text' THEN 1
                  ELSE json_extract(data,'$.attempt')=?2 END ORDER BY seq LIMIT 2",
        )
        .map_err(|error| error.to_string())?;
    let mut cursor = query
        .query((
            session.as_str(),
            message.usage_attempt.as_str(),
            i64::try_from(before).map_err(|_| "response position exceeds SQLite range")?,
            i64::try_from(grapt_event::request_cache::MAX_RECORD_BYTES)
                .map_err(|_| "request codec bound")?,
        ))
        .map_err(|error| error.to_string())?;
    let mut found = None;
    while let Some(row) = cursor.next().map_err(|error| error.to_string())? {
        if found.is_some() {
            return Err("recovery response has duplicate request reservations".into());
        }
        let seq = u64::try_from(row.get::<_, i64>(0).map_err(|error| error.to_string())?)
            .map_err(|_| "negative recovery request position")?;
        let raw: Option<String> = row.get(1).map_err(|error| error.to_string())?;
        let value = serde_json::from_str(&raw.ok_or("recovery request exceeds codec bound")?)
            .map_err(|error| error.to_string())?;
        let fact = grapt_event::request_cache::decode(&value)?;
        if fact.attempt != message.usage_attempt || fact.step != message.step {
            return Err("recovery reservation names another response".into());
        }
        found = Some(seq);
    }
    // Legacy histories and direct owner fixtures predate request fingerprints.
    Ok(found.unwrap_or(message.opened_at_seq))
}
