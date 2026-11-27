//! Local runtime witnesses, not real provider/native acceptance.
use super::snapshot::input;
use super::*;
use crate::commit_fault;
use grapt_mail_transport::fake::{DeterministicTransport, PortCall, TransportScript};
use grapt_mail_transport::*;
use grapt_store::EventStore;
use grapt_types::{MailboxBody, ProtocolVersion, SeatRecipientId};
use std::num::{NonZeroU32, NonZeroUsize};
type ResultTest = Result<(), Box<dyn std::error::Error>>;
mod high_water;
mod origin;
mod supersession;
mod v2;

fn n32(value: u32) -> NonZeroU32 {
    match NonZeroU32::new(value) {
        Some(value) => value,
        None => panic!("positive test profile"),
    }
}
fn ns(value: usize) -> NonZeroUsize {
    match NonZeroUsize::new(value) {
        Some(value) => value,
        None => panic!("positive test profile"),
    }
}
fn limits(pages: u32, rows: u32) -> ProviderLimits {
    use grapt_mail_transport::limits::ProviderLimitsParts;
    ProviderLimits::new(ProviderLimitsParts {
        max_refs_per_message: n32(64),
        max_ref_utf8_bytes: ns(2048),
        max_aggregate_ref_bytes_per_message: ns(131072),
        max_decoded_message_bytes_including_metadata: ns(196608),
        max_decoded_page_bytes_including_metadata: ns(524288),
        max_cursor_bytes: ns(144),
        max_snapshot_token_bytes: ns(36),
        max_pages_per_runtime_slice: n32(pages),
        max_messages_per_runtime_slice: n32(rows),
        max_decoded_bytes_per_runtime_slice: ns(4194304),
        max_ack_debt_items_per_recovery_page: n32(64),
        max_connect_attempts_per_activation: n32(3),
        max_reconnect_attempts_per_runtime_slice: n32(3),
        max_connect_duration_ticks: n32(5000),
        max_single_reconnect_delay_ticks: n32(1000),
        max_total_reconnect_delay_ticks_per_slice: n32(3000),
        max_fetch_duration_ticks: n32(10000),
        max_ack_duration_ticks: n32(10000),
        max_shutdown_duration_ticks: n32(5000),
        max_joined_transport_tasks_at_shutdown: n32(2),
        max_diagnostic_bytes: ns(512),
        max_body_utf8_bytes: ns(32768),
        max_messages_per_page: n32(64),
        max_aggregate_body_bytes_per_page: ns(262144),
    })
}
fn scope() -> Result<RemoteMailboxScope, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxScope {
        projects: ProjectPair::new(
            SourceProjectId::try_new("source")?,
            TargetProjectId::try_new("secode")?,
        ),
        principal: SourcePrincipalId::try_new("coder")?,
        recipient: SeatRecipientId::try_new("coder")?,
    })
}
fn protocol() -> Result<ProtocolVersion, Box<dyn std::error::Error>> {
    Ok(ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?)
}
fn binding(id: &str) -> Result<ScriptedBinding, Box<dyn std::error::Error>> {
    Ok(ScriptedBinding {
        binding_id: SourceBindingId::try_new(id)?,
        protocol: protocol()?,
        adapter_version: AdapterVersion::try_new("fake")?,
        projects: scope()?.projects,
        verified_principal: scope()?.principal,
        local_recipient: scope()?.recipient,
        mapping_version: RecipientMappingVersion::try_new("v1")?,
        takeover: TakeoverReceipt::first_open(BindingGeneration::new(1).ok_or("generation")?),
    })
}
fn armed(
    generation: u64,
    origin: &str,
) -> Result<ArmedMailboxSubscription, Box<dyn std::error::Error>> {
    Ok(ArmedMailboxSubscription::new(
        MailSubscriptionId::try_new(format!("sub-{generation}"))?,
        SubscriptionGeneration::new(generation).ok_or("generation")?,
        MailSnapshot::new(
            MailSnapshotId::try_new(format!("snapshot-{generation}"))?,
            SnapshotBarrier::try_new("barrier")?,
            None,
            SafeReplayOrigin::try_new(origin)?,
        ),
    ))
}
fn message(
    id: &str,
    bind: &str,
    attention: MailAttention,
) -> Result<BoundRemoteSeatMailMessageV1, Box<dyn std::error::Error>> {
    message_kind(id, bind, attention, RemoteSeatMailKind::Done)
}

