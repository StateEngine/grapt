use super::*;

#[test]
fn a_producer_day_spans_pages_without_truncating_or_recounting_stream_usage() -> TestResult {
    let conn = Fixture::new("producer-day")?;
    schema(&conn)?;
    let field = |value| TokenCount::new(value).map(UsageField::reported);
    let during = AttemptUsage::observed(
        field(100)?,
        field(10)?,
        field(5)?,
        UsageField::ABSENT,
        field(90)?,
        field(0)?,
        field(105)?,
        UsageObservationPhase::DuringStream,
    );
    let mut terminal = during.clone();
    terminal.phase = UsageObservationPhase::Terminal;
    conn.execute_batch("BEGIN;")?;
    for index in 0..2048_u32 {
        let attempt = PhysicalAttemptId::new(format!("pat_day_{index}")).ok_or("attempt")?;
        let opened = AssistantMessageOpenedFact::new(
            attempt.clone(),
            StepNumber::new(index + 1).ok_or("step")?,
            format!("msg_day_{index}").parse()?,
        );
        let seq = i64::from(index) * 3;
        row(
            &conn,
            "ses_day",
            seq,
            "session.assistant-message.opened.1",
            &session_message::encode_opened(&opened),
        )?;
        for (offset, usage) in [(1, during.clone()), (2, terminal.clone())] {
            row(
                &conn,
                "ses_day",
                seq + offset,
                "session.usage.observed.2",
                &session_message::encode_observed(&UsageObservedFact::new(attempt.clone(), usage)),
            )?;
        }
    }
    conn.execute_batch("COMMIT;")?;
    let snapshot = conn.read("ses_day")?;
    assert!(snapshot.enumeration_complete, "producer day was truncated");
    assert_eq!(snapshot.events_read, 6144);
    assert_eq!(snapshot.attempts.len(), 2048);
    let input = snapshot.attempts.iter().try_fold(0_u64, |total, call| {
        total
            .checked_add(call.usage.input_total().ok_or("input")?.count().value())
            .ok_or("sum overflow")
    })?;
    let read = snapshot.attempts.iter().try_fold(0_u64, |total, call| {
        total
            .checked_add(call.usage.cache_read().ok_or("read")?.count().value())
            .ok_or("sum overflow")
    })?;
    assert_eq!(input, 204_800, "stream and terminal usage were added");
    assert_eq!(read, 184_320);
    conn.finish()
}
