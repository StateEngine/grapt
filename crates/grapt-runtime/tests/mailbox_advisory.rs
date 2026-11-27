//! R3c1: private, body-free mailbox advisories derived from durable truth.

use grapt_runtime::{
    drain_mailbox_page_with_advisories, BoundedMailboxAdvisorySink, MailboxAdvisory,
    MailboxAdvisoryOffer, MailboxAdvisoryPhase, MailboxAdvisorySink, MailboxDrainError,
    MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES, MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES,
    MAX_RETAINED_MAILBOX_ADVISORIES, MAX_RETAINED_MAILBOX_ADVISORY_BYTES,
};
use grapt_store::{EventStore, MailboxPageEntry, MAX_MAILBOX_PAGE_MESSAGES};
use grapt_types::{
    MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor, ProtocolVersion,
    SeatMailboxMessage, SeatMessageKind, SeatRecipientId, MAX_MAILBOX_MESSAGE_ID_BYTES,
    MAX_SEAT_RECIPIENT_ID_BYTES,
};
use rusqlite::Connection;
use std::cell::RefCell;
use std::rc::Rc;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("intentional handler refusal")]
struct HandlerRefused;

fn recipient(raw: impl Into<String>) -> Result<SeatRecipientId, Box<dyn std::error::Error>> {
    Ok(SeatRecipientId::try_new(raw.into())?)
}

fn message_id(raw: impl Into<String>) -> Result<MailboxMessageId, Box<dyn std::error::Error>> {
    Ok(MailboxMessageId::try_new(raw.into())?)
}

fn message(
    id: MailboxMessageId,
    sender: SeatRecipientId,
    to: SeatRecipientId,
    kind: SeatMessageKind,
    body: &str,
) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        id,
        sender,
        to,
        kind,
        MailboxBody::new(body)?,
        None,
        MailboxSourceCursor::new("source-1").ok_or("source cursor")?,
    )?)
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

fn discovered<'a>(
    owner: &'a SeatRecipientId,
    id: &'a MailboxMessageId,
    sender: &'a SeatRecipientId,
) -> MailboxAdvisory<'a> {
    MailboxAdvisory::Discovered {
        recipient: owner,
        message: id,
        sender,
        kind: &SeatMessageKind::RulingRequest,
    }
}

#[derive(Clone)]
struct TraceSink(Rc<RefCell<Vec<&'static str>>>);

impl MailboxAdvisorySink for TraceSink {
    fn offer(&mut self, advisory: MailboxAdvisory<'_>) -> MailboxAdvisoryOffer {
        let label = match advisory {
            MailboxAdvisory::Discovered { .. } => "discovered",
            MailboxAdvisory::Handled { .. } => "handled",
        };
        self.0.borrow_mut().push(label);
        MailboxAdvisoryOffer::Retained
    }
}

#[test]
fn durable_read_precedes_discovered_and_commit_precedes_handled() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    let id = message_id("msg-order")?;
    store.admit_mailbox_message(
        &to,
        &message(
            id,
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::TaskPointer,
            "order-body",
        )?,
    )?;

    let trace = Rc::new(RefCell::new(Vec::new()));
    let mut sink = TraceSink(Rc::clone(&trace));
    let handler_trace = Rc::clone(&trace);
    let mut handler = move |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        handler_trace.borrow_mut().push("handler");
        Ok(())
    };
    let page = drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut handler,
        &mut sink,
    )?;
    assert_eq!(*trace.borrow(), ["discovered", "handler", "handled"]);
    assert_eq!(page.consumed_messages(), 1);
    assert!(actionable_ids(&store, &to)?.is_empty());
    Ok(())
}

