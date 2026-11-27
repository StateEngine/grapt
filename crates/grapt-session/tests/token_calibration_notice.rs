//! A token calibration survives reopening the store and is never history.
use grapt_session::notice::{self, CalibrationTarget, TokenCalibration};
use grapt_session::{create_session, SessionSeed};
use grapt_store::EventStore;
use grapt_types::{ClockAuthority, DeterministicAuthorities, IdAuthority, IdToken, Timestamp};
use std::cell::Cell;

#[derive(Debug, Default)]
struct Authorities(Cell<u64>);
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        self.0.set(self.0.get() + 1);
        IdToken::new(format!("calibration{}", self.0.get()))
            .unwrap_or_else(|| panic!("fixture ID refused"))
    }
}

#[test]
fn the_newest_calibration_reads_back_after_reopening_and_stays_out_of_history(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!(
        "grapt-token-calibration-{}.sqlite",
        std::process::id()
    ));
    assert!(!path.exists(), "refusing to overwrite existing data");
    let source = Authorities::default();
    let authorities = DeterministicAuthorities::new(&source, &source);
    let mut store = EventStore::open(&path, "token-calibration")?;
    let seed = SessionSeed::new(
        grapt_types::ProjectId::new("prj_calibration").ok_or("project ID refused")?,
        "/work/calibration",
        "calibration",
    )?;
    let session = create_session(&mut store, &authorities, &seed, &mut [])?.session;
    assert_eq!(
        notice::latest_token_calibration(store.connection(), &session)?,
        None
    );
    let target = CalibrationTarget::new("a".repeat(64)).ok_or("target refused")?;
    let calibration = |confirmed| TokenCalibration {
        target: target.clone(),
        confirmed,
        estimated: 4_000,
    };
    assert!(
        notice::record_token_calibration(&mut store, &authorities, &session, &calibration(0))
            .is_err(),
        "a zero count is refused, not recorded"
    );
    notice::record_token_calibration(&mut store, &authorities, &session, &calibration(1_000))?;
    notice::record_token_calibration(&mut store, &authorities, &session, &calibration(1_200))?;
    drop(store);

    let store = EventStore::open(&path, "token-calibration")?;
    let read = notice::latest_token_calibration(store.connection(), &session)?;
    assert_eq!(read, Some(calibration(1_200)));
    assert!(notice::skipped_kinds(store.connection(), &session)?.is_empty());
    drop(store);
    std::fs::remove_file(&path)?;
    Ok(())
}

/// Whether public history would select a notice row holding `data`.
fn public(data: &str) -> Result<bool, Box<dyn std::error::Error>> {
    let connection = rusqlite::Connection::open_in_memory()?;
    let sql = format!(
        "SELECT {} FROM (SELECT ?1 AS data)",
        notice::public_notice_predicate("data")
    );
    Ok(connection.query_row(&sql, [data], |row| row.get::<_, bool>(0))?)
}

#[test]
fn only_a_parsed_top_level_kind_marks_a_notice_as_calibration(
) -> Result<(), Box<dyn std::error::Error>> {
    // Spacing is JSON, and size does not make accounting history.
    let padding = "x".repeat(notice::MAX_NOTICE_DATA_BYTES);
    let spaced = format!(
        r#"{{ "sessionID" : "ses_a", "kind" : "token-calibration", "padding" : "{padding}" }}"#
    );
    assert!(
        !public(&spaced)?,
        "a spaced calibration row reached history"
    );
    // A malformed public notice that mentions the name stays on the strict
    // path every malformed notice takes, rather than vanishing.
    let mentions = r#"{"kind":"context-trimmed","detail":{"kind":"token-calibration"}"#;
    assert!(public(mentions)?, "a malformed public notice was hidden");
    let nested = r#"{"kind":"context-trimmed","detail":{"kind":"token-calibration"}}"#;
    assert!(public(nested)?, "a nested mention hid a public notice");
    Ok(())
}
