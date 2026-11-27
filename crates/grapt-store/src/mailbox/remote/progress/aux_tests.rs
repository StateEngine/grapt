use crate::{EventStore, RemoteMailboxInput, RemoteMailboxScope, WriteError};
use grapt_types::remote_mail::*;
use grapt_types::{MailboxBody, ProtocolVersion, SeatMessageKind, SeatRecipientId};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scope() -> Result<RemoteMailboxScope, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source-project")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    })
}
fn input(id: &str) -> Result<RemoteMailboxInput, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxInput {
        scope: scope()?,
        protocol: ProtocolVersion::new("remote-seat-mail-transport/v1").ok_or("protocol")?,
        source_message: RemoteMailboxMessageId::try_new(id)?,
        source_binding: SourceBindingId::try_new("original-binding")?,
        source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
        sender: SourcePrincipalId::try_new("pilot")?,
        kind: SeatMessageKind::Handoff,
        body: MailboxBody::new("bounded complete mail")?,
        source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
        refs: vec!["base-commit".into(), "plan-path".into()],
        supersedes: None,
        superseded_by: None,
        archive: SourceArchiveDisposition::Active,
        attention: MailAttention::Wake,
    })
}

#[test]
fn nmv2_auxiliary_supersession_is_charged_before_body_materialization() -> TestResult {
    use super::RecordMailPage;
    use grapt_types::remote_mail::progress::MailTransportVersion;
    let mut store = EventStore::open_in_memory()?;
    let scope = scope()?;
    store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
    let first = input("old")?;
    store.record_remote_mail_page(RecordMailPage {
        scope: &scope,
        protocol: MailTransportVersion::V2,
        previous: None,
        checkpoint: &first.source_cursor,
        messages: std::slice::from_ref(&first),
        gaps: &[],
        decode_failures: &[],
        created_ms: 17,
    })?;
    let mut successor = input("new")?;
    successor.body = MailboxBody::new("X".repeat(60000))?;
    successor.supersedes = Some(first.source_message.clone());
    store.record_remote_mail_page(RecordMailPage {
        scope: &scope,
        protocol: MailTransportVersion::V2,
        previous: Some(&first.source_cursor),
        checkpoint: &successor.source_cursor,
        messages: std::slice::from_ref(&successor),
        gaps: &[],
        decode_failures: &[],
        created_ms: 18,
    })?;
    let complete = store
        .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)?
        .ok_or("page")?;
    let original: usize = usize::try_from(store.connection().query_row::<i64, _, _>(
        "SELECT length(record_bytes) FROM mailbox_message WHERE supersedes IS NULL",
        [],
        |r| r.get(0),
    )?)?;
    let auxiliary: usize = usize::try_from(store.connection().query_row::<i64, _, _>(
        "SELECT length(record_bytes) FROM mailbox_message WHERE supersedes IS NOT NULL",
        [],
        |r| r.get(0),
    )?)?;
    let receipt = grapt_event::canonical_string(&complete.recorded.data).len();
    assert!(!complete.recovery[0].actionable);
    // This observes the actual bounded_row body-copy boundary, including the
    // ordinary reader: bypassing the budget cannot hide an auxiliary copy.
    super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.set(0));
    let refused = store.remote_mail_progress_page(
        &scope,
        MailTransportVersion::V2,
        None,
        64,
        receipt + original,
    );
    assert_eq!(
        super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.get()),
        original,
        "auxiliary body was materialized before its budget admission"
    );
    assert!(matches!(refused, Err(WriteError::RemoteMailProgressBudget)));
    assert_eq!(complete.decoded_bytes, receipt + original + auxiliary);
    super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.set(0));
    let exact = store
        .remote_mail_progress_page(
            &scope,
            MailTransportVersion::V2,
            None,
            64,
            receipt + original + auxiliary,
        )?
        .ok_or("exact page")?;
    assert_eq!(exact.decoded_bytes, receipt + original + auxiliary);
    assert_eq!(
        super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.get()),
        original + auxiliary
    );
    // The same validator is used by head reconciliation and original ACK reads.
    super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.set(0));
    assert_eq!(
        store
            .remote_mail_progress_acks(&scope, &exact.recorded.fact)?
            .len(),
        1
    );
    assert_eq!(
        super::super::MATERIALIZED_REMOTE_BYTES.with(|n| n.get()),
        original + auxiliary
    );
    Ok(())
}
