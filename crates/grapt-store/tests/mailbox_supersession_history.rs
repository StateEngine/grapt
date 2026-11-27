//! Consumed supersession history must not prevent reading new mail.
use grapt_store::EventStore;
use grapt_types::{
    MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor, ProtocolVersion,
    SeatMailboxMessage, SeatMessageKind, SeatRecipientId,
};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

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
fn consumed_supersession_history_keeps_new_mail_readable() -> Result {
    let mut store = EventStore::open_in_memory()?;
    let recipient = SeatRecipientId::new("seat-reader").ok_or("recipient")?;
    for index in 0..1_001 {
        let target = message(&format!("msg-target-{index}"), None)?;
        let replacement = message(
            &format!("msg-replacement-{index}"),
            Some(target.id().as_str()),
        )?;
        store.admit_mailbox_message(&recipient, &target)?;
        store.admit_mailbox_message(&recipient, &replacement)?;
        store.consume_mailbox_message(&recipient, target.id())?;
        store.consume_mailbox_message(&recipient, replacement.id())?;
    }
    let live = message("msg-live", None)?;
    store.admit_mailbox_message(&recipient, &live)?;
    let page = store.read_actionable_mailbox(&recipient, MailboxCursor::START, 1);
    assert!(page.is_ok(), "consumed history blocked new mail: {page:?}");
    let page = page?;
    assert_eq!(page.body_bytes, live.body().as_str().len());
    assert!(page.next_after.is_none(), "only the new message remains");
    Ok(())
}
