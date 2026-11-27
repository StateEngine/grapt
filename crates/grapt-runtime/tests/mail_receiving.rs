//! Wire mail reaches durable readback, acknowledgment and both notification consumers.
mod mailbox_receiving;

use grapt_inbox::Inbox;
use grapt_mail_transport::{
    MailTransportClientInstanceId, ProjectPair, SourcePrincipalId, SourceProjectId, TargetProjectId,
};
use grapt_runtime::mailbox::{
    presentation::{append_mail_page, MailPresentationRow},
    remote::http::{http_mailbox, http_mailbox_v2},
};
use grapt_store::{EventStore, MailBindingExpectation, RemoteMailboxScope};
use grapt_types::{
    ClockAuthority, InstructionKey, MailboxCursor, SeatRecipientId, Secret, StepNumber, Timestamp,
};
use mailbox_receiving::Peer;
use std::num::NonZeroU16;
use std::sync::{Arc, Mutex};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_760_000_000_123)
    }
}

async fn receive(version: u8, kind: &str, body: &str) -> TestResult {
    let peer = Peer::start(version, kind, body)?;
    let scope = RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source")?,
            TargetProjectId::try_new("target")?,
        ),
        principal: SourcePrincipalId::try_new("recipient")?,
        recipient: SeatRecipientId::try_new("recipient")?,
    };
    let inbox = Arc::new(Mutex::new(Inbox::new(EventStore::open_in_memory()?)?));
    let port = NonZeroU16::new(peer.address.port()).ok_or("port")?;
    let capability = Arc::new(Secret::new("MAIL_FIXTURE_CAPABILITY".to_owned()));
    let client = MailTransportClientInstanceId::try_new("fixture-client")?;
    let mut owner = if version == 2 {
        http_mailbox_v2(
            port,
            capability,
            inbox.clone(),
            scope.clone(),
            client,
            Arc::new(Clock),
        )?
    } else {
        http_mailbox(port, capability, inbox.clone(), scope.clone(), client)?
    };
    let (_sender, cancel) = tokio::sync::watch::channel(None);
    let observed = read_admitted(&mut owner, &inbox, &scope, &cancel).await;
    let closed = owner.close().await;
    let requests = peer.finish()?;
    closed?;
    let (record, batch, decode_failures) = observed?;
    assert_eq!(decode_failures, 0);
    verify_received(&inbox, &scope, &record, &batch, &requests, kind, body)
}

async fn read_admitted(
    owner: &mut grapt_runtime::mailbox::remote::http::HttpMailbox,
    inbox: &Mutex<Inbox>,
    scope: &RemoteMailboxScope,
    cancel: &tokio::sync::watch::Receiver<Option<grapt_types::InterruptionSource>>,
) -> TestResult<(
    grapt_store::RemoteMailboxRecord,
    grapt_runtime::mailbox::remote::NativeMailBatch,
    u64,
)> {
    for _ in 0..8 {
        if owner.advance_slice(cancel).await?.live {
            break;
        }
    }
    if !owner.is_live() {
        return Err("wire mailbox did not reach its live boundary".into());
    }
    let mut records = inbox
        .lock()
        .map_err(|_| "inbox")?
        .store()
        .read_remote_mailbox(scope, MailboxCursor::START, 64, 1_048_576)?
        .records;
    if records.len() != 1 {
        return Err(format!("received row was not listed: {} rows", records.len()).into());
    }
    let record = records.pop().ok_or("received row disappeared")?;
    let batch = owner
        .prepare_notifications(
            &"ses_mail_receiving".parse()?,
            StepNumber::new(1).ok_or("step")?,
            MailBindingExpectation::new(
                InstructionKey::new("mail-binding").ok_or("key")?,
                "fixture".into(),
            )?,
        )?
        .batch
        .ok_or("received mail had no notification")?;
    Ok((record, batch, owner.recorded_decode_failures()))
}

fn verify_received(
    inbox: &Mutex<Inbox>,
    scope: &RemoteMailboxScope,
    record: &grapt_store::RemoteMailboxRecord,
    batch: &grapt_runtime::mailbox::remote::NativeMailBatch,
    requests: &[serde_json::Value],
    kind: &str,
    body: &str,
) -> TestResult {
    assert_eq!(record.message().kind().as_str(), kind);
    assert_eq!(record.message().body().as_str(), body);
    assert!(record.actionable() && !record.delivery_pending());
    let mut held = inbox.lock().map_err(|_| "inbox")?;
    let reread = held
        .store()
        .remote_mailbox_message(scope, record.message().id())?;
    assert_eq!(reread.message(), record.message());
    let acknowledgments: Vec<_> = requests
        .iter()
        .filter(|request| request["operation"] == "acknowledge_delivery")
        .collect();
    assert_eq!(acknowledgments.len(), 1);
    assert_eq!(
        acknowledgments[0]["args"]["ack"]["source_message"],
        "mail-one"
    );
    assert_eq!(
        acknowledgments[0]["args"]["ack"]["local_message"],
        record.message().id().as_str()
    );
    assert_eq!(batch.count(), 1);
    assert_notification(batch.content(), kind, body);
    assert!(batch
        .content()
        .contains(&format!("\nfrom: {:?}\n", record.sender().as_str())));
    let mut local = Vec::new();
    append_mail_page(
        &mut local,
        &scope.recipient,
        MailboxCursor::START,
        &[MailPresentationRow {
            sequence: record.ack().local_admission(),
            source_created: Clock.now(),
            message: record.message(),
        }],
    )?;
    let grapt_provider::CodexInputItem::Message(notification) = &local[0] else {
        return Err("local mail consumer did not emit a message".into());
    };
    assert_notification(&notification.content, kind, body);
    assert!(notification.content.contains(&format!(
        "\nfrom: {:?}\n",
        record.message().sender().as_str()
    )));
    held.store_mut()
        .consume_remote_mailbox(scope, record.message().id())?;
    assert!(held
        .store()
        .remote_mailbox_message(scope, record.message().id())?
        .consumed());
    Ok(())
}

fn assert_notification(content: &str, kind: &str, body: &str) {
    let displayed_kind = match kind {
        "handoff" | "finding" => kind.to_owned(),
        unknown => format!("{unknown:?}"),
    };
    assert!(content.contains(&format!("\nkind: {displayed_kind}\n")));
    assert!(
        content.contains("source_created: \"2026-10-13T15:00:00.000Z\"")
            || content.contains("source_created_unix_ms: 1760000000123")
    );
    let preview = if body.is_empty() {
        "(empty message)".to_owned()
    } else {
        format!("{body:?}")
    };
    assert!(content.contains(&format!("\npreview: {preview}\n")));
    assert!(!content.contains("Inbox warning: skipped"));
}

#[tokio::test]
async fn never_seen_kind_is_listed_readable_acknowledged_and_notified_verbatim() -> TestResult {
    for version in [1, 2] {
        for kind in ["review-verdict", "Review-VerDict/β"] {
            receive(version, kind, "a received review").await?;
        }
    }
    Ok(())
}

#[tokio::test]
async fn empty_received_body_is_readable_acknowledged_and_explicitly_notified() -> TestResult {
    for version in [1, 2] {
        receive(version, "handoff", "").await?;
    }
    Ok(())
}

#[tokio::test]
async fn declared_finding_arrives_as_finding() -> TestResult {
    receive(2, "finding", "a received finding").await
}
