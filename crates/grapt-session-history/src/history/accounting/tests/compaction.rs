use super::*;
use grapt_event::compaction_call::{self, CallCoordinates, CallEnd};

fn coordinates(attempt: &str) -> Result<CallCoordinates, Box<dyn std::error::Error>> {
    Ok(CallCoordinates {
        compaction: grapt_types::CompactionId::new("summary").ok_or("compaction")?,
        attempt: PhysicalAttemptId::new(attempt).ok_or("attempt")?,
        step: StepNumber::new(1).ok_or("step")?,
    })
}

fn observed(attempt: &str, total: u64, read: u64) -> Result<Value, Box<dyn std::error::Error>> {
    let field = |count| TokenCount::new(count).map(UsageField::reported);
    let usage = AttemptUsage::observed(
        field(total)?,
        field(total - read)?,
        field(2)?,
        UsageField::ABSENT,
        field(read)?,
        field(0)?,
        UsageField::ABSENT,
        UsageObservationPhase::Terminal,
    );
    Ok(session_message::encode_observed(&UsageObservedFact::new(
        PhysicalAttemptId::new(attempt).ok_or("attempt")?,
        usage,
    )))
}

#[test]
fn summary_usage_is_included_once_as_a_separate_provider_operation() -> TestResult {
    let conn = Fixture::new("summary-once")?;
    schema(&conn)?;
    open(&conn, "ses_summary", 0, "assistant", 1)?;
    row(
        &conn,
        "ses_summary",
        1,
        "session.usage.observed.2",
        &observed("assistant", 100, 80)?,
    )?;
    let coordinates = coordinates("summary")?;
    row(
        &conn,
        "ses_summary",
        2,
        "session.compaction.call.started.1",
        &compaction_call::encode_started(&coordinates),
    )?;
    for seq in 3..=4 {
        row(
            &conn,
            "ses_summary",
            seq,
            "session.compaction.call.usage.1",
            &observed("summary", 50, 30)?,
        )?;
    }
    row(
        &conn,
        "ses_summary",
        5,
        "session.compaction.call.ended.1",
        &compaction_call::encode_ended(&coordinates, CallEnd::Completed),
    )?;
    let snapshot = conn.read("ses_summary")?;
    assert_eq!(snapshot.attempts.len(), 2);
    assert_eq!(
        snapshot
            .attempts
            .iter()
            .map(|a| a
                .usage
                .input_total()
                .map(|f| f.count().value())
                .ok_or("input unavailable"))
            .collect::<Result<Vec<_>, _>>()?
            .iter()
            .sum::<u64>(),
        150
    );
    let summary = snapshot
        .attempts
        .iter()
        .find(|a| a.attempt.as_str() == "summary")
        .ok_or("summary missing")?;
    assert_eq!(summary.operation, AccountingOperation::Compaction);
    assert_eq!(
        summary.usage.cache_read().ok_or("read")?.count().value(),
        30
    );
    assert!(
        summary.selection.is_none(),
        "a coding model cannot be invented for the summary"
    );
    assert!(summary.opening.is_some());
    assert_eq!(summary.step.map(StepNumber::value), Some(1));
    conn.finish()
}

#[test]
fn missing_usage_and_missing_opening_remain_distinct() -> TestResult {
    for (label, kind, value) in [
        (
            "no-usage",
            "session.compaction.call.started.1",
            compaction_call::encode_started(&coordinates("summary")?),
        ),
        (
            "orphan-usage",
            "session.compaction.call.usage.1",
            observed("summary", 50, 30)?,
        ),
    ] {
        let conn = Fixture::new(label)?;
        schema(&conn)?;
        row(&conn, "ses_summary", 0, kind, &value)?;
        let snapshot = conn.read("ses_summary")?;
        let call = snapshot.attempts.first().ok_or("summary omitted")?;
        assert_eq!(call.operation, AccountingOperation::Compaction);
        assert_eq!(call.opening.is_some(), label == "no-usage");
        assert_eq!(call.usage.input_total().is_some(), label == "orphan-usage");
        assert_eq!(call.step.is_some(), label == "no-usage");
        conn.finish()?;
    }
    Ok(())
}

