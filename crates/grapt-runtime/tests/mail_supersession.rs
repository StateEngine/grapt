//! A correction names the instruction already handed to the model.
use grapt_provider::CodexInputItem;
use grapt_runtime::mailbox::presentation::{
    append_mail_page, MailPresentationRow, MAX_MAIL_NOTIFICATION_BYTES,
};
use grapt_store::{EventStore, MailboxPageEntry};
use grapt_types::{
    MailboxAdmissionSeq, MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor,
    ProtocolVersion, SeatMailboxMessage, SeatMessageKind, SeatRecipientId, Timestamp,
};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn mail(id: &str, supersedes: Option<&str>, metadata: &str) -> TestResult<SeatMailboxMessage> {
    let sender = if metadata == "recipient" {
        "sender".to_owned()
    } else {
        "\u{3}".repeat(256)
    };
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        MailboxMessageId::try_new(id)?,
        SeatRecipientId::try_new(sender)?,
        SeatRecipientId::try_new(metadata)?,
        SeatMessageKind::Handoff,
        MailboxBody::new(metadata)?,
        supersedes.map(MailboxMessageId::try_new).transpose()?,
        MailboxSourceCursor::new(format!("source-{id}")).ok_or("source")?,
    )?)
}

fn append(
    input: &mut Vec<CodexInputItem>,
    message: &SeatMailboxMessage,
    sequence: MailboxAdmissionSeq,
    after: MailboxCursor,
) -> TestResult<MailboxCursor> {
    Ok(append_mail_page(
        input,
        message.recipient(),
        after,
        &[MailPresentationRow {
            sequence,
            source_created: Timestamp::from_millis(1),
            message,
        }],
    )?
    .next_after)
}

fn text(item: &CodexInputItem) -> TestResult<&str> {
    let CodexInputItem::Message(message) = item else {
        return Err("notification did not reach model input".into());
    };
    Ok(&message.content)
}

#[test]
fn later_local_correction_withdraws_the_instruction_already_in_model_input() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let first = mail("instruction-a", None, "recipient")?;
    let second = mail("instruction-b", Some("instruction-a"), "recipient")?;
    let first_seq = store
        .admit_mailbox_message(first.recipient(), &first)?
        .seq();
    let mut input = Vec::new();
    let after = append(&mut input, &first, first_seq, MailboxCursor::START)?;
    let original = input.clone();
    assert!(!text(&input[0])?.contains("withdrawn"));
    store.consume_mailbox_message(first.recipient(), first.id())?;
    let second_seq = store
        .admit_mailbox_message(second.recipient(), &second)?
        .seq();
    let page = store.read_actionable_mailbox(second.recipient(), after, 64)?;
    assert_eq!(page.entries.len(), 1);
    let MailboxPageEntry::Message(received) = &page.entries[0] else {
        return Err("correction unreadable".into());
    };
    append(&mut input, received, second_seq, after)?;
    assert_eq!(&input[..1], original.as_slice());
    assert!(text(&input[1])?.contains("instruction-a") && text(&input[1])?.contains("withdrawn"));
    Ok(())
}

#[test]
fn local_correction_requires_an_admitted_target_and_withdraws_only_that_row() -> TestResult {
    for reversed in [false, true] {
        let mut store = EventStore::open_in_memory()?;
        let a = mail("instruction-a", None, "recipient")?;
        let b = mail("instruction-b", Some("instruction-a"), "recipient")?;
        if reversed {
            assert!(matches!(
                store.admit_mailbox_message(b.recipient(), &b),
                Err(grapt_store::WriteError::MailboxSupersedesMissing { .. })
            ));
        }
        let mut sequence = None;
        for message in [&a, &b] {
            let admitted = store
                .admit_mailbox_message(message.recipient(), message)?
                .seq();
            if message.id() == b.id() {
                sequence = Some(admitted);
            }
        }
        let page = store.read_actionable_mailbox(b.recipient(), MailboxCursor::START, 64)?;
        assert_eq!(page.entries.len(), 1);
        let MailboxPageEntry::Message(received) = &page.entries[0] else {
            return Err("correction unreadable".into());
        };
        assert_eq!(received.id(), b.id());
        let mut input = Vec::new();
        append(
            &mut input,
            received,
            sequence.ok_or("admission")?,
            MailboxCursor::START,
        )?;
        assert!(
            text(&input[0])?.contains("instruction-a") && text(&input[0])?.contains("withdrawn")
        );
    }
    Ok(())
}

#[test]
fn maximum_escaped_local_supersession_reaches_the_bounded_model_input() -> TestResult {
    let message = mail(
        &"\u{1}".repeat(256),
        Some(&"\u{2}".repeat(256)),
        &"\u{1}".repeat(256),
    )?;
    let mut input = Vec::new();
    append(
        &mut input,
        &message,
        MailboxAdmissionSeq::new(1).ok_or("sequence")?,
        MailboxCursor::START,
    )?;
    let content = text(&input[0])?;
    assert!(content.len() <= MAX_MAIL_NOTIFICATION_BYTES);
    assert!(content.contains(&format!("{:?}", "\u{2}".repeat(256))));
    assert!(content.contains("withdrawn"));
    Ok(())
}
