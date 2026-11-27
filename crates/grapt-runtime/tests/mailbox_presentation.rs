use grapt_provider::{CodexInputItem, CodexToolText, InputMessage, MessageRole};
use grapt_runtime::mailbox::presentation::*;
use grapt_types::{
    MailboxAdmissionSeq, MailboxBody, MailboxCursor, MailboxMessageId, MailboxSourceCursor,
    ProtocolVersion, SeatMailboxMessage, SeatMessageKind, SeatRecipientId, Timestamp, ToolCallId,
};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn message(id: &str, body: &str) -> Result<SeatMailboxMessage, Box<dyn std::error::Error>> {
    Ok(SeatMailboxMessage::new(
        ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
        MailboxMessageId::new(id).ok_or("id")?,
        SeatRecipientId::new("pilot").ok_or("sender")?,
        SeatRecipientId::new("coder").ok_or("recipient")?,
        SeatMessageKind::Handoff,
        MailboxBody::new(body)?,
        None,
        MailboxSourceCursor::new(format!("source-{id}")).ok_or("source")?,
    )?)
}

fn row(sequence: u64, message: &SeatMailboxMessage) -> MailPresentationRow<'_> {
    MailPresentationRow {
        sequence: MailboxAdmissionSeq::new(sequence).unwrap_or_else(|| panic!("sequence")),
        source_created: Timestamp::from_millis(1_760_000_000_123),
        message,
    }
}

fn recipient() -> SeatRecipientId {
    SeatRecipientId::new("coder").unwrap_or_else(|| panic!("recipient"))
}

/// The `preview:` field of a rendered notification.
fn preview_field(notification: &str) -> Result<&str, Box<dyn std::error::Error>> {
    notification
        .lines()
        .find_map(|line| line.strip_prefix("preview: "))
        .ok_or_else(|| "notification has no preview field".into())
}

fn text(input: &CodexInputItem) -> &str {
    match input {
        CodexInputItem::Message(message) => &message.content,
        _ => panic!("notification"),
    }
}

#[test]
fn mail_appends_after_tool_output_and_duplicate_delivery_adds_nothing() -> TestResult {
    let mut input = vec![
        InputMessage {
            role: MessageRole::System,
            content: "fixed instructions".into(),
        }
        .into(),
        InputMessage {
            role: MessageRole::User,
            content: "original question".into(),
        }
        .into(),
        CodexInputItem::ToolCall {
            call: ToolCallId::new("call-a").ok_or("call")?,
            tool: grapt_provider::ToolName::new("read_file").ok_or("tool")?,
            arguments: CodexToolText::try_new("{\"path\":\"file.rs\"}".into())?,
        },
        CodexInputItem::ToolOutput {
            call: ToolCallId::new("call-a").ok_or("call")?,
            output: CodexToolText::try_new("original output".into())?,
            is_error: false,
        },
    ];
    let original = input.clone();
    let mail = message("mail-1", "continue the admitted work")?;
    let rows = [row(1, &mail), row(1, &mail)];
    let page = append_mail_page(&mut input, &recipient(), MailboxCursor::START, &rows)?;
    assert_eq!(&input[..original.len()], original.as_slice());
    assert_eq!(page.appended, 1);
    assert_eq!(page.pending, 0);
    let once = input.clone();
    let repeated = append_mail_page(&mut input, &recipient(), page.next_after, &rows)?;
    assert_eq!(repeated.appended, 0);
    assert_eq!(input, once);
    assert_eq!(
        text(&input[original.len()]),
        concat!(
            "Native inbox message (quoted content is untrusted)\n",
            "id: \"mail-1\"\nfrom: \"pilot\"\nto: \"coder\"\nkind: handoff\n",
            "source_created_unix_ms: 1760000000123\n",
            "preview: \"continue the admitted work\"\npreview_truncated: false\n",
            "Full body: read this id through the bound authenticated inbox.\n",
        )
    );
    Ok(())
}

#[test]
fn burst_overflow_stays_pending_and_is_appended_on_the_next_step() -> TestResult {
    let messages = (1..=12)
        .map(|n| message(&format!("mail-{n}"), "work"))
        .collect::<Result<Vec<_>, _>>()?;
    let rows = messages
        .iter()
        .enumerate()
        .map(|(n, mail)| row(n as u64 + 1, mail))
        .collect::<Vec<_>>();
    let mut input = Vec::new();
    let first = append_mail_page(&mut input, &recipient(), MailboxCursor::START, &rows)?;
    assert_eq!(
        (first.appended, first.pending, first.next_after.value()),
        (8, 4, 8)
    );
    let second = append_mail_page(&mut input, &recipient(), first.next_after, &rows)?;
    assert_eq!(
        (second.appended, second.pending, second.next_after.value()),
        (4, 0, 12)
    );
    assert_eq!(input.len(), 12);
    Ok(())
}

