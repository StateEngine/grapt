//! History pages preserve exact admission coordinates without per-row scans.
use grapt_inbox::{AdmissionCoordinates, AdmitOutcome, AdmitRequest, Inbox, InboxError};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{DeliveryMode, EventId, MessageId, SessionId};
use rusqlite::Connection;
use std::sync::atomic::{AtomicU64, Ordering};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
struct Scratch(std::path::PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}
struct Fixture {
    inbox: Inbox,
    raw: Connection,
    inputs: Vec<MessageId>,
    expected: Vec<AdmissionCoordinates>,
    // Drop after both SQLite connections, including on Windows.
    _root: Scratch,
}
impl Fixture {
    fn new(label: &str) -> Result<Self> {
        Self::with_count(label, 3)
    }
    fn with_count(label: &str, count: i64) -> Result<Self> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "grapt-coordinate-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root)?;
        let db = root.join("inbox.db");
        let mut store = EventStore::open(&db, "coordinates")?;
        let session: SessionId = "ses_coordinates".parse()?;
        store.elect_writer(&AggregateId::Session(session.clone()))?;
        store.elect_writer(&AggregateId::Session("ses_other".parse()?))?;
        let mut inbox = Inbox::new(store)?;
        let mut inputs = Vec::new();
        let mut expected = Vec::new();
        for index in 0..count {
            let event: EventId = format!("evt_coordinates{index}").parse()?;
            let input: MessageId = format!("msg_coordinates{index}").parse()?;
            let created_ms = 100 + index;
            let outcome = inbox.admit(AdmitRequest {
                event_id: &event,
                session: &session,
                input: &input,
                text: "history input",
                delivery: DeliveryMode::Queue,
                resume: false,
                created_ms,
            })?;
            let AdmitOutcome::Admitted { seq, .. } = outcome else {
                return Err("fixture did not admit its input".into());
            };
            inputs.push(input);
            expected.push(AdmissionCoordinates { seq, created_ms });
        }
        inbox.admit(AdmitRequest {
            event_id: &"evt_coordinate_other".parse()?,
            session: &"ses_other".parse()?,
            input: &"msg_coordinate_other".parse()?,
            text: "other Session input",
            delivery: DeliveryMode::Queue,
            resume: false,
            created_ms: 200,
        })?;
        Ok(Self {
            _root: Scratch(root),
            raw: Connection::open(db)?,
            inbox,
            inputs,
            expected,
        })
    }
    fn refuses(&self, inputs: &[MessageId]) {
        assert!(matches!(
            self.inbox.admission_coordinate_page(inputs),
            Err(InboxError::Corruption { .. })
        ));
    }
}

#[test]
fn coordinates_follow_caller_order_and_keep_repeated_identities_and_bounds() -> Result {
    let f = Fixture::new("order")?;
    let inputs = [
        f.inputs[2].clone(),
        f.inputs[0].clone(),
        f.inputs[2].clone(),
    ];
    assert_eq!(
        f.inbox.admission_coordinate_page(&inputs)?,
        vec![f.expected[2], f.expected[0], f.expected[2]]
    );
    assert_eq!(f.inbox.admission_coordinates(&f.inputs[1])?, f.expected[1]);
    assert!(f.inbox.admission_coordinate_page(&[])?.is_empty());
    assert_eq!(
        f.inbox
            .admission_coordinate_page(&vec![f.inputs[0].clone(); 200])?,
        vec![f.expected[0]; 200]
    );
    assert!(matches!(
        f.inbox
            .admission_coordinate_page(&vec![f.inputs[0].clone(); 201]),
        Err(InboxError::CoordinatePageTooLong { limit: 200 })
    ));
    Ok(())
}