#[test]
fn handler_failure_retains_discovered_without_handled_or_consumption() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    let id = message_id("msg-refused")?;
    store.admit_mailbox_message(
        &to,
        &message(
            id.clone(),
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::Question,
            "BODY-CANARY-MUST-NOT-ENTER-SEMADV1",
        )?,
    )?;
    let mut sink = BoundedMailboxAdvisorySink::new(to.clone());
    let mut handler =
        |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Err(HandlerRefused) };
    assert!(matches!(
        drain_mailbox_page_with_advisories(
            &mut store,
            &to,
            MailboxCursor::START,
            &mut handler,
            &mut sink,
        ),
        Err(MailboxDrainError::Handler { .. })
    ));
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Discovered)
    );
    assert_eq!(actionable_ids(&store, &to)?, ["msg-refused"]);
    let frames = sink.take_encoded();
    assert_eq!(frames.len(), 1);
    assert!(!frames[0]
        .windows(b"BODY-CANARY-MUST-NOT-ENTER-SEMADV1".len())
        .any(|window| window == b"BODY-CANARY-MUST-NOT-ENTER-SEMADV1"));
    assert!(frames[0]
        .windows(b"seat-alpha".len())
        .any(|window| window == b"seat-alpha"));
    assert!(frames[0]
        .windows(b"question".len())
        .any(|window| window == b"question"));
    Ok(())
}

#[test]
fn consume_failure_never_offers_handled() -> TestResult {
    let root =
        std::env::temp_dir().join(format!("grapt-r3c1-consume-failure-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(&root)?;
    let db = root.join("scratch.db");
    let to = recipient("seat-zeta")?;
    let id = message_id("msg-consume-fails")?;
    let mut store = EventStore::open(&db, "r3c1-test")?;
    store.admit_mailbox_message(
        &to,
        &message(
            id.clone(),
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::TaskPointer,
            "body",
        )?,
    )?;

    let db_for_handler = db.clone();
    let mut handler = move |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        let connection = Connection::open(&db_for_handler).map_err(|_| HandlerRefused)?;
        connection
            .execute_batch("DROP TABLE mailbox_message;")
            .map_err(|_| HandlerRefused)?;
        Ok(())
    };
    let mut sink = BoundedMailboxAdvisorySink::new(to.clone());
    assert!(matches!(
        drain_mailbox_page_with_advisories(
            &mut store,
            &to,
            MailboxCursor::START,
            &mut handler,
            &mut sink,
        ),
        Err(MailboxDrainError::Consume { .. })
    ));
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Discovered)
    );
    drop(store);
    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}

#[test]
fn stable_key_coalesces_and_advances_in_place_without_regression() -> TestResult {
    let owner = recipient("seat-zeta")?;
    let sender = recipient("seat-alpha")?;
    let id = message_id("msg-one")?;
    let mut sink = BoundedMailboxAdvisorySink::new(owner.clone());
    assert_eq!(
        sink.offer(discovered(&owner, &id, &sender)),
        MailboxAdvisoryOffer::Retained
    );
    let discovered_bytes = sink.retained_encoded_bytes();
    assert_eq!(
        sink.offer(discovered(&owner, &id, &sender)),
        MailboxAdvisoryOffer::Coalesced
    );
    assert_eq!(sink.retained_entries(), 1);
    assert_eq!(sink.retained_encoded_bytes(), discovered_bytes);
    assert_eq!(
        sink.offer(MailboxAdvisory::Handled {
            recipient: &owner,
            message: &id,
        }),
        MailboxAdvisoryOffer::Retained
    );
    assert_eq!(sink.retained_entries(), 1);
    assert!(sink.retained_encoded_bytes() < discovered_bytes);
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Handled)
    );
    assert_eq!(
        sink.offer(discovered(&owner, &id, &sender)),
        MailboxAdvisoryOffer::Coalesced
    );
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Handled)
    );
    Ok(())
}

