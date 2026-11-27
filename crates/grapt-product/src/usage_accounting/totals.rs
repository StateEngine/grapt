use grapt_session::history::accounting::{AccountingAttempt, AccountingSnapshot};
use serde_json::{json, Value};

pub(super) fn usage_outcome(attempt: &AccountingAttempt) -> Value {
    let usage = &attempt.usage;
    if [
        usage.input_total(),
        usage.input_fresh(),
        usage.legacy_input(),
        usage.output(),
        usage.reasoning(),
        usage.cache_read(),
        usage.cache_write(),
        usage.total(),
    ]
    .iter()
    .any(|field| field.is_some())
    {
        json!({"status":"observed"})
    } else if attempt.step_terminal.is_some() {
        json!({"status":"unavailable","reason":"provider-usage-not-recorded"})
    } else {
        json!({"status":"pending","reason":"attempt-not-terminal"})
    }
}

pub(super) fn session(snapshot: &AccountingSnapshot, subtotals: &Value) -> Value {
    let mut observed = 0;
    let mut unavailable = 0;
    let mut pending = 0;
    for attempt in &snapshot.attempts {
        if attempt.step_terminal.is_none() {
            pending += 1;
        }
        match usage_outcome(attempt)["status"].as_str() {
            Some("observed") => observed += 1,
            Some("unavailable") => unavailable += 1,
            _ => {}
        }
    }
    let complete =
        snapshot.enumeration_complete && snapshot.operational_coverage_complete && pending == 0;
    let mut tokens = serde_json::Map::new();
    if let Some(fields) = subtotals.as_object() {
        for (name, field) in fields {
            let known = complete && field["unavailable_attempts"] == 0;
            tokens.insert(name.clone(), if known {
                json!({"status":"observed","tokens":field["tokens"]})
            } else {
                json!({"status":"unavailable","reason":"usage-not-observed-for-every-dispatched-attempt"})
            });
        }
    }
    json!({"status":if complete {"complete"} else {"incomplete"},
        "scope":"dispatched-attempt-outcomes",
        "reason":if !snapshot.enumeration_complete {Some("event-page-bound")}
            else if !snapshot.operational_coverage_complete {Some("historical-attempt-coverage-unavailable")}
            else if pending != 0 {Some("attempt-not-terminal")} else {None},
        "dispatched_attempts":snapshot.attempts.iter().filter(|a|a.send_permitted).count(),
        "observed_usage_attempts":observed,"unavailable_usage_attempts":unavailable,
        "pending_attempts":pending,"tokens":tokens})
}
