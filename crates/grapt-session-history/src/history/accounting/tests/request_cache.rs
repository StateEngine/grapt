use super::*;
use grapt_event::request_cache::{self, RequestCacheFact};
use grapt_types::request_cache::{RequestCacheFingerprint, RequestComponents};
fn fact(attempt: &str, step: u32, tool: u8) -> Result<RequestCacheFact, String> {
    Ok(RequestCacheFact {
        attempt: PhysicalAttemptId::new(attempt).ok_or("attempt")?,
        step: StepNumber::new(step).ok_or("step")?,
        fingerprint: Some(
            RequestCacheFingerprint::new(
                RequestComponents {
                    target: [1; 32],
                    cache_key: [2; 32],
                    routing: [3; 32],
                    tools: [tool; 32],
                    instructions: [5; 32],
                    options: [6; 32],
                    input: [7; 32],
                },
                vec![[8; 32]],
                1,
            )
            .ok_or("fingerprint")?,
        ),
    })
}
#[test]
fn durable_request_comparison_survives_read_restart_and_exports_only_its_own_attempt() -> TestResult
{
    let conn = Fixture::new("durable-request-cache")?;
    schema(&conn)?;
    let first = fact("pat_cache_one", 1, 4)?;
    row(
        &conn,
        "ses_cache",
        0,
        "session.request.cache.1",
        &request_cache::encode(&first),
    )?;
    open(&conn, "ses_cache", 1, "pat_cache_one", 1)?;
    let snapshot = conn.read("ses_cache")?;
    let request = snapshot
        .attempts
        .first()
        .and_then(|a| a.request_cache.as_ref())
        .ok_or("request absent")?;
    assert!(request.previous_attempt.is_none() && request.comparison.is_none());
    let second = fact("pat_cache_two", 2, 9)?;
    row(
        &conn,
        "ses_cache",
        2,
        "session.request.cache.1",
        &request_cache::encode(&second),
    )?;
    let opened = AssistantMessageOpenedFact::new(
        second.attempt.clone(),
        second.step,
        "msg_cache_two".parse()?,
    );
    row(
        &conn,
        "ses_cache",
        3,
        "session.assistant-message.opened.1",
        &session_message::encode_opened(&opened),
    )?;
    let snapshot = conn.read("ses_cache")?;
    let request = snapshot
        .attempts
        .iter()
        .find(|a| a.attempt == second.attempt)
        .and_then(|a| a.request_cache.as_ref())
        .ok_or("second request absent")?;
    assert_eq!(request.previous_attempt.as_ref(), Some(&first.attempt));
    assert_eq!(
        request
            .comparison
            .as_ref()
            .ok_or("comparison absent")?
            .changed_components,
        vec!["tools"]
    );
    assert_eq!(request.admitted_ms, 102);
    conn.finish()
}
#[test]
fn durable_request_corruption_refuses_instead_of_becoming_an_unknown_miss() -> TestResult {
    let conn = Fixture::new("request-cache-corruption")?;
    schema(&conn)?;
    let value = request_cache::encode(&fact("pat_cache", 1, 4)?);
    row(
        &conn,
        "ses_cache_negative",
        -1,
        "session.request.cache.1",
        &value,
    )?;
    assert!(conn.read("ses_cache_negative").is_err());
    row(
        &conn,
        "ses_cache_version",
        0,
        "session.request.cache.2",
        &value,
    )?;
    assert!(conn.read("ses_cache_version").is_err());
    row(
        &conn,
        "ses_cache_duplicate",
        0,
        "session.request.cache.1",
        &value,
    )?;
    row(
        &conn,
        "ses_cache_duplicate",
        1,
        "session.request.cache.1",
        &value,
    )?;
    assert!(conn.read("ses_cache_duplicate").is_err());
    row(
        &conn,
        "ses_cache_step",
        0,
        "session.request.cache.1",
        &value,
    )?;
    open(&conn, "ses_cache_step", 1, "pat_cache", 2)?;
    assert!(conn.read("ses_cache_step").is_err());
    conn.finish()
}

#[test]
fn a_usage_only_retry_joins_its_durable_request_step_without_inheriting_a_model() -> TestResult {
    let conn = Fixture::new("request-cache-usage-retry")?;
    schema(&conn)?;
    open(&conn, "ses_cache_retry", 0, "pat_first", 1)?;
    let retry = fact("pat_retry", 1, 4)?;
    row(
        &conn,
        "ses_cache_retry",
        1,
        "session.request.cache.1",
        &request_cache::encode(&retry),
    )?;
    usage(&conn, "ses_cache_retry", 2, "pat_retry", 7)?;
    let ended = grapt_event::session_step_ended::StepEndedFact::new(
        retry.step,
        grapt_types::ProviderCompletionDisposition::AssertedComplete,
    );
    row(
        &conn,
        "ses_cache_retry",
        3,
        "session.step.ended.2",
        &grapt_event::session_step_ended::encode(&ended),
    )?;
    let snapshot = conn.read("ses_cache_retry")?;
    let attempt = snapshot
        .attempts
        .iter()
        .find(|attempt| attempt.attempt == retry.attempt)
        .ok_or("retry omitted")?;
    assert_eq!(attempt.step, Some(retry.step));
    assert!(attempt.terminal_observed);
    assert!(attempt.step_terminal.is_some());
    assert!(attempt.selection.is_none() && attempt.opening.is_none());
    assert!(attempt.request_cache.is_some());
    conn.finish()
}

#[test]
fn an_unrecorded_retry_or_summary_breaks_request_adjacency() -> TestResult {
    for mode in ["usage-only", "summary"] {
        let conn = Fixture::new(&format!("request-cache-legacy-{mode}"))?;
        schema(&conn)?;
        let first = fact("pat_cache_one", 1, 4)?;
        row(
            &conn,
            "ses_cache_legacy",
            0,
            "session.request.cache.1",
            &request_cache::encode(&first),
        )?;
        open(&conn, "ses_cache_legacy", 1, "pat_cache_one", 1)?;
        if mode == "usage-only" {
            usage(&conn, "ses_cache_legacy", 2, "pat_legacy", 7)?;
        } else {
            let coordinates = grapt_event::compaction_call::CallCoordinates {
                compaction: grapt_types::CompactionId::new("cache-legacy").ok_or("compaction")?,
                attempt: PhysicalAttemptId::new("pat_legacy").ok_or("attempt")?,
                step: StepNumber::new(1).ok_or("step")?,
            };
            row(
                &conn,
                "ses_cache_legacy",
                2,
                "session.compaction.call.started.1",
                &grapt_event::compaction_call::encode_started(&coordinates),
            )?;
        }
        let second = fact("pat_cache_two", 2, 9)?;
        row(
            &conn,
            "ses_cache_legacy",
            3,
            "session.request.cache.1",
            &request_cache::encode(&second),
        )?;
        let opened = AssistantMessageOpenedFact::new(
            second.attempt.clone(),
            second.step,
            "msg_cache_two".parse()?,
        );
        row(
            &conn,
            "ses_cache_legacy",
            4,
            "session.assistant-message.opened.1",
            &session_message::encode_opened(&opened),
        )?;
        let snapshot = conn.read("ses_cache_legacy")?;
        let request = snapshot
            .attempts
            .iter()
            .find(|a| a.attempt == second.attempt)
            .and_then(|a| a.request_cache.as_ref())
            .ok_or("second request absent")?;
        assert!(
            request.previous_attempt.is_none() && request.comparison.is_none(),
            "compared across {mode} without its fingerprint"
        );
        conn.finish()?;
    }
    Ok(())
}