#[test]
fn count_overflow_is_sticky_coalesced_and_does_not_block_consumption() -> TestResult {
    let owner = recipient("seat-zeta")?;
    let sender = recipient("seat-alpha")?;
    let mut sink = BoundedMailboxAdvisorySink::new(owner.clone());
    for ordinal in 0..MAX_RETAINED_MAILBOX_ADVISORIES {
        let id = message_id(format!("retained-{ordinal:02}"))?;
        assert_eq!(
            sink.offer(discovered(&owner, &id, &sender)),
            MailboxAdvisoryOffer::Retained
        );
    }
    assert_eq!(sink.retained_entries(), 64);

    let live_id = message_id("live-count-overflow")?;
    let mut store = EventStore::open_in_memory()?;
    store.admit_mailbox_message(
        &owner,
        &message(
            live_id,
            sender,
            owner.clone(),
            SeatMessageKind::Done,
            "must still consume",
        )?,
    )?;
    let mut handler = |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Ok(()) };
    let page = drain_mailbox_page_with_advisories(
        &mut store,
        &owner,
        MailboxCursor::START,
        &mut handler,
        &mut sink,
    )?;
    assert_eq!(page.consumed_messages(), 1);
    assert!(actionable_ids(&store, &owner)?.is_empty());
    assert_eq!(sink.retained_entries(), 64);
    assert!(sink.rescan_required());
    let frames = sink.take_encoded();
    assert_eq!(frames.len(), 65, "one sticky rescan frame is coalesced");
    assert!(!sink.rescan_required());
    Ok(())
}

#[test]
fn hostile_byte_overflow_reserves_one_max_rescan_and_does_not_block_consumption() -> TestResult {
    let owner = recipient("r".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
    let sender = recipient("s".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
    let mut sink = BoundedMailboxAdvisorySink::new(owner.clone());
    for ordinal in 0..40 {
        let suffix = format!("{ordinal:02}");
        let id = message_id(format!(
            "{}{}",
            "m".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES - suffix.len()),
            suffix
        ))?;
        assert_eq!(
            sink.offer(discovered(&owner, &id, &sender)),
            MailboxAdvisoryOffer::Retained
        );
    }
    assert_eq!(
        sink.retained_encoded_bytes(),
        40 * MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES
    );

    let live_id = message_id("z".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES))?;
    let mut store = EventStore::open_in_memory()?;
    store.admit_mailbox_message(
        &owner,
        &message(
            live_id,
            sender,
            owner.clone(),
            SeatMessageKind::RulingRequest,
            "must still consume",
        )?,
    )?;
    let mut handler = |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Ok(()) };
    let page = drain_mailbox_page_with_advisories(
        &mut store,
        &owner,
        MailboxCursor::START,
        &mut handler,
        &mut sink,
    )?;
    assert_eq!(page.consumed_messages(), 1);
    assert!(actionable_ids(&store, &owner)?.is_empty());
    assert_eq!(sink.retained_entries(), 40);
    assert!(sink.rescan_required());
    assert_eq!(
        sink.retained_encoded_bytes(),
        (40 * MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES) + MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES
    );
    assert!(sink.retained_encoded_bytes() <= MAX_RETAINED_MAILBOX_ADVISORY_BYTES);
    Ok(())
}

