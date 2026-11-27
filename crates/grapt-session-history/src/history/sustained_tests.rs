use super::*;

#[test]
fn omission_ranges_refuse_negative_coordinates_instead_of_reporting_zero() {
    let conn = stored(&[(-3, "session.usage.observed.1", "{}")]);
    assert!(
        matches!(
            SessionHistoryStore::new(&conn).omissions_between(&session(), None, 10),
            Err(HistoryReadError::Corrupt { seq: -3, .. })
        ),
        "a structural sequence contradiction was reported as an omission at zero"
    );
}

/// A gap scan names a row whose payload is valid JSON but violates its own
/// contract.
///
/// The fold already degrades such a row to a recorded gap and answers `Ok`,
/// so a probe that only looked at an escaping error classified EVERY one of
/// them as readable and the row vanished from the page's marker. Driven through
/// the real scan, because the defect lived in how that scan asks the fold.
#[test]
fn a_gap_scan_names_a_valid_json_payload_that_violates_its_contract() {
    // `content.appended.1` with an object carrying no message, item or capture
    // disposition: JSON the fold's decoder must refuse.
    let conn = stored(&[(0, "session.content.appended.1", "{}")]);
    let omissions = match SessionHistoryStore::new(&conn).omissions_between(&session(), None, 10) {
        Ok(omissions) => omissions,
        Err(error) => panic!("one unreadable row must not refuse the span scan: {error}"),
    };
    let [omission] = omissions.as_slice() else {
        panic!("expected exactly one named gap, got {omissions:?}");
    };
    assert_eq!(omission.seq, 0);
    assert_eq!(omission.kind, "session.content.appended");
    assert_eq!(omission.class, HistoryOmissionClass::Undecodable);
}

#[test]
fn swr_c4_attempt_usage_read_is_scoped_and_independent_of_session_lifetime() {
    let conn = stored(&[]);
    for index in 0..5_001usize {
        let seq = match i64::try_from(index) {
            Ok(value) => value,
            Err(_) => panic!("bounded fixture sequence fits i64"),
        };
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 0, 'execution.observed.1', '{}')",
            rusqlite::params![format!("evt_unrelated_{index}"), seq],
        ) {
            panic!("unrelated row must insert: {error}");
        }
    }
    let usage = |at: &str, input: u64| {
        sm::encode_observed(&UsageObservedFact::new(
            attempt(at),
            AttemptUsage::observed(
                reported(input),
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageField::ABSENT,
                UsageObservationPhase::Terminal,
            ),
        ))
        .to_string()
    };
    for (index, (at, input)) in [("pat_scoped", 41), ("pat_foreign", 99)]
        .into_iter()
        .enumerate()
    {
        let offset = match i64::try_from(index) {
            Ok(value) => value,
            Err(_) => panic!("bounded fixture offset fits i64"),
        };
        let seq = 6_000i64 + offset;
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 0, 'session.usage.observed.2', ?3)",
            rusqlite::params![format!("evt_usage_{index}"), seq, usage(at, input)],
        ) {
            panic!("usage row must insert: {error}");
        }
    }
    for index in 0..64usize {
        let offset = match i64::try_from(index) {
            Ok(value) => value,
            Err(_) => panic!("bounded foreign usage offset fits i64"),
        };
        let at = format!("pat_foreign_{index}");
        if let Err(error) = conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data)
             VALUES (?1, 'session', 'ses_df0a', ?2, 0, 'session.usage.observed.2', ?3)",
            rusqlite::params![
                format!("evt_foreign_usage_{index}"),
                6_002i64 + offset,
                usage(&at, 99)
            ],
        ) {
            panic!("foreign usage row must insert: {error}");
        }
    }
    let (observed, gaps) = match SessionHistoryStore::new(&conn)
        .usage_for_attempt(&session(), &attempt("pat_scoped"))
    {
        Ok(observed) => observed,
        Err(error) => panic!("attempt-scoped usage must remain readable: {error}"),
    };
    assert!(gaps.is_empty(), "a clean read named a gap: {gaps:?}");
    assert_eq!(observed.len(), 1);
    assert_eq!(
        observed[0]
            .input_total_tokens()
            .and_then(grapt_types::UsageField::value),
        Some(41)
    );
}
