//! R3b-1: one caller-handled page of durable seat mail.
//!
//! These are public-surface witnesses. They use the real governed store seam,
//! not a fake page source, and prove the runtime's only new ownership: handler
//! order and consume-after-success. Subscription, notification, Session inbox
//! admission, and seat-process liveness are deliberately absent.

use grapt_runtime::{drain_mailbox_page, MailboxDrainError};
use grapt_store::{
    EventStore, MailboxPageEntry, MAX_MAILBOX_PAGE_BYTES, MAX_MAILBOX_PAGE_MESSAGES,
};
use grapt_types::{
    ClockAuthority, MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor,
    ProtocolVersion, SeatMailboxMessage, SeatMessageKind, SeatRecipientId, Timestamp,
};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the test handler refused the message")]
struct HandlerRefused;

#[derive(Debug)]
struct FrozenClock(i64);

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

fn scratch(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-fc1-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Restores only the event tables to the accepted pre-target shape. Mailbox
/// state remains untouched so the production migration must carry it across
/// the rebuild before the runtime is allowed to consume it.
fn downgrade_event_tables(db: &std::path::Path) -> TestResult {
    let conn = Connection::open(db)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = OFF;
         BEGIN IMMEDIATE;
         ALTER TABLE event RENAME TO event_target;
         ALTER TABLE event_sequence RENAME TO event_sequence_target;
         DROP INDEX IF EXISTS event_aggregate_seq_idx;
         DROP INDEX IF EXISTS event_aggregate_type_seq_idx;
         DROP INDEX IF EXISTS event_campaign_transition_idx;
         CREATE TABLE event_sequence (
             aggregate_family TEXT NOT NULL,
             aggregate_id TEXT PRIMARY KEY,
             seq INTEGER NOT NULL,
             owner_id TEXT
         );
         CREATE TABLE event (
             id TEXT PRIMARY KEY,
             aggregate_family TEXT NOT NULL,
             aggregate_id TEXT NOT NULL,
             seq INTEGER NOT NULL,
             created INTEGER NOT NULL,
             type TEXT NOT NULL,
             data TEXT,
             writer_epoch INTEGER,
             envelope_bytes BLOB,
             campaign_transition_id TEXT,
             CONSTRAINT fk_event FOREIGN KEY (aggregate_id)
                 REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE
         );
         INSERT INTO event_sequence SELECT aggregate_family, aggregate_id, seq, owner_id
             FROM event_sequence_target;
         INSERT INTO event SELECT id, aggregate_family, aggregate_id, seq, created, type,
             data, writer_epoch, envelope_bytes, campaign_transition_id FROM event_target;
         DROP TABLE event_target;
         DROP TABLE event_sequence_target;
         CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);
         CREATE INDEX event_aggregate_type_seq_idx ON event (aggregate_id, type, seq);
         COMMIT;",
    )?;
    Ok(())
}

fn recipient(raw: &str) -> Result<SeatRecipientId, Box<dyn std::error::Error>> {
    SeatRecipientId::new(raw).ok_or_else(|| "recipient is empty".into())
}

fn message(
    ordinal: usize,
    to: &SeatRecipientId,
    supersedes: Option<MailboxMessageId>,
) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
    let id = format!("msg-{ordinal:03}");
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        MailboxMessageId::new(id.clone()).ok_or("message id")?,
        recipient("seat-alpha")?,
        to.clone(),
        SeatMessageKind::TaskPointer,
        MailboxBody::new(format!("body-{ordinal:03}"))?,
        supersedes,
        MailboxSourceCursor::new(format!("source-{id}")).ok_or("source cursor")?,
    )?)
}

fn admit(
    store: &mut EventStore,
    recipient: &SeatRecipientId,
    ordinal: usize,
) -> Result<(), Box<dyn std::error::Error>> {
    store.admit_mailbox_message(recipient, &message(ordinal, recipient, None)?)?;
    Ok(())
}

fn actionable_ids(
    store: &EventStore,
    recipient: &SeatRecipientId,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    Ok(store
        .read_actionable_mailbox(recipient, MailboxCursor::START, MAX_MAILBOX_PAGE_MESSAGES)?
        .entries
        .iter()
        .map(|entry| match entry {
            MailboxPageEntry::Message(message) => message.id().as_str().to_owned(),
            MailboxPageEntry::Unreadable(row) => row.message.clone(),
        })
        .collect())
}

#[test]
fn a_handler_failure_leaves_that_message_actionable() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    admit(&mut store, &to, 0)?;
    admit(&mut store, &to, 1)?;

    let mut seen = Vec::new();
    let mut handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        seen.push(message.id().as_str().to_owned());
        Err(HandlerRefused)
    };
    match drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut handler) {
        Err(MailboxDrainError::Handler {
            message,
            consumed_before_failure,
            source: HandlerRefused,
        }) => {
            assert_eq!(message.as_str(), "msg-000");
            assert_eq!(consumed_before_failure, 0);
        }
        other => panic!("handler refusal must stay typed and stop the page: {other:?}"),
    }
    assert_eq!(seen, ["msg-000"]);
    assert_eq!(
        actionable_ids(&store, &to)?,
        ["msg-000", "msg-001"],
        "a caller refusal must happen before the consumed marker"
    );
    Ok(())
}

#[test]
fn a_successful_handler_is_followed_by_consumption() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    admit(&mut store, &to, 0)?;

    let mut seen = Vec::new();
    let mut handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        seen.push(message.id().as_str().to_owned());
        Ok(())
    };
    let page = drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut handler)?;
    assert_eq!(seen, ["msg-000"]);
    assert_eq!(page.consumed_messages(), 1);
    assert!(
        actionable_ids(&store, &to)?.is_empty(),
        "handler success must be followed by the durable consumed marker"
    );
    Ok(())
}

