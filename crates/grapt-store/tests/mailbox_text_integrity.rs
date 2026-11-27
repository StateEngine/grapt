//! Corrupt indexed text must not become valid metadata through replacement.
use grapt_store::EventStore;
use grapt_types::{
    MailboxBody, MailboxMessageId, MailboxSourceCursor, ProtocolVersion, SeatMailboxMessage,
    SeatMessageKind, SeatRecipientId,
};
use rusqlite::Connection;
use std::path::PathBuf;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn message(id: &str, target: Option<&str>) -> Result<SeatMailboxMessage> {
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        MailboxMessageId::new(id).ok_or("message")?,
        SeatRecipientId::new("seat-sender").ok_or("sender")?,
        SeatRecipientId::new("seat-reader").ok_or("recipient")?,
        SeatMessageKind::TaskPointer,
        MailboxBody::new("mail")?,
        target
            .map(|raw| MailboxMessageId::new(raw).ok_or("target"))
            .transpose()?,
        MailboxSourceCursor::new(id).ok_or("source")?,
    )?)
}

#[test]
fn invalid_text_cannot_authorize_normal_consumption() -> Result {
    let root = std::env::temp_dir().join(format!("mailbox-text-integrity-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let scratch = Scratch(root);
    let path = scratch.0.join("mailbox.sqlite");
    let recipient = SeatRecipientId::new("seat-reader").ok_or("recipient")?;
    let target = message("msg-\u{fffd}", None)?;
    let replacement = message("msg-replacement", Some(target.id().as_str()))?;
    {
        let mut store = EventStore::open(&path, "mailbox-test")?;
        store.admit_mailbox_message(&recipient, &target)?;
        store.admit_mailbox_message(&recipient, &replacement)?;
    }
    {
        let outside = Connection::open(&path)?;
        outside.execute(
            "UPDATE mailbox_message SET supersedes = CAST(X'6d73672dff' AS TEXT)
             WHERE message_id = 'msg-replacement'",
            [],
        )?;
    }
    let mut store = EventStore::open(&path, "mailbox-test")?;
    let result = store.consume_mailbox_message(&recipient, replacement.id());
    assert!(
        result.is_err(),
        "corrupt indexed text authorized consumption: {result:?}"
    );
    let outside = Connection::open(&path)?;
    let consumed: i64 = outside.query_row(
        "SELECT consumed FROM mailbox_message WHERE message_id = 'msg-replacement'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        consumed, 0,
        "refused consumption must leave the message pending"
    );
    Ok(())
}
