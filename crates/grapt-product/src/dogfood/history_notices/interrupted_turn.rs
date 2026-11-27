//! A turn a service stop cut short, as one system row for the user.
//!
//! The row is history, never an instruction: the provider does not read it. It
//! lives here rather than in `history_notices::append` because that function is
//! a legacy size exception that may not grow.
use serde_json::json;

use super::IndexedRow;

/// Append the interrupted-turn rows, oldest first.
pub(super) fn append(
    rows: &mut Vec<IndexedRow>,
    turns: Vec<grapt_session::interrupted_turn::InterruptedTurn>,
) -> Result<(), String> {
    for turn in turns {
        let suffix = turn
            .position
            .event
            .as_str()
            .strip_prefix("evt_")
            .ok_or("interrupted-turn notice event identity has no prefix")?;
        let id = format!("msg_{suffix}");
        let raw = message(&id, turn.position.created_ms, &turn)?;
        rows.push(IndexedRow::rendered(turn.position.seq, id, raw));
    }
    Ok(())
}

/// One row: what stopped the turn, and the action that actually continues it.
fn message(
    id: &str,
    created: i64,
    turn: &grapt_session::interrupted_turn::InterruptedTurn,
) -> Result<String, String> {
    let mut metadata = json!({
        "interruptedTurn": true,
        "executionID": turn.execution.as_str(),
        "messageID": turn.input.as_str(),
    });
    if turn.resume == grapt_session::interrupted_turn::Resume::Resumable {
        metadata["resumable"] = serde_json::Value::Bool(true);
    }
    let message: grapt_api::u3a::SessionMessageSystemDto =
        crate::dogfood::support::decode(json!({
            "type": "system",
            "id": id,
            "metadata": metadata,
            "time": {"created": created},
            "text": turn.text(),
        }))?;
    serde_json::to_string(&message).map_err(|error| error.to_string())
}