#[test]
fn corrupt_or_future_compaction_evidence_cannot_silently_disappear() -> TestResult {
    let valid = compaction_call::encode_started(&coordinates("summary")?);
    for (index, kind, raw) in [
        (0, "session.compaction.call.started.2", valid.to_string()),
        (1, "session.compaction.call.started", valid.to_string()),
        (2, "session.compaction.call.started.1", "{".into()),
        (
            3,
            "session.compaction.call.usage.1",
            r#"{"attempt":"summary"}"#.into(),
        ),
        (
            4,
            "session.compaction.call.ended.1",
            r#"{"call":null,"outcome":"completed"}"#.into(),
        ),
    ] {
        let conn = Fixture::new(&format!("summary-corrupt-{index}"))?;
        schema(&conn)?;
        super::raw(&conn, "ses_summary", 0, kind, &raw)?;
        assert!(conn
            .read("ses_summary")
            .err()
            .ok_or("corruption accepted")?
            .contains("compaction"));
        conn.finish()?;
    }
    Ok(())
}

#[test]
fn operation_identity_and_terminal_coordinates_cannot_conflict() -> TestResult {
    for mode in ["identity", "coordinates", "duplicate-end", "usage-conflict"] {
        let conn = Fixture::new(&format!("summary-{mode}"))?;
        schema(&conn)?;
        let coordinates = coordinates("summary")?;
        row(
            &conn,
            "ses_summary",
            0,
            "session.compaction.call.started.1",
            &compaction_call::encode_started(&coordinates),
        )?;
        match mode {
            "identity" => open(&conn, "ses_summary", 1, "summary", 1)?,
            "coordinates" => {
                let other = CallCoordinates {
                    compaction: grapt_types::CompactionId::new("different").ok_or("id")?,
                    ..coordinates
                };
                row(
                    &conn,
                    "ses_summary",
                    1,
                    "session.compaction.call.ended.1",
                    &compaction_call::encode_ended(&other, CallEnd::Failed),
                )?;
            }
            "duplicate-end" => {
                for seq in 1..=2 {
                    row(
                        &conn,
                        "ses_summary",
                        seq,
                        "session.compaction.call.ended.1",
                        &compaction_call::encode_ended(&coordinates, CallEnd::Cancelled),
                    )?;
                }
            }
            _ => {
                row(
                    &conn,
                    "ses_summary",
                    1,
                    "session.compaction.call.usage.1",
                    &observed("summary", 50, 30)?,
                )?;
                row(
                    &conn,
                    "ses_summary",
                    2,
                    "session.compaction.call.usage.1",
                    &observed("summary", 51, 30)?,
                )?;
            }
        }
        assert!(conn.read("ses_summary").is_err(), "{mode} was accepted");
        conn.finish()?;
    }
    Ok(())
}

#[test]
fn compaction_enumeration_and_observations_obey_existing_accounting_bounds() -> TestResult {
    for mode in ["attempts", "observations"] {
        let conn = Fixture::new(&format!("summary-bound-{mode}"))?;
        schema(&conn)?;
        let limit = if mode == "attempts" {
            MAX_ACCOUNTING_ATTEMPTS + 1
        } else {
            65
        };
        conn.execute_batch("BEGIN;")?;
        for index in 0..limit {
            let seq = i64::try_from(index)?;
            if mode == "attempts" {
                row(
                    &conn,
                    "ses_summary",
                    seq,
                    "session.compaction.call.started.1",
                    &compaction_call::encode_started(&coordinates(&format!("summary{index}"))?),
                )?;
            } else {
                row(
                    &conn,
                    "ses_summary",
                    seq,
                    "session.compaction.call.usage.1",
                    &observed("summary", 50, 30)?,
                )?;
            }
        }
        conn.execute_batch("COMMIT;")?;
        let reason = conn.read("ses_summary").err().ok_or("bound did not fire")?;
        assert!(
            reason.contains(if mode == "attempts" {
                "8192 attempts"
            } else {
                "64 usage observations"
            }),
            "{reason}"
        );
        conn.finish()?;
    }
    Ok(())
}