#[test]
fn one_invocation_handles_one_page_in_order_and_returns_its_exclusive_cursor() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    for ordinal in 0..=MAX_MAILBOX_PAGE_MESSAGES {
        admit(&mut store, &to, ordinal)?;
    }

    let mut first_seen = Vec::new();
    let mut first_handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        first_seen.push(message.id().as_str().to_owned());
        Ok(())
    };
    let first = drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut first_handler)?;
    let expected: Vec<String> = (0..MAX_MAILBOX_PAGE_MESSAGES)
        .map(|ordinal| format!("msg-{ordinal:03}"))
        .collect();
    assert_eq!(
        first_seen, expected,
        "the caller handler must observe ascending admission order"
    );
    assert_eq!(first.consumed_messages(), MAX_MAILBOX_PAGE_MESSAGES);
    assert!(first.body_bytes() <= MAX_MAILBOX_PAGE_BYTES);
    let next_after = match first.next_after() {
        Some(cursor) => cursor,
        None => panic!("a full page must return its exclusive next_after cursor"),
    };
    assert_eq!(next_after.value(), MAX_MAILBOX_PAGE_MESSAGES as u64);

    let mut second_seen = Vec::new();
    let mut second_handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        second_seen.push(message.id().as_str().to_owned());
        Ok(())
    };
    let second = drain_mailbox_page(&mut store, &to, next_after, &mut second_handler)?;
    assert_eq!(second_seen, ["msg-064"]);
    assert_eq!(second.consumed_messages(), 1);
    assert!(second.next_after().is_none());
    Ok(())
}

#[test]
fn superseded_and_already_consumed_rows_do_not_reach_the_handler() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    let first = message(0, &to, None)?;
    store.admit_mailbox_message(&to, &first)?;
    let second = message(1, &to, Some(first.id().clone()))?;
    store.admit_mailbox_message(&to, &second)?;
    store.consume_mailbox_message(&to, second.id())?;

    let mut calls = 0usize;
    let mut handler = |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        calls = calls.saturating_add(1);
        Ok(())
    };
    let page = drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut handler)?;
    assert_eq!(calls, 0);
    assert_eq!(page.consumed_messages(), 0);
    assert!(page.next_after().is_none());
    Ok(())
}

/// FC-1 join-only witness: R2b-2 preserves an admitted mailbox row through the
/// production event-table migration, and R3b-1 must then carry that exact row
/// through its production handler and durable-consumption path. Neither parent
/// suite alone observes the complete property.
#[test]
fn a_migrated_mailbox_row_reaches_the_handler_and_is_durably_consumed() -> TestResult {
    let dir = scratch("migrated-mailbox-drain")?;
    let db = dir.join("scratch.db");
    let to = recipient("seat-zeta")?;

    {
        let mut store = EventStore::open(&db, "0.0.1")?;
        admit(&mut store, &to, 0)?;
    }

    downgrade_event_tables(&db)?;
    EventStore::migrate_with_clock(&db, &dir, "0.0.1", &FrozenClock(1_786_147_200_100))?;

    let mut store = EventStore::open(&db, "0.0.1")?;
    let mut seen = Vec::new();
    let mut handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        seen.push(message.id().as_str().to_owned());
        Ok(())
    };
    let page = drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut handler)?;
    assert_eq!(
        seen,
        ["msg-000"],
        "the migrated row must reach the handler once"
    );
    assert_eq!(page.consumed_messages(), 1);
    assert!(
        actionable_ids(&store, &to)?.is_empty(),
        "the runtime must durably consume the migrated row, not merely report success"
    );

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// A row the page cannot decode must not hold the drain at its cursor: it is
/// named in the page, consumed as its own named absence, and the surrounding
/// rows still reach the handler in order.
#[test]
fn an_unreadable_row_is_consumed_as_its_named_absence_and_the_drain_moves_on() -> TestResult {
    let dir = scratch("unreadable-mailbox-drain")?;
    let db = dir.join("scratch.db");
    let to = recipient("seat-zeta")?;

    {
        let mut store = EventStore::open(&db, "0.0.1")?;
        admit(&mut store, &to, 0)?;
        admit(&mut store, &to, 1)?;
        admit(&mut store, &to, 2)?;
    }
    {
        // A second connection is the honest hostile seam: the row's kind
        // column now disagrees with its bytes.
        let hostile = Connection::open(&db)?;
        hostile.execute(
            "UPDATE mailbox_message SET kind = 'done' WHERE message_id = ?1",
            ["msg-001"],
        )?;
    }

    let mut store = EventStore::open(&db, "0.0.1")?;
    let mut seen = Vec::new();
    let mut handler = |message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        seen.push(message.id().as_str().to_owned());
        Ok(())
    };
    let page = drain_mailbox_page(&mut store, &to, MailboxCursor::START, &mut handler)?;
    assert_eq!(
        seen,
        ["msg-000", "msg-002"],
        "the unreadable row never reaches the handler; its neighbours do"
    );
    assert_eq!(page.consumed_messages(), 2);
    assert_eq!(
        page.unreadable_rows(),
        1,
        "the drain counts the named absence it took"
    );
    assert!(
        page.next_after().is_none(),
        "the drain reached the end of the mailbox past the bad row"
    );
    assert!(
        actionable_ids(&store, &to)?.is_empty(),
        "the named absence is consumed durably; nothing holds the cursor"
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