fn message_kind(
    id: &str,
    bind: &str,
    attention: MailAttention,
    kind: RemoteSeatMailKind,
) -> Result<BoundRemoteSeatMailMessageV1, Box<dyn std::error::Error>> {
    Ok(BoundRemoteSeatMailMessageV1::try_new(
        BoundRemoteSeatMailMessageParts {
            protocol: protocol()?,
            source_binding: SourceBindingId::try_new(bind)?,
            projects: scope()?.projects,
            source_message: RemoteMailboxMessageId::try_new(id)?,
            source_cursor: ExclusiveSourceCursor::try_new(format!("cursor-{id}"))?,
            sender: SourcePrincipalId::try_new("pilot")?,
            recipient: scope()?.recipient,
            kind,
            body: MailboxBody::new("bounded body")?,
            refs: BoundedOrderedMailRefs::empty(),
            supersession: SourceSupersession::none(),
            archive: SourceArchiveDisposition::Active,
            source_created: SourceTimestamp::try_new("2026-10-13T00:00:00.000Z")?,
            attention,
        },
        &limits(8, 512).settle(),
    )?)
}
fn new_inbox() -> Result<Arc<Mutex<Inbox>>, Box<dyn std::error::Error>> {
    Ok(Arc::new(Mutex::new(Inbox::new(
        EventStore::open_in_memory()?,
    )?)))
}

#[test]
fn finding_preserves_its_kind_at_the_local_admission_boundary() -> ResultTest {
    let message = message_kind(
        "finding",
        "bind-1",
        MailAttention::Wake,
        RemoteSeatMailKind::Finding,
    )?;
    let incoming = input(&scope()?, &message);
    assert_eq!(
        incoming.kind,
        grapt_types::mailbox::SeatMessageKind::Finding
    );
    let mut store = EventStore::open_in_memory()?;
    let admitted = store.admit_remote_mailbox(&scope()?, &incoming)?;
    assert_eq!(admitted.ack.source_message(), message.source_message());
    Ok(())
}
fn ack_receipt(ack: &DeliveryAdmissionAck) -> DeliveryAckOutcome {
    DeliveryAckOutcome::Recorded(DeliveryAckReceipt::new(
        ack.ack_id().clone(),
        ack.source_message().clone(),
        ack.projects().clone(),
    ))
}
fn predicted_ack(
    message: &BoundRemoteSeatMailMessageV1,
) -> Result<DeliveryAdmissionAck, Box<dyn std::error::Error>> {
    Ok(EventStore::open_in_memory()?
        .admit_remote_mailbox(&scope()?, &input(&scope()?, message))?
        .ack)
}
fn runtime(
    inbox: Arc<Mutex<Inbox>>,
    script: TransportScript,
    limits: ProviderLimits,
) -> Result<RemoteMailboxRuntime<DeterministicTransport>, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxRuntime::new(
        DeterministicTransport::new(script),
        inbox,
        scope()?,
        OpenRequest::new(
            scope()?.projects.target().clone(),
            scope()?.recipient,
            protocol()?,
            MailTransportClientInstanceId::try_new("client")?,
        ),
        limits,
    )?)
}
fn row_count(inbox: &Arc<Mutex<Inbox>>) -> Result<i64, Box<dyn std::error::Error>> {
    Ok(inbox
        .lock()
        .map_err(|_| "store lock")?
        .store()
        .connection()
        .query_row("SELECT count(*) FROM mailbox_message", [], |row| row.get(0))?)
}
fn pending(
    inbox: &Arc<Mutex<Inbox>>,
) -> Result<Vec<grapt_store::RemoteMailboxRecord>, Box<dyn std::error::Error>> {
    Ok(inbox
        .lock()
        .map_err(|_| "store lock")?
        .store()
        .read_remote_mailbox(&scope()?, MailboxCursor::START, 64, 4194304)?
        .records)
}
fn cancel() -> (
    watch::Sender<Option<InterruptionSource>>,
    watch::Receiver<Option<InterruptionSource>>,
) {
    watch::channel(None)
}