#[test]
fn byte_limit_reserves_rescan_before_admitting_a_new_key() -> TestResult {
    let owner = recipient("r".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
    let sender = recipient("s".repeat(MAX_SEAT_RECIPIENT_ID_BYTES))?;
    let mut sink = BoundedMailboxAdvisorySink::new(owner.clone());
    for ordinal in 0..40 {
        let suffix = format!("{ordinal:02}");
        let id = message_id(format!(
            "{}{}",
            "m".repeat(MAX_MAILBOX_MESSAGE_ID_BYTES - suffix.len()),
            suffix
        ))?;
        assert_eq!(
            sink.offer(discovered(&owner, &id, &sender)),
            MailboxAdvisoryOffer::Retained
        );
    }
    let short_sender = recipient("s")?;
    let short_id = message_id("n")?;
    assert_eq!(
        sink.offer(discovered(&owner, &short_id, &short_sender)),
        MailboxAdvisoryOffer::OverflowedBytes
    );
    assert_eq!(sink.retained_entries(), 40);
    assert!(sink.rescan_required());
    assert_eq!(
        sink.retained_encoded_bytes(),
        (40 * MAX_DISCOVERED_MAILBOX_ADVISORY_BYTES) + MAX_RESCAN_REQUIRED_MAILBOX_ADVISORY_BYTES
    );
    Ok(())
}

#[test]
fn a_recipient_sink_refuses_foreign_keys_loudly() -> TestResult {
    let owner = recipient("seat-zeta")?;
    let foreign = recipient("seat-beta")?;
    let sender = recipient("seat-alpha")?;
    let id = message_id("msg-foreign")?;
    let mut sink = BoundedMailboxAdvisorySink::new(owner);
    assert_eq!(
        sink.offer(discovered(&foreign, &id, &sender)),
        MailboxAdvisoryOffer::Fault(grapt_runtime::MailboxAdvisoryFault::RecipientMismatch)
    );
    assert_eq!(sink.retained_entries(), 0);
    assert!(sink.rescan_required());
    assert_eq!(
        sink.last_fault(),
        Some(grapt_runtime::MailboxAdvisoryFault::RecipientMismatch)
    );
    Ok(())
}

#[test]
fn reconnect_converges_by_durable_reread_without_advisory_replay() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    let id = message_id("msg-reconnect")?;
    store.admit_mailbox_message(
        &to,
        &message(
            id.clone(),
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::Handoff,
            "body",
        )?,
    )?;
    let mut sink = BoundedMailboxAdvisorySink::new(to.clone());
    let mut refuse =
        |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Err(HandlerRefused) };
    assert!(drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut refuse,
        &mut sink,
    )
    .is_err());
    assert_eq!(sink.retained_entries(), 1);

    let mut accept = |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Ok(()) };
    drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut accept,
        &mut sink,
    )?;
    assert_eq!(sink.retained_entries(), 1);
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Handled)
    );
    sink.disconnect();
    sink.reconnect();
    assert_eq!(sink.retained_entries(), 0);
    let empty = drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut accept,
        &mut sink,
    )?;
    assert_eq!(empty.consumed_messages(), 0);
    assert_eq!(
        sink.retained_entries(),
        0,
        "no private advisory replay exists"
    );
    Ok(())
}

#[test]
fn complete_live_state_loss_may_redisplay_an_actionable_durable_key() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    let id = message_id("msg-redisplay")?;
    store.admit_mailbox_message(
        &to,
        &message(
            id.clone(),
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::Blocker,
            "body",
        )?,
    )?;
    let mut sink = BoundedMailboxAdvisorySink::new(to.clone());
    let mut refuse =
        |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> { Err(HandlerRefused) };
    let _ = drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut refuse,
        &mut sink,
    );
    sink.disconnect();
    sink.reconnect();
    let _ = drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut refuse,
        &mut sink,
    );
    assert_eq!(sink.retained_entries(), 1);
    assert_eq!(
        sink.retained_phase(&id),
        Some(MailboxAdvisoryPhase::Discovered)
    );
    assert_eq!(actionable_ids(&store, &to)?, ["msg-redisplay"]);
    Ok(())
}

#[test]
fn disconnected_sink_loss_never_changes_handler_or_consumption() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let to = recipient("seat-zeta")?;
    store.admit_mailbox_message(
        &to,
        &message(
            message_id("msg-disconnected")?,
            recipient("seat-alpha")?,
            to.clone(),
            SeatMessageKind::Done,
            "body",
        )?,
    )?;
    let mut sink = BoundedMailboxAdvisorySink::new(to.clone());
    sink.disconnect();
    let mut calls = 0usize;
    let mut handler = |_message: &SeatMailboxMessage| -> Result<(), HandlerRefused> {
        calls += 1;
        Ok(())
    };
    let page = drain_mailbox_page_with_advisories(
        &mut store,
        &to,
        MailboxCursor::START,
        &mut handler,
        &mut sink,
    )?;
    assert_eq!(calls, 1);
    assert_eq!(page.consumed_messages(), 1);
    assert!(actionable_ids(&store, &to)?.is_empty());
    assert_eq!(sink.retained_entries(), 0);
    Ok(())
}
