//! An admitted kind must still fit the notification consumer's bounded projection.
use grapt_mail_transport::{
    ExclusiveSourceCursor, MailAttention, ProjectPair, RemoteMailboxMessageId,
    SourceArchiveDisposition, SourceBindingId, SourcePrincipalId, SourceProjectId, SourceTimestamp,
    TargetProjectId,
};
use grapt_runtime::mailbox::{
    presentation::MAX_MAIL_NOTIFICATION_BYTES, remote::render_notification,
};
use grapt_store::{EventStore, RemoteMailboxInput, RemoteMailboxRecord, RemoteMailboxScope};
use grapt_types::{MailboxBody, ProtocolVersion, SeatMessageKind, SeatRecipientId};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

fn admitted(kind: &str) -> TestResult<RemoteMailboxRecord> {
    admitted_with_supersedes(kind, false)
}

fn admitted_with_supersedes(kind: &str, superseding: bool) -> TestResult<RemoteMailboxRecord> {
    let metadata = "\u{1}".repeat(256);
    let scope = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new(metadata.clone())?,
            TargetProjectId::try_new("target")?,
        ),
        principal: SourcePrincipalId::try_new(metadata.clone())?,
        recipient: SeatRecipientId::try_new(metadata.clone())?,
    };
    let input = RemoteMailboxInput {
        scope: scope.clone(),
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(metadata.clone())?,
        source_binding: SourceBindingId::try_new("fixture-binding")?,
        source_cursor: ExclusiveSourceCursor::try_new("cursor")?,
        sender: SourcePrincipalId::try_new(metadata.clone())?,
        kind: SeatMessageKind::parse(kind).ok_or("admissible kind refused")?,
        body: MailboxBody::new(metadata.clone())?,
        source_created: SourceTimestamp::try_new(metadata)?,
        refs: Vec::new(),
        supersedes: superseding
            .then(|| RemoteMailboxMessageId::try_new("\u{2}".repeat(256)))
            .transpose()?,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention: MailAttention::Wake,
    };
    let mut store = EventStore::open_in_memory()?;
    let acknowledgment = store.admit_remote_mailbox(&scope, &input)?.ack;
    store.settle_remote_mailbox_delivery(&scope, &acknowledgment, None)?;
    Ok(store.remote_mailbox_message(&scope, acknowledgment.local_message())?)
}

#[test]
fn a_superseding_row_at_admitted_metadata_bounds_still_reaches_the_notification_consumer(
) -> TestResult {
    let label = format!("{}\"\"", "\u{200b}".repeat(42));
    let record = admitted_with_supersedes(&label, true)?;
    assert!(record.actionable());
    let notification = render_notification(&record)?;
    assert!(notification.len() <= MAX_MAIL_NOTIFICATION_BYTES);
    assert!(record.message().body().len_bytes() > 0);
    let displayed = notification
        .lines()
        .find_map(|line| line.strip_prefix("preview: "))
        .ok_or("notification has no preview field")?;
    assert_ne!(
        displayed, "(empty message)",
        "an omitted preview is not an empty body"
    );
    Ok(())
}

#[test]
fn a_maximum_unknown_kind_can_notify_with_other_admitted_metadata_at_its_bounds() -> TestResult {
    let known = render_notification(&admitted("ruling-request")?)?;
    assert!(known.contains("\nkind: ruling-request\n"));
    let label = format!("{}\"\"", "\u{200b}".repeat(42));
    assert_eq!(label.len(), 128);
    let record = admitted(&label)?;
    assert_eq!(record.message().kind().as_str(), label);
    assert_eq!(record.message().body().len_bytes(), 256);
    let notification = render_notification(&record)?;
    assert!(notification.len() <= MAX_MAIL_NOTIFICATION_BYTES);
    let displayed_kind = notification
        .lines()
        .find_map(|line| line.strip_prefix("kind: "))
        .ok_or("notification has no kind")?;
    assert!(displayed_kind.starts_with('"'));
    assert!(displayed_kind.contains('…') || displayed_kind.contains("..."));
    assert!(displayed_kind.len() < format!("{label:?}").len());
    assert_eq!(record.message().kind().as_str(), label);
    Ok(())
}