#[tokio::test]
async fn source_before_admission_remains_absent_on_fetch_failure() -> ResultTest {
    let inbox = new_inbox()?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Err(TransportFailure::Disconnected)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await.is_err());
    assert_eq!(row_count(&inbox)?, 0);
    assert_eq!(
        owner
            .port
            .count(|call| matches!(call, PortCall::AcknowledgeDelivery { .. })),
        0
    );
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn admission_before_failed_ack_retains_full_original_debt() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![mail]))],
            acks: vec![Err(DeliveryAckFailure::AcknowledgementConflict)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await.is_err());
    assert_eq!(row_count(&inbox)?, 1);
    let rows = pending(&inbox)?;
    assert!(rows[0].delivery_pending());
    assert!(!rows[0].consumed());
    assert_eq!(rows[0].ack().source_binding().as_str(), "original");
    assert_eq!(
        owner
            .port
            .count(|call| matches!(call, PortCall::AcknowledgeDelivery { .. })),
        1
    );
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn restarted_owner_retries_original_debt_before_enumeration() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Quiet)?;
    let original = inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, &mail))?
        .ack;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("replacement")?)],
            acks: vec![Ok(ack_receipt(&original))],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    let slice = owner.advance_slice(&signal).await?;
    assert!(slice.live);
    assert!(!slice.rescan_notifications);
    let rows = pending(&inbox)?;
    assert_eq!(rows[0].ack(), &original);
    assert!(!rows[0].delivery_pending());
    let calls = owner.port.calls();
    assert_eq!(
        calls[1],
        PortCall::AcknowledgeDelivery {
            ack: Box::new(original.clone())
        }
    );
    assert!(matches!(
        calls[2],
        PortCall::SubscribeAndSnapshot { cold: true }
    ));
    Ok(())
}

#[tokio::test]
async fn consumed_quiet_debt_is_repaired_without_notification() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let original = inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, &mail))?
        .ack;
    inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .consume_remote_mailbox(&scope()?, original.local_message())?;
    // It was actionable when consumed; a later quiet replay cannot reset that
    // marker and does not erase its independent delivery debt.
    let quiet = message("one", "replacement", MailAttention::Quiet)?;
    inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, &quiet))?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("replacement")?)],
            acks: vec![Ok(ack_receipt(&original))],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    let slice = owner.advance_slice(&signal).await?;
    assert!(slice.live && !slice.rescan_notifications);
    assert!(!pending(&inbox)?[0].delivery_pending());
    assert!(pending(&inbox)?[0].consumed());
    Ok(())
}

#[tokio::test]
async fn foreign_binding_cannot_resume_on_a_second_call() -> ResultTest {
    let inbox = new_inbox()?;
    let mut foreign = binding("foreign")?;
    foreign.verified_principal = SourcePrincipalId::try_new("other")?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(foreign)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    for _ in 0..2 {
        assert!(matches!(
            owner.advance_slice(&signal).await,
            Err(RemoteMailboxError::ForeignBinding)
        ));
    }
    assert_eq!(row_count(&inbox)?, 0);
    assert_eq!(owner.port.calls(), &[PortCall::Open]);
    Ok(())
}