#[test]
fn missing_or_duplicate_admission_in_another_session_refuses_the_whole_page() -> Result {
    let f = Fixture::new("identity")?;
    let mut inputs = f.inputs.clone();
    inputs.push("msg_absent".parse()?);
    f.refuses(&inputs);
    f.raw.execute_batch("INSERT INTO event
        (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
        SELECT 'evt_duplicate', aggregate_family, 'ses_other', seq, created, type, data, writer_epoch
        FROM event WHERE id='evt_coordinates1'")?;
    f.refuses(&f.inputs);
    assert!(matches!(
        f.inbox.admission_coordinates(&f.inputs[1]),
        Err(InboxError::Corruption { .. })
    ));
    Ok(())
}

#[test]
fn malformed_json_outside_the_requested_identity_and_session_still_refuses() -> Result {
    let f = Fixture::new("json")?;
    f.raw.execute_batch(
        "UPDATE event SET data='{broken', aggregate_id='ses_other'
        WHERE id='evt_coordinates2'",
    )?;
    f.refuses(&f.inputs[..1]);
    assert!(matches!(
        f.inbox.admission_coordinates(&f.inputs[0]),
        Err(InboxError::Corruption { .. })
    ));
    Ok(())
}

#[test]
fn duplicate_json_identity_keys_cannot_reassign_coordinates() -> Result {
    let f = Fixture::new("duplicate-key")?;
    f.raw.execute(
        "UPDATE event SET data=?1 WHERE id='evt_coordinates0'",
        [r#"{"inputID":"msg_coordinates0","inputID":"msg_coordinates1"}"#],
    )?;
    f.refuses(&f.inputs);
    f.refuses(&f.inputs[..1]);
    Ok(())
}

#[test]
fn negative_sequences_refuse_instead_of_becoming_zero() -> Result {
    let f = Fixture::new("negative")?;
    f.raw.execute_batch(
        "PRAGMA ignore_check_constraints=ON;
        UPDATE event SET seq=-1 WHERE id='evt_coordinates1'",
    )?;
    f.refuses(&f.inputs);
    Ok(())
}

#[test]
fn full_page_reads_two_hundred_distinct_admissions() -> Result {
    let f = Fixture::with_count("full-page", 200)?;
    assert_eq!(f.inbox.admission_coordinate_page(&f.inputs)?, f.expected);
    Ok(())
}

#[test]
fn acceptance_reads_immutable_admission_before_and_after_promotion() -> Result {
    let mut f = Fixture::new("acceptance")?;
    let session: SessionId = "ses_coordinates".parse()?;
    let input = &f.inputs[0];
    assert_eq!(f.expected[0].seq, 0, "first admission is zero-based");
    let receipt = f
        .inbox
        .input_acceptance(&session, input)?
        .ok_or("acceptance absent")?;
    assert_eq!(receipt.session, session);
    assert_eq!(&receipt.input, input);
    assert_eq!(receipt.seq, f.expected[0].seq);
    assert_eq!(receipt.created_ms, f.expected[0].created_ms);
    assert_eq!(receipt.kind, grapt_inbox::PromotedInputKind::User);
    assert_eq!(receipt.text, "history input");
    assert_eq!(receipt.delivery, DeliveryMode::Queue);
    assert_eq!(receipt.metadata, None);
    assert!(f
        .inbox
        .input_acceptance(&session, &"msg_unknown".parse()?)?
        .is_none());
    assert!(matches!(
        f.inbox.input_acceptance(&"ses_other".parse()?, input),
        Err(InboxError::Conflict {
            differs: "session",
            ..
        })
    ));
    let promoted: EventId = "evt_acceptance_promoted".parse()?;
    f.inbox.promote(
        &session,
        grapt_inbox::PromoteScope::InputAtIdle,
        &mut || promoted.clone(),
        300,
    )?;
    assert_eq!(f.inbox.input_acceptance(&session, input)?, Some(receipt));
    for malformed in [
        "{broken",
        r#"{"inputID":"msg_other","inputID":"msg_coordinates0","sessionID":"ses_coordinates"}"#,
        r#"{"inputID":"msg_other","sessionID":"ses_other","sessionID":"ses_coordinates"}"#,
        r#"{"sessionID":"ses_coordinates"}"#,
        r#"{"inputID":3,"sessionID":"ses_coordinates"}"#,
        r#"{"inputID":"msg_other"}"#,
        r#"{"inputID":"msg_other","sessionID":null}"#,
    ] {
        f.raw.execute(
            "UPDATE event SET data=?1 WHERE id='evt_coordinates0'",
            [malformed],
        )?;
        for requested in [input.clone(), "msg_unknown".parse()?] {
            assert!(matches!(
                f.inbox.input_acceptance(&session, &requested),
                Err(InboxError::Corruption { .. })
            ));
        }
    }
    Ok(())
}
