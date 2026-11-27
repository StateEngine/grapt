//! A malformed stored row must leave the rest of the local mailbox readable.
use grapt_store::{EventStore, MAX_MAILBOX_PAGE_MESSAGES};
use grapt_types::{
    MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor, ProtocolVersion,
    SeatMailboxMessage, SeatMessageKind, SeatRecipientId,
};
use rusqlite::{params, types::Value, Connection};
use std::path::PathBuf;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn readable_neighbours(case: &str, column: &str, value: Value) -> Result {
    let root = std::env::temp_dir().join(format!(
        "mailbox-row-progress-{case}-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let scratch = Scratch(root);
    let path = scratch.0.join("mailbox.sqlite");
    let recipient = SeatRecipientId::new("seat-reader").ok_or("recipient")?;
    {
        let mut store = EventStore::open(&path, "mailbox-test")?;
        for (id, body) in [
            ("msg-before", "one"),
            ("msg-bad", "bad"),
            ("msg-after", "three"),
        ] {
            let message = SeatMailboxMessage::new(
                ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
                MailboxMessageId::new(id).ok_or("message")?,
                SeatRecipientId::new("seat-sender").ok_or("sender")?,
                recipient.clone(),
                SeatMessageKind::TaskPointer,
                MailboxBody::new(body)?,
                None,
                MailboxSourceCursor::new(id).ok_or("source")?,
            )?;
            store.admit_mailbox_message(&recipient, &message)?;
        }
    }
    {
        let outside = Connection::open(&path)?;
        outside.execute(
            &format!("UPDATE mailbox_message SET {column} = ?1 WHERE message_id = ?2"),
            params![value, "msg-bad"],
        )?;
    }
    let store = EventStore::open(&path, "mailbox-test")?;
    let page =
        store.read_actionable_mailbox(&recipient, MailboxCursor::START, MAX_MAILBOX_PAGE_MESSAGES);
    assert!(
        page.is_ok(),
        "one {case} row blocked its readable neighbours: {page:?}"
    );
    let page = page?;
    assert_eq!(page.body_bytes, "one".len() + "three".len());
    assert!(
        page.next_after.is_none(),
        "the readable tail must be reached"
    );
    Ok(())
}

#[test]
fn a_disagreeing_kind_leaves_its_neighbours_readable() -> Result {
    readable_neighbours("kind-mismatch", "kind", Value::Text("done".into()))
}

#[test]
fn a_text_record_leaves_its_neighbours_readable() -> Result {
    readable_neighbours(
        "text-record",
        "record_bytes",
        Value::Text("unreadable".into()),
    )
}

#[test]
fn a_blob_kind_leaves_its_neighbours_readable() -> Result {
    readable_neighbours("blob-kind", "kind", Value::Blob(vec![0xff]))
}

#[test]
fn an_unreadable_superseder_does_not_hide_a_readable_neighbour() -> Result {
    readable_neighbours(
        "false-superseder",
        "supersedes",
        Value::Text("msg-before".into()),
    )
}