#[tokio::test]
async fn primary_receipt_mismatch_never_settles_local_debt() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let ack = predicted_ack(&mail)?;
    let wrong = DeliveryAckReceipt::new(
        ack.ack_id().clone(),
        RemoteMailboxMessageId::try_new("another")?,
        ack.projects().clone(),
    );
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![mail]))],
            acks: vec![Ok(DeliveryAckOutcome::Recorded(wrong))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(matches!(
        owner.advance_slice(&signal).await,
        Err(RemoteMailboxError::ForeignReceipt)
    ));
    assert!(pending(&inbox)?[0].delivery_pending());
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn slice_exhaustion_keeps_source_page_cursor_and_owed_work() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let ack = predicted_ack(&mail)?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![
                Ok(RemoteMailPage::bounded(
                    vec![mail.clone()],
                    mail.source_cursor().clone(),
                )),
                Ok(RemoteMailPage::final_page(vec![])),
            ],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(1, 512),
    )?;
    let (_sender, signal) = cancel();
    let first = owner.advance_slice(&signal).await?;
    assert!(!first.live && first.pages == 1);
    assert_eq!(
        owner
            .port
            .count(|call| matches!(call, PortCall::FetchSnapshotPage { .. })),
        0
    );
    let second = owner.advance_slice(&signal).await?;
    assert!(!second.live && second.pages == 1);
    let third = owner.advance_slice(&signal).await?;
    assert!(!third.live && third.rescan_notifications);
    let fourth = owner.advance_slice(&signal).await?;
    assert!(fourth.live);
    assert!(owner.port.calls().iter().any(|call| call
        == &PortCall::FetchSnapshotPage {
            after: Some("cursor-one".into())
        }));
    assert_eq!(row_count(&inbox)?, 1);
    Ok(())
}

#[tokio::test]
async fn invalid_cursor_replays_only_the_named_covering_origin() -> ResultTest {
    let inbox = new_inbox()?;
    let invalid = CursorInvalidation::new(
        CursorInvalidReason::Expired,
        CursorRecovery::ReplayFrom(SafeReplayOrigin::try_new("covering")?),
    );
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?), Ok(armed(2, "wrong")?)],
            pages: vec![
                Err(TransportFailure::CursorInvalid(invalid)),
                Ok(RemoteMailPage::final_page(vec![])),
            ],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(matches!(
        owner.advance_slice(&signal).await,
        Err(RemoteMailboxError::RecoveryUnavailable)
    ));
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn unavailable_recovery_is_not_empty_mail() -> ResultTest {
    let inbox = new_inbox()?;
    let invalid = CursorInvalidation::new(
        CursorInvalidReason::Compacted,
        CursorRecovery::RecoveryUnavailable {
            detail: "no covering origin",
        },
    );
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Err(TransportFailure::CursorInvalid(invalid))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await.is_err());
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn cancellation_wins_before_any_physical_call() -> ResultTest {
    let inbox = new_inbox()?;
    let mut owner = runtime(inbox.clone(), TransportScript::default(), limits(8, 512))?;
    let (sender, signal) = cancel();
    sender.send_replace(Some(InterruptionSource::User));
    assert!(matches!(
        owner.advance_slice(&signal).await,
        Err(RemoteMailboxError::Cancelled)
    ));
    assert!(owner.port.calls().is_empty());
    assert_eq!(row_count(&inbox)?, 0);
    Ok(())
}

#[tokio::test]
async fn owned_wait_cancels_without_a_detached_task() -> ResultTest {
    let (sender, signal) = cancel();
    let wait = io::operation::<()>(&signal, 5000, std::future::pending());
    tokio::pin!(wait);
    tokio::select! { biased; result=&mut wait => panic!("unexpected completion {result:?}"), ()=tokio::task::yield_now() => {} }
    sender.send_replace(Some(InterruptionSource::User));
    assert!(matches!(wait.await, Err(RemoteMailboxError::Cancelled)));
    Ok(())
}