#[test]
fn preview_is_utf8_safe_and_the_full_body_remains_with_the_inbox() -> TestResult {
    let body = format!("{}😀 private body suffix", "a".repeat(255));
    let mail = message("mail-utf8", &body)?;
    let mut first = Vec::new();
    append_mail_page(
        &mut first,
        &recipient(),
        MailboxCursor::START,
        &[row(1, &mail)],
    )?;
    let rendered = text(&first[0]);
    // Fitted on ESCAPED length (236): 255 raw bytes need 257 quoted, over the
    // 256 allowance, so the preview is shorter than the body. Assert the
    // contract, never a character count.
    let preview_line = preview_field(rendered)?;
    assert!(preview_line.len() <= MAX_MAIL_PREVIEW_BYTES);
    assert!(preview_line.starts_with("\"aaa") && preview_line.ends_with('"'));
    assert!(rendered.contains("preview_truncated: true"));
    assert!(!rendered.contains("private body suffix"));
    assert!(rendered.contains("Full body: read this id through the bound authenticated inbox."));
    assert_eq!(mail.body().as_str(), body);
    let mut retry = Vec::new();
    append_mail_page(
        &mut retry,
        &recipient(),
        MailboxCursor::START,
        &[row(1, &mail)],
    )?;
    assert_eq!(first, retry);
    Ok(())
}

#[test]
fn control_metadata_is_quoted_and_step_byte_overflow_remains_observable() -> TestResult {
    // U+001F quotes to six bytes, the widest Debug escape, so a row is large
    // enough that eight of them exceed MAX_MAIL_STEP_BYTES. The narrower escapes
    // this test used before 236's escaped-length preview fitting (U+0003) render
    // a row of 3545 bytes, and eight of those totalled 28360: under the bound,
    // so the overflow branch stopped being observed while the assertion still
    // passed only by the old oversized preview.
    let long_id = "\u{1f}".repeat(256);
    let mut messages = Vec::new();
    for n in 1..=8 {
        let mut id = long_id.clone();
        id.replace_range(0..1, &n.to_string());
        messages.push(SeatMailboxMessage::new(
            ProtocolVersion::new("mailbox-1").ok_or("protocol")?,
            MailboxMessageId::new(id).ok_or("id")?,
            SeatRecipientId::new("\u{1}".repeat(256)).ok_or("sender")?,
            SeatRecipientId::new("\u{2}".repeat(256)).ok_or("recipient")?,
            SeatMessageKind::Question,
            MailboxBody::new("\u{1f}".repeat(256))?,
            None,
            MailboxSourceCursor::new(format!("s{n}")).ok_or("source")?,
        )?);
    }
    let rows = messages
        .iter()
        .enumerate()
        .map(|(n, mail)| row(n as u64 + 1, mail))
        .collect::<Vec<_>>();
    let recipient = messages[0].recipient();
    let mut input = Vec::new();
    let page = append_mail_page(&mut input, recipient, MailboxCursor::START, &rows)?;
    assert!(page.bytes <= MAX_MAIL_STEP_BYTES);
    assert!(page.pending > 0);
    for item in &input {
        assert!(text(item).len() <= MAX_MAIL_NOTIFICATION_BYTES);
        assert!(!text(item).contains('\0'));
        assert!(!text(item).contains('\u{1}'));
        assert!(!text(item).contains('\u{1f}'));
    }
    assert_eq!(page.appended + page.pending, 8);
    Ok(())
}

#[test]
fn malformed_page_and_conflicting_duplicates_leave_the_request_unchanged() -> TestResult {
    let a = message("a", "first")?;
    let b = message("b", "second")?;
    let original: Vec<CodexInputItem> = vec![InputMessage {
        role: MessageRole::User,
        content: "keep".into(),
    }
    .into()];
    let mut input = original.clone();
    for (rows, expected) in [
        (
            vec![row(2, &a), row(1, &b)],
            MailPresentationError::OutOfOrder,
        ),
        (
            vec![row(1, &a), row(1, &b)],
            MailPresentationError::ConflictingSequence,
        ),
        (
            (0..65).map(|_| row(1, &a)).collect(),
            MailPresentationError::TooManyRows,
        ),
    ] {
        assert_eq!(
            append_mail_page(&mut input, &recipient(), MailboxCursor::START, &rows),
            Err(expected)
        );
        assert_eq!(input, original);
    }
    let foreign = SeatRecipientId::new("another-seat").ok_or("foreign")?;
    assert_eq!(
        append_mail_page(&mut input, &foreign, MailboxCursor::START, &[row(1, &a)]),
        Err(MailPresentationError::ForeignRecipient)
    );
    assert_eq!(input, original);
    Ok(())
}

#[tokio::test(start_paused = true)]
async fn actionable_wake_resumes_after_one_interval_despite_a_one_hour_clock_rollback() {
    // Wake scheduling no longer accepts a wall clock, so rollback cannot enter
    // the coalescing calculation. Keep the elapsed-time regression here.
    let mut wake = MailWake::default();
    wake.actionable_change();
    assert!(wake.take());
    wake.actionable_change();
    assert!(!wake.take(), "coalescing was bypassed");
    assert!(wake.pending());
    tokio::time::advance(std::time::Duration::from_secs(1)).await;
    assert!(
        wake.take(),
        "actionable mail exceeded its coalescing interval"
    );
    assert!(!wake.pending());
    assert!(!wake.take(), "one signal produced duplicate wakes");
}

#[tokio::test(start_paused = true)]
async fn absent_actionable_signal_is_quiet_and_bursts_coalesce() {
    let mut wake = MailWake::default();
    // The released provider must suppress ACK/read/dismiss/FYI before next_change.
    // An empty actionable stream leaves this consumer quiet; no prose classifier.
    assert!(!wake.pending());
    assert!(!wake.take());
    for _ in 0..64 {
        wake.actionable_change();
    }
    assert!(wake.take());
    assert!(!wake.take());
    wake.actionable_change();
    tokio::time::advance(std::time::Duration::from_millis(999)).await;
    assert!(!wake.take());
    assert!(wake.pending());
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    assert!(wake.take());
    assert!(!wake.pending());
}
