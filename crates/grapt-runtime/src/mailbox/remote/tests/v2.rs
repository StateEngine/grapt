use super::*;
mod high_water;
mod recovery_refusal;
mod reobserve;
mod wake;
#[derive(Debug)]
struct Clock;
impl grapt_types::ClockAuthority for Clock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(17)
    }
}
fn gap(id: usize) -> Result<RefusedMailRow, Box<dyn std::error::Error>> {
    Ok(RefusedMailRow::new(
        None,
        id as u64 * 2,
        id as u64 * 2 + 1,
        RefusedRowReason::MalformedEnvelope,
        ExclusiveSourceCursor::try_new(format!("ref1.{id}"))?,
    )
    .ok_or("gap")?)
}
fn bound(id: &str) -> Result<ScriptedBinding, Box<dyn std::error::Error>> {
    let mut binding = binding(id)?;
    binding.protocol = ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V2).ok_or("protocol")?;
    Ok(binding)
}
fn subscribed(
    after: Option<ExclusiveSourceCursor>,
) -> Result<ArmedMailboxSubscription, Box<dyn std::error::Error>> {
    Ok(ArmedMailboxSubscription::new(
        MailSubscriptionId::try_new("v2-sub")?,
        SubscriptionGeneration::FIRST,
        MailSnapshot::new(
            MailSnapshotId::try_new("v2-snapshot")?,
            SnapshotBarrier::try_new("barrier")?,
            after,
            SafeReplayOrigin::try_new("retained-origin/v1")?,
        ),
    ))
}
fn page(
    messages: Vec<BoundRemoteSeatMailMessageV1>,
    gaps: Vec<RefusedMailRow>,
    more: bool,
) -> Result<RemoteMailPage, Box<dyn std::error::Error>> {
    let last = gaps
        .last()
        .map(|g| g.cursor().clone())
        .or_else(|| messages.last().map(|m| m.source_cursor().clone()));
    Ok(
        RemoteMailPage::v2_page(messages, gaps, if more { last.clone() } else { None }, last)
            .ok_or("page")?,
    )
}
fn owner(
    inbox: Arc<Mutex<Inbox>>,
    script: TransportScript,
    profile: ProviderLimits,
) -> Result<RemoteMailboxRuntime<DeterministicTransport>, Box<dyn std::error::Error>> {
    Ok(RemoteMailboxRuntime::new(
        DeterministicTransport::new(script),
        inbox,
        scope()?,
        OpenRequest::new(
            scope()?.projects.target().clone(),
            scope()?.recipient,
            ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V2).ok_or("protocol")?,
            MailTransportClientInstanceId::try_new("v2-client")?,
        ),
        profile,
    )?
    .with_progress_clock(Arc::new(Clock))?)
}
fn counts(inbox: &Arc<Mutex<Inbox>>) -> Result<(i64, i64), Box<dyn std::error::Error>> {
    let held = inbox.lock().map_err(|_| "inbox")?;
    Ok((
        held.store()
            .connection()
            .query_row("SELECT count(*) FROM mailbox_message", [], |r| r.get(0))?,
        held.store().connection().query_row(
            "SELECT count(*) FROM event WHERE aggregate_family='mail'",
            [],
            |r| r.get(0),
        )?,
    ))
}
#[tokio::test]
async fn nmv2_mixed_page_records_gaps_without_ack_or_notification_authority() -> ResultTest {
    let inbox = new_inbox()?;
    let message = message("healthy", "bind-1", MailAttention::Wake)?;
    let ack = predicted_ack(&message)?;
    let gap = gap(1)?;
    let last = gap.cursor().clone();
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![message], vec![gap], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    let slice = owner.advance_slice(&signal).await?;
    assert!(slice.live);
    assert_eq!(slice.rows, 2);
    assert_eq!(counts(&inbox)?, (1, 1));
    assert_eq!(owner.validated_checkpoint()?, Some(&last));
    assert_eq!(
        owner
            .port
            .calls()
            .iter()
            .filter(|call| matches!(call, PortCall::AcknowledgeDelivery { .. }))
            .count(),
        1
    );
    let batch = owner.prepare_notifications(
        &"ses_notification".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    assert_eq!(batch.batch.ok_or("batch")?.count(), 1);
    Ok(())
}
#[tokio::test]
async fn nmv2_crash_after_page_commit_repairs_original_ack_before_warm_resume() -> ResultTest {
    let inbox = new_inbox()?;
    let message = message("healthy", "bind-1", MailAttention::Quiet)?;
    let ack = predicted_ack(&message)?;
    let gap = gap(1)?;
    let last = gap.cursor().clone();
    let mut first = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![message], vec![gap], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    first.port.lose_next_ack_response();
    let (_cancel, signal) = watch::channel(None);
    assert!(first.advance_slice(&signal).await.is_err());
    assert_eq!(counts(&inbox)?, (1, 1));
    drop(first);
    let mut resumed = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            subscribe: vec![Ok(subscribed(Some(last.clone()))?)],
            pages: vec![Ok(page(vec![], vec![], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    assert!(resumed.advance_slice(&signal).await?.live);
    assert_eq!(counts(&inbox)?, (1, 1));
    assert_eq!(resumed.validated_checkpoint()?, Some(&last));
    let calls = resumed.port.calls();
    let ack_index = calls
        .iter()
        .position(|c| matches!(c,PortCall::AcknowledgeDelivery{ack:original} if **original==ack))
        .ok_or("original ACK not retried")?;
    let subscribe = calls
        .iter()
        .position(|c| matches!(c, PortCall::SubscribeAndSnapshot { cold: false }))
        .ok_or("not warm")?;
    assert!(ack_index < subscribe);
    assert!(!inbox
        .lock()
        .map_err(|_| "inbox")?
        .store()
        .remote_mailbox_message(&scope()?, ack.local_message())?
        .consumed());
    Ok(())
}
#[tokio::test]
async fn nmv2_all_refused_pages_charge_slice_and_resume_from_final_refusal() -> ResultTest {
    let inbox = new_inbox()?;
    let gaps = (0..64).map(gap).collect::<Result<Vec<_>, _>>()?;
    let last = gaps.last().ok_or("gap")?.cursor().clone();
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![
                Ok(page(vec![], gaps, true)?),
                Ok(page(vec![], vec![], false)?),
            ],
            ..Default::default()
        },
        limits(1, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(!owner.advance_slice(&signal).await?.live);
    let slice = owner.advance_slice(&signal).await?;
    assert!(!slice.live);
    assert_eq!(slice.rows, 64);
    assert_eq!(counts(&inbox)?, (0, 1));
    assert!(owner.advance_slice(&signal).await?.live);
    assert_eq!(owner.validated_checkpoint()?, Some(&last));
    assert!(!owner.port.calls().iter().any(|c| matches!(
        c,
        PortCall::AcknowledgeDelivery { .. } | PortCall::Dismiss { .. }
    )));
    Ok(())
}
#[tokio::test]
async fn nmv2_progress_write_failure_never_acknowledges_or_skips_healthy_mail() -> ResultTest {
    let inbox = new_inbox()?;
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(
                vec![message("healthy", "bind-1", MailAttention::Wake)?],
                vec![gap(1)?],
                false,
            )?)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let _fault = commit_fault::CommitFault::install(
        inbox.lock().map_err(|_| "inbox")?.store_mut(),
        rusqlite::hooks::Action::SQLITE_INSERT,
        "event",
        0,
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(owner.advance_slice(&signal).await.is_err());
    assert_eq!(counts(&inbox)?, (0, 0));
    assert!(owner.source_after.is_none());
    assert!(!owner.is_live());
    assert!(!owner
        .port
        .calls()
        .iter()
        .any(|c| matches!(c, PortCall::AcknowledgeDelivery { .. })));
    Ok(())
}

#[tokio::test]
async fn nmv2_warm_echo_mismatch_and_invalid_cursor_never_fall_back_cold() -> ResultTest {
    for echo_mismatch in [true, false] {
        let inbox = new_inbox()?;
        let gap = gap(1)?;
        let last = gap.cursor().clone();
        let mut first = owner(
            inbox.clone(),
            TransportScript {
                open: vec![Ok(bound("bind-1")?)],
                subscribe: vec![Ok(subscribed(None)?)],
                pages: vec![Ok(page(vec![], vec![gap], false)?)],
                ..Default::default()
            },
            limits(8, 512),
        )?;
        let (_cancel, signal) = watch::channel(None);
        assert!(first.advance_slice(&signal).await?.live);
        drop(first);
        let invalid = TransportFailure::CursorInvalid(CursorInvalidation::new(
            CursorInvalidReason::WrongProtocol,
            CursorRecovery::ReplayFrom(SafeReplayOrigin::try_new("retained-origin/v1")?),
        ));
        let mut resumed = owner(
            inbox.clone(),
            TransportScript {
                open: vec![Ok(bound("bind-2")?)],
                subscribe: vec![Ok(subscribed(if echo_mismatch {
                    None
                } else {
                    Some(last)
                })?)],
                pages: if echo_mismatch {
                    vec![]
                } else {
                    vec![Err(invalid)]
                },
                ..Default::default()
            },
            limits(8, 512),
        )?;
        let outcome = resumed.advance_slice(&signal).await;
        if echo_mismatch {
            assert!(matches!(outcome, Err(RemoteMailboxError::ForeignReceipt)));
        } else {
            assert!(matches!(outcome, Err(RemoteMailboxError::Port(_))));
        }
        assert!(!resumed.is_live());
        assert_eq!(
            resumed
                .port
                .calls()
                .iter()
                .filter(|c| matches!(c, PortCall::SubscribeAndSnapshot { .. }))
                .count(),
            1
        );
        assert!(!resumed
            .port
            .calls()
            .iter()
            .any(|c| matches!(c, PortCall::SubscribeAndSnapshot { cold: true })));
    }
    Ok(())
}
#[tokio::test]
async fn nmv2_foreign_page_and_v1_receipts_cannot_enter_v2_progress() -> ResultTest {
    let inbox = new_inbox()?;
    let mut foreign = message("foreign", "bind-other", MailAttention::Wake)?;
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(page(vec![foreign.clone()], vec![gap(1)?], false)?)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(matches!(
        owner.advance_slice(&signal).await,
        Err(RemoteMailboxError::ForeignReceipt)
    ));
    assert_eq!(counts(&inbox)?, (0, 0));
    foreign = message("legacy", "bind-1", MailAttention::Quiet)?;
    inbox
        .lock()
        .map_err(|_| "inbox")?
        .store_mut()
        .admit_remote_mailbox(&scope()?, &input(&scope()?, &foreign))?;
    let mut other = self::owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    assert!(other.advance_slice(&signal).await.is_err());
    assert!(!other.port.calls().iter().any(|c| matches!(
        c,
        PortCall::SubscribeAndSnapshot { .. } | PortCall::AcknowledgeDelivery { .. }
    )));
    Ok(())
}

#[tokio::test]
async fn nmv2_partial_recovery_budget_yields_without_losing_its_prefix() -> ResultTest {
    use grapt_store::mailbox::remote::progress::RecordMailPage;
    let inbox = new_inbox()?;
    let scope = scope()?;
    let first = gap(1)?;
    let second = gap(2)?;
    {
        let mut held = inbox.lock().map_err(|_| "inbox")?;
        let store = held.store_mut();
        store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
        for (previous, gap) in [(None, &first), (Some(first.cursor()), &second)] {
            store.record_remote_mail_page(RecordMailPage {
                scope: &scope,
                protocol: MailTransportVersion::V2,
                previous,
                checkpoint: gap.cursor(),
                messages: &[],
                gaps: std::slice::from_ref(gap),
                decode_failures: &[],
                created_ms: 17,
            })?;
        }
    }
    let mut owner = owner(inbox.clone(), TransportScript::default(), limits(8, 512))?;
    let (_cancel, signal) = watch::channel(None);
    let mut slice = RemoteMailboxSlice::default();
    // A fact that cannot fit even a fresh slice refuses, rather than spinning.
    assert!(matches!(
        owner.recover_progress(&signal, 64, 1, &mut slice).await,
        Err(RemoteMailboxError::SliceTooSmall)
    ));
    owner
        .recover_progress(&signal, 64, 262144, &mut slice)
        .await?;
    let retained = owner.progress_after;
    assert_eq!(owner.checkpoint_candidate.as_ref(), Some(first.cursor()));
    assert!(!owner.recovery_done);
    // Only the unused tail is too small. Yield with both coordinates intact.
    owner.recover_progress(&signal, 64, 1, &mut slice).await?;
    assert_eq!(owner.progress_after, retained);
    assert_eq!(owner.checkpoint_candidate.as_ref(), Some(first.cursor()));
    assert!(!owner.recovery_done);
    assert_eq!(
        slice.pages,
        owner.limits().max_pages_per_runtime_slice().get()
    );
    let mut fresh = RemoteMailboxSlice::default();
    owner
        .recover_progress(&signal, 64, 262144, &mut fresh)
        .await?;
    assert!(owner.recovery_done);
    assert_eq!(owner.checkpoint_candidate.as_ref(), Some(second.cursor()));
    assert_eq!(owner.recorded_integrity_gaps(), 2);
    assert_eq!(counts(&inbox)?, (0, 2));
    assert!(owner.port.calls().is_empty());
    Ok(())
}

#[tokio::test]
#[expect(clippy::too_many_lines, reason = "legacy")]
async fn nmv2_auxiliary_budget_preserves_partial_prefix_and_fresh_refusal() -> ResultTest {
    use grapt_store::mailbox::remote::progress::RecordMailPage;
    let inbox = new_inbox()?;
    let scope = scope()?;
    let gap = gap(70)?;
    let old = input(&scope, &message("aux-old", "bind-1", MailAttention::Wake)?);
    let mut successor = input(&scope, &message("aux-new", "bind-1", MailAttention::Wake)?);
    successor.body = grapt_types::MailboxBody::new("X".repeat(60000))?;
    successor.supersedes = Some(old.source_message.clone());
    let acks;
    let short_budget;
    {
        let mut held = inbox.lock().map_err(|_| "inbox")?;
        let store = held.store_mut();
        store.prepare_remote_mail_progress(&scope, MailTransportVersion::V2)?;
        store.record_remote_mail_page(RecordMailPage {
            scope: &scope,
            protocol: MailTransportVersion::V2,
            previous: None,
            checkpoint: gap.cursor(),
            messages: &[],
            gaps: std::slice::from_ref(&gap),
            decode_failures: &[],
            created_ms: 17,
        })?;
        let mut original = store.record_remote_mail_page(RecordMailPage {
            scope: &scope,
            protocol: MailTransportVersion::V2,
            previous: Some(gap.cursor()),
            checkpoint: &old.source_cursor,
            messages: std::slice::from_ref(&old),
            gaps: &[],
            decode_failures: &[],
            created_ms: 17,
        })?;
        original.extend(store.record_remote_mail_page(RecordMailPage {
            scope: &scope,
            protocol: MailTransportVersion::V2,
            previous: Some(&old.source_cursor),
            checkpoint: &successor.source_cursor,
            messages: std::slice::from_ref(&successor),
            gaps: &[],
            decode_failures: &[],
            created_ms: 17,
        })?);
        acks = original;
        let gap_page = store
            .remote_mail_progress_page(&scope, MailTransportVersion::V2, None, 64, 262144)?
            .ok_or("gap")?;
        let page = store
            .remote_mail_progress_page(
                &scope,
                MailTransportVersion::V2,
                Some(gap_page.recorded.seq),
                64,
                262144,
            )?
            .ok_or("old")?;
        let original = usize::try_from(store.connection().query_row::<i64, _, _>(
            "SELECT length(record_bytes) FROM mailbox_message WHERE supersedes IS NULL",
            [],
            |r| r.get(0),
        )?)?;
        short_budget = serde_json::to_vec(&page.recorded.data)?.len() + original;
    }
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            acks: acks.iter().map(|ack| Ok(ack_receipt(ack))).collect(),
            ..Default::default()
        },
        limits(8, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    owner.ensure_open(&signal).await?;
    let mut partial = RemoteMailboxSlice::default();
    owner
        .recover_progress(&signal, 64, 262144, &mut partial)
        .await?;
    let position = owner.progress_after;
    owner
        .recover_progress(&signal, 64, short_budget, &mut partial)
        .await?;
    assert_eq!(owner.progress_after, position);
    assert_eq!(owner.checkpoint_candidate.as_ref(), Some(gap.cursor()));
    assert_eq!(
        partial.pages,
        owner.limits().max_pages_per_runtime_slice().get()
    );
    let mut fresh = RemoteMailboxSlice::default();
    assert!(matches!(
        owner
            .recover_progress(&signal, 64, short_budget, &mut fresh)
            .await,
        Err(RemoteMailboxError::SliceTooSmall)
    ));
    assert_eq!(owner.progress_after, position);
    assert!(!owner.recovery_done);
    owner
        .recover_progress(&signal, 64, 262144, &mut fresh)
        .await?;
    assert!(fresh.charged_bytes > short_budget + 60000);
    assert_eq!(
        owner.checkpoint_candidate.as_ref(),
        Some(&old.source_cursor)
    );
    owner
        .recover_progress(&signal, 64, 262144, &mut fresh)
        .await?;
    assert!(owner.recovery_done);
    assert_eq!(
        owner.checkpoint_candidate.as_ref(),
        Some(&successor.source_cursor)
    );
    assert_eq!(counts(&inbox)?, (2, 3));
    Ok(())
}

#[tokio::test]
async fn local_unknown_kind_crash_recovery_retains_cursor_without_ack_authority() -> ResultTest {
    let inbox = new_inbox()?;
    let message = message("healthy", "bind-1", MailAttention::Wake)?;
    let ack = predicted_ack(&message)?;
    let cursor = ExclusiveSourceCursor::try_new("opaque-local-unknown-kind")?;
    let failure = MailDecodeFailure::unknown_kind(cursor.clone()).ok_or("failure")?;
    let source = RemoteMailPage::v2_page_with_decode_failures(
        vec![message],
        vec![],
        vec![failure],
        None,
        Some(cursor.clone()),
    )
    .ok_or("page")?;
    let mut first = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(source)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    first.port.lose_next_ack_response();
    let (_cancel, signal) = watch::channel(None);
    assert!(first.advance_slice(&signal).await.is_err());
    assert_eq!(counts(&inbox)?, (1, 1));
    drop(first);
    let mut resumed = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-2")?)],
            subscribe: vec![Ok(subscribed(Some(cursor.clone()))?)],
            pages: vec![Ok(page(vec![], vec![], false)?)],
            acks: vec![Ok(ack_receipt(&ack))],
            ..Default::default()
        },
        limits(8, 512),
    )?;
    assert!(resumed.advance_slice(&signal).await?.live);
    assert_eq!(resumed.recorded_decode_failures(), 1);
    assert_eq!(resumed.recorded_integrity_gaps(), 0);
    assert_eq!(resumed.validated_checkpoint()?, Some(&cursor));
    assert_eq!(counts(&inbox)?, (1, 1));
    assert_eq!(
        resumed
            .port
            .calls()
            .iter()
            .filter(|c| matches!(c, PortCall::AcknowledgeDelivery { .. }))
            .count(),
        1
    );
    let batch = resumed.prepare_notifications(
        &"ses_notification".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    assert_eq!(batch.batch.ok_or("batch")?.count(), 1);
    Ok(())
}

#[tokio::test]
async fn local_unknown_only_page_is_bounded_progress_and_has_no_notification() -> ResultTest {
    let inbox = new_inbox()?;
    let failures = (0..64)
        .map(|n| -> Result<_, Box<dyn std::error::Error>> {
            Ok(
                MailDecodeFailure::unknown_kind(ExclusiveSourceCursor::try_new(format!(
                    "opaque-kind-{n}"
                ))?)
                .ok_or("failure")?,
            )
        })
        .collect::<Result<Vec<_>, _>>()?;
    let last = failures.last().ok_or("last")?.cursor().clone();
    let source = RemoteMailPage::v2_page_with_decode_failures(
        vec![],
        vec![],
        failures,
        Some(last.clone()),
        Some(last.clone()),
    )
    .ok_or("page")?;
    let mut owner = owner(
        inbox.clone(),
        TransportScript {
            open: vec![Ok(bound("bind-1")?)],
            subscribe: vec![Ok(subscribed(None)?)],
            pages: vec![Ok(source), Ok(page(vec![], vec![], false)?)],
            ..Default::default()
        },
        limits(1, 512),
    )?;
    let (_cancel, signal) = watch::channel(None);
    assert!(!owner.advance_slice(&signal).await?.live);
    let slice = owner.advance_slice(&signal).await?;
    assert_eq!(slice.rows, 64);
    assert!(!slice.live);
    assert!(owner.advance_slice(&signal).await?.live);
    assert_eq!(owner.recorded_decode_failures(), 64);
    assert_eq!(owner.validated_checkpoint()?, Some(&last));
    assert_eq!(counts(&inbox)?, (0, 1));
    assert!(!owner.port.calls().iter().any(|c| matches!(
        c,
        PortCall::AcknowledgeDelivery { .. } | PortCall::Dismiss { .. }
    )));
    let scan = owner.prepare_notifications(
        &"ses_notification".parse()?,
        grapt_types::StepNumber::new(1).ok_or("step")?,
        notification_binding()?,
    )?;
    assert!(scan.batch.is_none());
    Ok(())
}

#[tokio::test]
async fn interleaved_capture_orders_mail_by_admission() -> ResultTest {
    let one = message("one", "bind-1", MailAttention::Wake)?;
    let two = message("two", "bind-1", MailAttention::Wake)?;
    let acks = vec![
        Ok(ack_receipt(&predicted_ack(&two)?)),
        Ok(ack_receipt(&predicted_ack(&one)?)),
    ];
    let script = TransportScript {
        open: vec![Ok(bound("bind-1")?)],
        subscribe: vec![Ok(subscribed(None)?)],
        pages: vec![Ok(page(vec![one, two], vec![], false)?)],
        acks,
        ..Default::default()
    };
    let mut owner = owner(new_inbox()?, script, limits(8, 512))?.with_interleaved_delivery()?;
    let (_cancel, signal) = watch::channel(None);
    assert!(owner.advance_slice(&signal).await?.live);
    for _ in 0..8 {
        if owner.delivery_work_pending() {
            assert!(owner.advance_slice(&signal).await?.live);
        }
    }
    assert!(!owner.delivery_work_pending());
    let step = grapt_types::StepNumber::new(1).ok_or("step")?;
    let scan =
        owner.prepare_notifications(&"ses_notification".parse()?, step, notification_binding()?)?;
    let attempt = grapt_types::PhysicalAttemptId::new("pat_a").ok_or("attempt")?;
    scan.batch.ok_or("batch")?.presentation(&attempt, step)?;
    Ok(())
}