#[tokio::test]
async fn current_signal_rescans_but_stale_generation_does_not() -> ResultTest {
    let inbox = new_inbox()?;
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(2, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await?.live);
    assert!(!owner.observe_change(ChangeSignal::new(SubscriptionGeneration::FIRST)));
    assert!(owner.is_live());
    assert!(owner.observe_change(ChangeSignal::new(
        SubscriptionGeneration::new(2).ok_or("generation")?
    )));
    assert!(!owner.is_live());
    assert_eq!(owner.recovery_after, MailboxCursor::START);
    Ok(())
}

#[tokio::test]
async fn lost_successful_ack_response_preserves_exact_debt_for_retry() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let ack = predicted_ack(&mail)?;
    let receipt = match ack_receipt(&ack) {
        DeliveryAckOutcome::Recorded(receipt) => receipt,
        _ => return Err("receipt".into()),
    };
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![
                Ok(RemoteMailPage::final_page(vec![mail.clone()])),
                Ok(RemoteMailPage::final_page(vec![mail])),
            ],
            acks: vec![
                Ok(DeliveryAckOutcome::Recorded(receipt.clone())),
                Ok(DeliveryAckOutcome::ExactRetry(receipt)),
            ],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    owner.port.lose_next_ack_response();
    let (_sender, signal) = cancel();
    let failed = owner.advance_slice(&signal).await;
    assert!(matches!(
        failed,
        Err(RemoteMailboxError::Port(OperationFailure {
            effect: grapt_mail_transport::operation::WriteEffect::UnknownRetryExact,
            ..
        }))
    ));
    assert!(pending(&inbox)?[0].delivery_pending());
    assert!(!owner.is_live());
    assert!(owner.advance_slice(&signal).await?.live);
    let sent: Vec<_> = owner
        .port
        .calls()
        .iter()
        .filter_map(|call| match call {
            PortCall::AcknowledgeDelivery { ack } => Some(ack.as_ref()),
            _ => None,
        })
        .collect();
    assert_eq!(sent, vec![&ack, &ack]);
    assert_eq!(row_count(&inbox)?, 1);
    assert!(!pending(&inbox)?[0].delivery_pending());
    Ok(())
}

#[tokio::test]
async fn checkpoint_candidate_remains_private_until_all_prefix_pages_validate() -> ResultTest {
    let inbox = new_inbox()?;
    for id in ["one", "two", "three"] {
        let mail = message(id, "original", MailAttention::Quiet)?;
        let ack = inbox
            .lock()
            .map_err(|_| "store lock")?
            .store_mut()
            .admit_remote_mailbox(&scope()?, &input(&scope()?, &mail))?
            .ack;
        inbox
            .lock()
            .map_err(|_| "store lock")?
            .store_mut()
            .settle_remote_mailbox_delivery(&scope()?, &ack, None)?;
    }
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            ..Default::default()
        },
        limits(1, 1),
    )?;
    let (_sender, signal) = cancel();
    for _ in 0..3 {
        assert!(!owner.advance_slice(&signal).await?.live);
    }
    // Newest candidate is found first, but two older pages remain unread.
    assert!(!owner.advance_slice(&signal).await?.live);
    assert!(owner.checkpoint_candidate.is_some());
    assert!(owner.validated_checkpoint().is_err());
    assert!(!owner.advance_slice(&signal).await?.live);
    assert!(owner.validated_checkpoint().is_err());
    assert!(!owner.advance_slice(&signal).await?.live);
    assert_eq!(
        owner
            .validated_checkpoint()?
            .map(ExclusiveSourceCursor::as_str),
        Some("cursor-three")
    );
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn unmeasured_ack_cannot_erase_known_secondary_repair_debt() -> ResultTest {
    let inbox = new_inbox()?;
    let mail = message("one", "original", MailAttention::Quiet)?;
    let ack = inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, &mail))?
        .ack;
    inbox
        .lock()
        .map_err(|_| "store lock")?
        .store_mut()
        .settle_remote_mailbox_delivery(
            &scope()?,
            &ack,
            Some(grapt_mail_transport::ack::SecondaryProjectionState::Refused),
        )?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("replacement")?)],
            acks: vec![Ok(ack_receipt(&ack))],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(owner.advance_slice(&signal).await?.live);
    assert!(!pending(&inbox)?[0].delivery_pending());
    assert!(
        pending(&inbox)?[0].repair_pending(),
        "absent projection cannot claim repair completion"
    );
    Ok(())
}

#[tokio::test]
async fn successful_primary_before_failed_local_commit_leaves_retry_debt() -> ResultTest {
    let inbox = new_inbox()?;
    // Allow admission, but veto the transaction that updates settlement.
    let _fault = commit_fault::CommitFault::install(
        inbox.lock().map_err(|_| "store lock")?.store_mut(),
        rusqlite::hooks::Action::SQLITE_UPDATE,
        "mailbox_message",
        0,
    )?;
    let mail = message("one", "original", MailAttention::Wake)?;
    let ack = predicted_ack(&mail)?;
    let mut owner = runtime(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![mail]))],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_sender, signal) = cancel();
    assert!(matches!(
        owner.advance_slice(&signal).await,
        Err(RemoteMailboxError::Store(_))
    ));
    assert_eq!(
        owner
            .port
            .count(|call| matches!(call, PortCall::AcknowledgeDelivery { .. })),
        1
    );
    assert!(pending(&inbox)?[0].delivery_pending());
    assert_eq!(pending(&inbox)?[0].ack(), &ack);
    assert!(!owner.is_live());
    Ok(())
}

#[tokio::test]
async fn bounded_shutdown_still_closes_after_user_cancellation() -> ResultTest {
    let inbox = new_inbox()?;
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            closes: vec![Ok(ShutdownReceipt::new(0, 1))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (sender, signal) = cancel();
    owner.ensure_open(&signal).await?;
    sender.send_replace(Some(InterruptionSource::User));
    assert_eq!(owner.close().await?, Some(ShutdownReceipt::new(0, 1)));
    assert!(matches!(owner.port.calls().last(), Some(PortCall::Close)));
    assert!(owner.binding.is_none());
    assert!(!owner.is_live());
    Ok(())
}

fn notification_binding() -> Result<grapt_store::MailBindingExpectation, Box<dyn std::error::Error>>
{
    Ok(grapt_store::MailBindingExpectation::new(
        grapt_types::InstructionKey::new("stateengine-identity").ok_or("key")?,
        "bound-coder".into(),
    )?)
}
async fn notification_owner(
    rows: Vec<grapt_store::RemoteMailboxInput>,
) -> Result<RemoteMailboxRuntime<DeterministicTransport>, Box<dyn std::error::Error>> {
    let inbox = new_inbox()?;
    {
        let mut held = inbox.lock().map_err(|_| "inbox")?;
        for row in rows {
            let ack = held.store_mut().admit_remote_mailbox(&scope()?, &row)?.ack;
            held.store_mut().settle_remote_mailbox_delivery(
                &scope()?,
                &ack,
                Some(grapt_types::remote_mail::SecondaryProjectionState::Posted),
            )?;
        }
    }
    let mut owner = runtime(
        inbox,
        TransportScript {
            open: vec![Ok(binding("original")?)],
            subscribe: vec![Ok(armed(1, "origin")?)],
            pages: vec![Ok(RemoteMailPage::final_page(vec![]))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_send, cancel) = cancel();
    for _ in 0..4 {
        if owner.advance_slice(&cancel).await?.live {
            return Ok(owner);
        }
    }
    Err("fixture did not converge".into())
}
#[tokio::test]
async fn notification_batch_is_stable_unconsumed_and_bounded_with_overflow_pending() -> ResultTest {
    let mut rows = Vec::new();
    for i in 0..9 {
        rows.push(input(
            &scope()?,
            &message(&format!("source-{i}"), "original", MailAttention::Wake)?,
        ));
    }
    let mut owner = notification_owner(rows).await?;
    let session = "ses_notification".parse()?;
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    let first = owner.prepare_notifications(&session, step, notification_binding()?)?;
    let first_batch = first.batch.ok_or("batch")?;
    assert_eq!(first_batch.count(), 8);
    assert!(first.pending);
    assert!(first_batch.content().contains("id: \"source-0\""));
    assert!(first_batch
        .content()
        .contains("source_created: \"2026-10-13T00:00:00.000Z\""));
    assert!(!first_batch.content().contains("source-8"));
    let second = owner
        .prepare_notifications(&session, step, notification_binding()?)?
        .batch
        .ok_or("batch")?;
    assert_eq!(first_batch.content(), second.content());
    assert!(pending(&owner.inbox)?.iter().all(|row| !row.consumed()));
    assert!(first_batch
        .presentation(
            &grapt_types::PhysicalAttemptId::new("attempt").ok_or("attempt")?,
            grapt_types::StepNumber::new(2).ok_or("step")?
        )
        .is_err());
    Ok(())
}
#[tokio::test]
async fn quiet_prefix_does_not_starve_actionable_notifications() -> ResultTest {
    let mut rows = Vec::new();
    for i in 0..65 {
        rows.push(input(
            &scope()?,
            &message(&format!("quiet-{i}"), "original", MailAttention::Quiet)?,
        ));
    }
    let mut wake = input(
        &scope()?,
        &message("wake-after-prefix", "original", MailAttention::Wake)?,
    );
    wake.body = MailboxBody::new("€".repeat(100))?;
    rows.push(wake);
    let mut owner = notification_owner(rows).await?;
    let scan = owner.prepare_notifications(
        &"ses_notification".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    let batch = scan.batch.ok_or("quiet prefix starved work")?;
    assert_eq!(batch.count(), 1);
    assert!(batch.content().contains("wake-after-prefix"));
    // ESCAPED-length fitting (236): 85 raw `€` need 257 quoted bytes.
    let p = batch
        .content()
        .lines()
        .find_map(|l| l.strip_prefix("preview: "));
    let p = p.ok_or("preview absent")?;
    assert!(p.len() <= crate::mailbox::presentation::MAX_MAIL_PREVIEW_BYTES && p.contains('€'));
    assert!(batch.content().contains("preview_truncated: true"));
    assert!(!batch.content().contains("quiet-"));
    Ok(())
}
#[tokio::test]
async fn notification_byte_overflow_remains_unconsumed_pending() -> ResultTest {
    let mut rows = Vec::new();
    for i in 0..8 {
        let id = format!("{}{}", "\u{1}".repeat(255), i);
        let mut row = input(
            &scope()?,
            &message("byte-id", "original", MailAttention::Wake)?,
        );
        row.source_message = RemoteMailboxMessageId::try_new(id)?;
        row.source_cursor = ExclusiveSourceCursor::try_new(format!("cursor-{i}"))?;
        row.sender = SourcePrincipalId::try_new("\u{1}".repeat(256))?;
        row.source_created = SourceTimestamp::try_new("\u{1}".repeat(256))?;
        row.body = MailboxBody::new("\u{1}".repeat(256))?;
        rows.push(row);
    }
    let mut owner = notification_owner(rows).await?;
    let scan = owner.prepare_notifications(
        &"ses_notification".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    let batch = scan.batch.ok_or("batch")?;
    assert!(scan.pending);
    assert!(batch.count() < 8);
    assert!(batch.content().len() <= crate::mailbox::presentation::MAX_MAIL_STEP_BYTES);
    assert!(pending(&owner.inbox)?.iter().all(|row| !row.consumed()));
    Ok(())
}
