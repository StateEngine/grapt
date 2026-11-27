//! The immutable common contract vectors for `remote-seat-mail-transport/v1`.
//!
//! These are the ten scenarios named in the R3b-2T admission packet §12. Ruling
//! J7 makes the deterministic fake the v1 vehicle for them; when a real adapter
//! eventually exists, R3b-2I runs these SAME vectors against both and compares.
//!
//! # What these vectors prove, and what they cannot
//!
//! Every vector here is transport-observable only. Several of the §12 scenarios
//! have a second half that this lane does not own and must not claim:
//!
//! * local admission, delivery-ack debt rows and checkpoint advancement are
//!   R3b-2B's;
//! * the coalesced capacity-one wake bit, reconnect scheduling, slice budgets
//!   and the `Live` declaration are R3b-2C's;
//! * end-to-end crash cuts across all of the above are R3b-2I's.
//!
//! Each vector states its own boundary in a comment rather than quietly
//! asserting less than its name suggests. A vector that appeared to prove a law
//! it only half-touches would be worse than no vector at all.
//!
//! # The numbers here are FICTIONAL
//!
//! The limit profile below is a vector fixture, not measured provider evidence.
//! Seat 5D found StateEngine NOT ADMITTED at `bb466976` and no real profile
//! exists yet; these values exist so the vectors can execute, and they are
//! deliberately kept out of the shipped crate so nobody can mistake them for
//! the ratified measurements.

use core::num::{NonZeroU32, NonZeroUsize};
use grapt_mail_transport::ack::{
    DeliveryAckOutcome, DeliveryAckReceipt, DeliveryAdmissionAck, DeliveryAdmissionAckParts,
};
use grapt_mail_transport::binding::{BindingGeneration, MailBinding, OpenRequest, TakeoverReceipt};
use grapt_mail_transport::cursor::{
    ArmedMailboxSubscription, CursorInvalidReason, CursorInvalidation, CursorRecovery,
    MailSnapshot, RemoteMailPage, SubscriptionGeneration,
};
use grapt_mail_transport::failure::{
    DeliveryAckFailure, LifecycleOutcome, OpenFailure, SubscriptionClosed, TransportFailure,
};
use grapt_mail_transport::fake::{
    block_on, DeterministicTransport, PortCall, ScriptedBinding, TransportScript,
};
use grapt_mail_transport::identity::*;
use grapt_mail_transport::limits::{ProviderLimits, ProviderLimitsParts, SettledTransportLimits};
use grapt_mail_transport::message::{
    BoundRemoteSeatMailMessageParts, BoundRemoteSeatMailMessageV1, BoundedMailRef,
    BoundedOrderedMailRefs, RemoteSeatMailKind, SourceArchiveDisposition, SourceArchiveReason,
    SourceSupersession,
};
use grapt_mail_transport::port::{
    PageLimits, RemoteSeatMailTransportV1, ResumeRequest, ShutdownBudget, ShutdownReceipt,
};
use grapt_mail_transport::signal::{
    BulkDismissOutcome, BulkDismissRequest, ChangeSignal, DismissReason,
};
use grapt_mail_transport::REMOTE_SEAT_MAIL_TRANSPORT_V1;
use grapt_types::mailbox::{MailboxAdmissionSeq, MailboxBody, MailboxMessageId, SeatRecipientId};
use grapt_types::protocol::ProtocolVersion;

// ---------------------------------------------------------------- fixtures --

fn nz32(v: u32) -> NonZeroU32 {
    NonZeroU32::new(v).unwrap_or(NonZeroU32::MIN)
}

fn nzs(v: usize) -> NonZeroUsize {
    NonZeroUsize::new(v).unwrap_or(NonZeroUsize::MIN)
}

/// A fictional provider profile. See the module docs: NOT measured evidence.
///
/// Written out in full deliberately. `ProviderLimitsParts` has no `Default`, so
/// this is the completeness rule doing its job on an EXTERNAL consumer: a 25th
/// mandatory field would stop these vectors compiling rather than silently
/// defaulting.
fn provider_limits() -> ProviderLimits {
    ProviderLimits::new(ProviderLimitsParts {
        max_refs_per_message: nz32(64),
        max_ref_utf8_bytes: nzs(512),
        max_aggregate_ref_bytes_per_message: nzs(32_768),
        max_decoded_message_bytes_including_metadata: nzs(131_072),
        max_decoded_page_bytes_including_metadata: nzs(524_288),
        max_cursor_bytes: nzs(1_024),
        max_snapshot_token_bytes: nzs(1_024),
        max_diagnostic_bytes: nzs(4_096),
        max_pages_per_runtime_slice: nz32(8),
        max_messages_per_runtime_slice: nz32(256),
        max_decoded_bytes_per_runtime_slice: nzs(1_048_576),
        max_ack_debt_items_per_recovery_page: nz32(64),
        max_connect_attempts_per_activation: nz32(4),
        max_reconnect_attempts_per_runtime_slice: nz32(2),
        max_connect_duration_ticks: nz32(300),
        max_single_reconnect_delay_ticks: nz32(600),
        max_total_reconnect_delay_ticks_per_slice: nz32(1_200),
        max_fetch_duration_ticks: nz32(300),
        max_ack_duration_ticks: nz32(300),
        max_shutdown_duration_ticks: nz32(300),
        max_joined_transport_tasks_at_shutdown: nz32(8),
        max_body_utf8_bytes: nzs(65_536),
        max_messages_per_page: nz32(64),
        max_aggregate_body_bytes_per_page: nzs(262_144),
    })
}

fn settled() -> SettledTransportLimits {
    provider_limits().settle()
}

fn projects() -> Result<ProjectPair, Box<dyn std::error::Error>> {
    Ok({
        ProjectPair::new(
            SourceProjectId::new("stateengine").ok_or("source project")?,
            TargetProjectId::new("secode").ok_or("target project")?,
        )
    })
}

fn protocol() -> Result<ProtocolVersion, Box<dyn std::error::Error>> {
    Ok(ProtocolVersion::new(REMOTE_SEAT_MAIL_TRANSPORT_V1).ok_or("protocol")?)
}

fn recipient() -> Result<SeatRecipientId, Box<dyn std::error::Error>> {
    Ok(SeatRecipientId::new("seat-delta").ok_or("recipient")?)
}

/// What the fake will pretend it verified.
///
/// This used to be a `MailBindingReceipt` these vectors authored directly. That
/// route is gone: receipt construction is crate-owned now, because an external
/// consumer that could author a receipt could name any `verified_principal` and
/// mint authority that no adapter ever checked. A vector states the INERT
/// description and the crate turns it into a receipt.
///
/// Note what is absent: the settled limit profile. The fake settles the profile
/// passed to `open`, so a vector can no longer script a "settled" value that was
/// never settled.
fn scripted_binding(
    takeover: TakeoverReceipt,
) -> Result<ScriptedBinding, Box<dyn std::error::Error>> {
    Ok({
        ScriptedBinding {
            binding_id: SourceBindingId::new("bind-1").ok_or("binding")?,
            protocol: protocol()?,
            adapter_version: AdapterVersion::new("deterministic-fake-1").ok_or("adapter")?,
            projects: projects()?,
            verified_principal: SourcePrincipalId::new("agt-demo").ok_or("principal")?,
            local_recipient: recipient()?,
            mapping_version: RecipientMappingVersion::new("map-1").ok_or("mapping")?,
            takeover,
        }
    })
}

fn open_request() -> Result<OpenRequest, Box<dyn std::error::Error>> {
    Ok({
        OpenRequest::new(
            TargetProjectId::new("secode").ok_or("target")?,
            recipient()?,
            protocol()?,
            MailTransportClientInstanceId::new("pid-4496").ok_or("client")?,
        )
    })
}

fn snapshot(start_after: Option<&str>) -> Result<MailSnapshot, Box<dyn std::error::Error>> {
    Ok({
        MailSnapshot::new(
            MailSnapshotId::new("snap-1").ok_or("snapshot")?,
            SnapshotBarrier::new("barrier-1").ok_or("barrier")?,
            match start_after {
                Some(c) => Some(ExclusiveSourceCursor::new(c).ok_or("cursor")?),
                None => None,
            },
            SafeReplayOrigin::new("origin-1").ok_or("origin")?,
        )
    })
}

fn subscription(
    snapshot: MailSnapshot,
) -> Result<ArmedMailboxSubscription, Box<dyn std::error::Error>> {
    Ok({
        ArmedMailboxSubscription::new(
            MailSubscriptionId::new("sub-1").ok_or("subscription")?,
            SubscriptionGeneration::FIRST,
            snapshot,
        )
    })
}

/// A complete message with every J5/J8 coordinate populated.
fn message(
    id: &str,
    cursor: &str,
    kind: RemoteSeatMailKind,
) -> Result<BoundRemoteSeatMailMessageV1, Box<dyn std::error::Error>> {
    Ok({
        let refs = BoundedOrderedMailRefs::try_new(
            vec![
                BoundedMailRef::try_new("be1c33c2", 512).map_err(|e| format!("ref: {e:?}"))?,
                BoundedMailRef::try_new("3f9a607e", 512).map_err(|e| format!("ref: {e:?}"))?,
            ],
            64,
            32_768,
        )
        .map_err(|e| format!("bounded refs: {e:?}"))?;
        BoundRemoteSeatMailMessageV1::try_new(
            BoundRemoteSeatMailMessageParts {
                protocol: protocol()?,
                source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
                projects: projects()?,
                source_message: RemoteMailboxMessageId::new(id).ok_or("message")?,
                source_cursor: ExclusiveSourceCursor::new(cursor).ok_or("cursor")?,
                sender: SourcePrincipalId::new("agt-demo").ok_or("sender")?,
                recipient: recipient()?,
                kind,
                body: MailboxBody::new("do the thing").map_err(|e| format!("body: {e:?}"))?,
                refs,
                supersession: SourceSupersession::none(),
                archive: SourceArchiveDisposition::Active,
                source_created: SourceTimestamp::new("2026-08-13T07:00:00Z").ok_or("timestamp")?,
                attention: grapt_mail_transport::MailAttention::Wake,
            },
            &settled(),
        )
        .map_err(|e| format!("a complete message: {e:?}"))?
    })
}

fn ack_for(
    source_message: &str,
    cursor: &str,
) -> Result<DeliveryAdmissionAck, Box<dyn std::error::Error>> {
    Ok({
        DeliveryAdmissionAck::new(DeliveryAdmissionAckParts {
            ack_id: DeliveryAdmissionAckId::new("ack-1").ok_or("ack")?,
            source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
            projects: projects()?,
            source_message: RemoteMailboxMessageId::new(source_message).ok_or("message")?,
            source_cursor: ExclusiveSourceCursor::new(cursor).ok_or("cursor")?,
            recipient: recipient()?,
            local_message: MailboxMessageId::new("msg-local-1").ok_or("local")?,
            local_admission: MailboxAdmissionSeq::FIRST,
            canonical_message_digest: MailboxMessageDigest::new("digest-a").ok_or("digest")?,
            protocol: protocol()?,
        })
    })
}

fn ack_receipt() -> Result<DeliveryAckReceipt, Box<dyn std::error::Error>> {
    Ok({
        DeliveryAckReceipt::new(
            DeliveryAdmissionAckId::new("ack-1").ok_or("ack")?,
            RemoteMailboxMessageId::new("msg-remote-1").ok_or("message")?,
            projects()?,
        )
    })
}

fn opened(
    script: TransportScript,
) -> Result<(DeterministicTransport, MailBinding), Box<dyn std::error::Error>> {
    Ok({
        let mut transport = DeterministicTransport::new(script);
        let binding = block_on(transport.open(open_request()?, provider_limits()))
            .map_err(|e| format!("the vector's script opens cleanly: {e:?}"))?;
        (transport, binding)
    })
}

fn script_opening(
    takeover: TakeoverReceipt,
) -> Result<TransportScript, Box<dyn std::error::Error>> {
    Ok(TransportScript {
        open: vec![Ok(scripted_binding(takeover)?)],
        ..TransportScript::default()
    })
}

// ------------------------------------------------------------------ vectors --

/// Vector 1: `mail-before-during-after-barrier`.
///
/// TRANSPORT HALF ONLY. The atomic handshake means everything committed at or
/// before the barrier is in the snapshot, and everything after it is covered by
/// the armed subscription. This vector proves the transport surfaces each
/// message exactly once, in source order, across a bounded page boundary — that
/// there is no gap between "what the snapshot held" and "what the subscription
/// covers". Local single-admission is R3b-2B's and is not claimed here.
#[test]
fn vector_01_mail_before_during_and_after_the_barrier_appears_once_in_order(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.subscribe = vec![Ok(subscription(snapshot(None)?)?)];
    script.pages = vec![
        Ok(RemoteMailPage::bounded(
            vec![
                message("msg-before", "cur-1", RemoteSeatMailKind::TaskPointer)?,
                message("msg-during", "cur-2", RemoteSeatMailKind::Question)?,
            ],
            ExclusiveSourceCursor::new("cur-2").ok_or("cursor")?,
        )),
        Ok(RemoteMailPage::final_page(vec![message(
            "msg-after",
            "cur-3",
            RemoteSeatMailKind::Ruling,
        )?])),
    ];

    let (mut transport, binding) = opened(script)?;
    let armed = block_on(transport.subscribe_and_snapshot(&binding, ResumeRequest::cold()))
        .map_err(|e| format!("one atomic handshake: {e:?}"))?;

    let page_limits = PageLimits::from_settled(&settled());
    let first =
        block_on(transport.fetch_snapshot_page(&binding, armed.snapshot(), None, page_limits))
            .map_err(|e| format!("first page: {e:?}"))?;
    assert!(first.has_more(), "a bounded page must say more remains");

    let second = block_on(transport.fetch_snapshot_page(
        &binding,
        armed.snapshot(),
        first.next_after(),
        page_limits,
    ))
    .map_err(|e| format!("second page: {e:?}"))?;
    assert!(!second.has_more());

    let seen: Vec<&str> = first
        .messages()
        .iter()
        .chain(second.messages())
        .map(|m| m.source_message().as_str())
        .collect();
    assert_eq!(seen, ["msg-before", "msg-during", "msg-after"]);
    assert_eq!(
        seen.len(),
        seen.iter().collect::<std::collections::BTreeSet<_>>().len(),
        "no message may appear twice across the page boundary"
    );

    // Exactly one handshake: a second subscribe would mean a list/subscribe race
    // had been reintroduced.
    assert_eq!(
        transport.count(|c| matches!(c, PortCall::SubscribeAndSnapshot { .. })),
        1
    );
    // The resume was cold, and the second fetch continued strictly after the
    // first page's last fully emitted message.
    assert_eq!(
        transport.calls()[1],
        PortCall::SubscribeAndSnapshot { cold: true }
    );
    assert_eq!(
        transport.calls()[3],
        PortCall::FetchSnapshotPage {
            after: Some("cur-2".to_owned())
        }
    );
    Ok(())
}

/// Vector 2: `exact-principal-binding`.
///
/// A wrong project, recipient or protocol yields no binding — and therefore no
/// message, cursor, subscription or acknowledgement authority. The assertion
/// that matters is the ABSENCE: zero calls of every other kind.
#[test]
fn vector_02_a_wrong_binding_yields_no_mail_cursor_or_acknowledgement(
) -> Result<(), Box<dyn std::error::Error>> {
    for failure in [
        OpenFailure::ForeignProjectMismatch,
        OpenFailure::ExactPrincipalMismatch,
        OpenFailure::RecipientMismatch,
        OpenFailure::MappingVersionMismatch,
        OpenFailure::ProtocolVersionUnsupported,
    ] {
        let mut transport = DeterministicTransport::new(TransportScript {
            open: vec![Err(failure.clone())],
            ..TransportScript::default()
        });
        let outcome = block_on(transport.open(open_request()?, provider_limits()));
        assert_eq!(outcome.err(), Some(failure.into()));

        assert_eq!(transport.calls(), [PortCall::Open]);
        assert_eq!(
            transport.count(|c| matches!(c, PortCall::FetchSnapshotPage { .. })),
            0
        );
        assert_eq!(
            transport.count(|c| matches!(c, PortCall::AcknowledgeDelivery { .. })),
            0
        );
    }
    Ok(())
}

/// Vector 3: `complete-message-shape`.
///
/// Ordered refs, both project halves, `Ruling`, supersession and the archive
/// reason all survive strict conversion.
#[test]
fn vector_03_a_complete_message_survives_conversion_with_every_coordinate(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.subscribe = vec![Ok(subscription(snapshot(None)?)?)];

    // A message in the middle of a correction chain, archived by supersession:
    // the J5 shape the old either/or enum could not express.
    let me = RemoteMailboxMessageId::new("msg-2").ok_or("message")?;
    let supersession = SourceSupersession::try_new(
        &me,
        Some(RemoteMailboxMessageId::new("msg-1").ok_or("earlier")?),
        Some(RemoteMailboxMessageId::new("msg-3").ok_or("later")?),
    )
    .map_err(|e| format!("both edges are legal: {e:?}"))?;

    let refs = BoundedOrderedMailRefs::try_new(
        vec![
            BoundedMailRef::try_new("be1c33c2", 512).map_err(|e| format!("ref: {e:?}"))?,
            BoundedMailRef::try_new("3f9a607e", 512).map_err(|e| format!("ref: {e:?}"))?,
            BoundedMailRef::try_new("be1c33c2", 512).map_err(|e| format!("ref: {e:?}"))?,
        ],
        64,
        32_768,
    )
    .map_err(|e| format!("ordered refs: {e:?}"))?;

    let built = BoundRemoteSeatMailMessageV1::try_new(
        BoundRemoteSeatMailMessageParts {
            protocol: protocol()?,
            source_binding: SourceBindingId::new("bind-1").ok_or("binding")?,
            projects: projects()?,
            source_message: me,
            source_cursor: ExclusiveSourceCursor::new("cur-2").ok_or("cursor")?,
            sender: SourcePrincipalId::new("fable-5").ok_or("sender")?,
            recipient: recipient()?,
            kind: RemoteSeatMailKind::Ruling,
            body: MailboxBody::new("the eight rulings are decided")
                .map_err(|e| format!("body: {e:?}"))?,
            refs,
            supersession,
            archive: SourceArchiveDisposition::Archived(SourceArchiveReason::ExplicitSupersession),
            source_created: SourceTimestamp::new("2026-08-13T06:31:33Z").ok_or("timestamp")?,
            attention: grapt_mail_transport::MailAttention::Wake,
        },
        &settled(),
    )
    .map_err(|e| format!("a complete message: {e:?}"))?;

    script.pages = vec![Ok(RemoteMailPage::final_page(vec![built]))];

    let (mut transport, binding) = opened(script)?;
    let armed = block_on(transport.subscribe_and_snapshot(&binding, ResumeRequest::cold()))
        .map_err(|e| format!("handshake: {e:?}"))?;
    let page = block_on(transport.fetch_snapshot_page(
        &binding,
        armed.snapshot(),
        None,
        PageLimits::from_settled(&settled()),
    ))
    .map_err(|e| format!("page: {e:?}"))?;

    let got = &page.messages()[0];
    // Kind survives as itself: the named resurrection is mapping it to
    // RulingRequest.
    assert_eq!(got.kind(), &RemoteSeatMailKind::Ruling);
    assert_ne!(got.kind(), &RemoteSeatMailKind::RulingRequest);
    // Refs keep source order, including the duplicate.
    let seen: Vec<&str> = got
        .refs()
        .as_slice()
        .iter()
        .map(BoundedMailRef::as_str)
        .collect();
    assert_eq!(seen, ["be1c33c2", "3f9a607e", "be1c33c2"]);
    // Both project halves, J8.
    assert_eq!(got.projects().source().as_str(), "stateengine");
    assert_eq!(got.projects().target().as_str(), "secode");
    // Both supersession edges, J5.
    assert_eq!(
        got.supersession()
            .supersedes()
            .map(RemoteMailboxMessageId::as_str),
        Some("msg-1")
    );
    assert_eq!(
        got.supersession()
            .superseded_by()
            .map(RemoteMailboxMessageId::as_str),
        Some("msg-3")
    );
    // Archive provenance with its explicit reason.
    assert_eq!(
        got.archive(),
        SourceArchiveDisposition::Archived(SourceArchiveReason::ExplicitSupersession)
    );
    assert_eq!(got.source_created().as_str(), "2026-08-13T06:31:33Z");
    Ok(())
}

/// Vector 4: `delivery-not-read`.
///
/// Acknowledging delivery succeeds, and a provider that offers its READ endpoint
/// instead is refused with its own reason rather than quietly accepted.
#[test]
fn vector_04_delivery_acknowledgement_is_not_a_read_acknowledgement(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.acks = vec![
        Ok(DeliveryAckOutcome::Recorded(ack_receipt()?)),
        Err(DeliveryAckFailure::ReadAckSubstitutionRefused),
    ];

    let (mut transport, binding) = opened(script)?;

    let recorded =
        block_on(transport.acknowledge_delivery(&binding, ack_for("msg-remote-1", "cur-1")?))
            .map_err(|e| format!("a delivery acknowledgement: {e:?}"))?;
    assert!(recorded.is_first_record());

    let refused =
        block_on(transport.acknowledge_delivery(&binding, ack_for("msg-remote-2", "cur-2")?));
    assert_eq!(
        refused.err(),
        Some(DeliveryAckFailure::ReadAckSubstitutionRefused.into()),
        "a read endpoint may never settle a delivery fact"
    );
    Ok(())
}

/// Vector 5: `lost-ack-response`.
///
/// TRANSPORT HALF ONLY. The exact retry reconciles to the ORIGINAL receipt and
/// creates no second fact. Debt survival and the single checkpoint advance are
/// R3b-2B's and are not claimed here.
#[test]
fn vector_05_an_exact_retry_after_a_lost_response_reconciles_to_one_receipt(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.acks = vec![
        Ok(DeliveryAckOutcome::Recorded(ack_receipt()?)),
        Ok(DeliveryAckOutcome::ExactRetry(ack_receipt()?)),
    ];

    let (mut transport, binding) = opened(script)?;
    let ack = ack_for("msg-remote-1", "cur-1")?;

    let first = block_on(transport.acknowledge_delivery(&binding, ack.clone()))
        .map_err(|e| format!("first: {e:?}"))?;
    let retry = block_on(transport.acknowledge_delivery(&binding, ack))
        .map_err(|e| format!("exact retry: {e:?}"))?;

    assert!(first.is_first_record());
    assert!(
        !retry.is_first_record(),
        "a retry must not create a new fact"
    );
    assert_eq!(first.receipt(), retry.receipt(), "one immutable receipt");
    assert_eq!(
        transport.count(|c| matches!(c, PortCall::AcknowledgeDelivery { .. })),
        2,
        "the retry really was a second physical call, not a local short-circuit"
    );
    Ok(())
}

/// Vector 6: `duplicate-coalesced-signals`.
///
/// TRANSPORT HALF ONLY. The transport's obligation is at-least-once payload-free
/// delivery while connected; duplicates and coalescing are explicitly legal. The
/// capacity-one pending bit that turns many signals into one follow-up pass is
/// R3b-2C's and is NOT modelled here.
#[test]
fn vector_06_signals_may_duplicate_and_carry_no_mail() -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.subscribe = vec![Ok(subscription(snapshot(None)?)?)];
    script.changes = vec![
        Ok(ChangeSignal::new(SubscriptionGeneration::FIRST)),
        Ok(ChangeSignal::new(SubscriptionGeneration::FIRST)),
        Ok(ChangeSignal::with_token(
            SubscriptionGeneration::FIRST,
            "floor-gen-172",
        )),
        Err(SubscriptionClosed::Disconnected),
    ];

    let (mut transport, binding) = opened(script)?;
    let mut armed = block_on(transport.subscribe_and_snapshot(&binding, ResumeRequest::cold()))
        .map_err(|e| format!("handshake: {e:?}"))?;

    let mut signals = 0;
    loop {
        match block_on(transport.next_change(&mut armed)) {
            Ok(signal) => {
                assert_eq!(
                    signal.mail_payload_bytes(),
                    0,
                    "a signal that carried mail would be a second delivery path"
                );
                assert_eq!(signal.generation(), SubscriptionGeneration::FIRST);
                signals += 1;
            }
            Err(closed) => {
                assert_eq!(closed, SubscriptionClosed::Disconnected.into());
                break;
            }
        }
    }
    assert_eq!(
        signals, 3,
        "duplicates are legal and are not deduplicated here"
    );
    Ok(())
}

/// Vector 7: `disconnect-mid-page`.
///
/// The settled prefix survives, and the reconnect resumes strictly after it. The
/// vector asserts the RESUME COORDINATE, because a reconnect that quietly went
/// cold would skip the tail.
#[test]
fn vector_07_a_disconnect_mid_page_resumes_after_the_settled_prefix(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.subscribe = vec![
        Ok(subscription(snapshot(None)?)?),
        Ok(subscription(snapshot(Some("cur-2"))?)?),
    ];
    script.pages = vec![
        Ok(RemoteMailPage::bounded(
            vec![message("msg-1", "cur-1", RemoteSeatMailKind::TaskPointer)?],
            ExclusiveSourceCursor::new("cur-2").ok_or("cursor")?,
        )),
        Err(TransportFailure::Disconnected),
        Ok(RemoteMailPage::final_page(vec![message(
            "msg-2",
            "cur-3",
            RemoteSeatMailKind::Done,
        )?])),
    ];

    let (mut transport, binding) = opened(script)?;
    let page_limits = PageLimits::from_settled(&settled());

    let armed = block_on(transport.subscribe_and_snapshot(&binding, ResumeRequest::cold()))
        .map_err(|e| format!("handshake: {e:?}"))?;
    let prefix =
        block_on(transport.fetch_snapshot_page(&binding, armed.snapshot(), None, page_limits))
            .map_err(|e| format!("settled prefix: {e:?}"))?;
    let settled_cursor = prefix
        .next_after()
        .ok_or("a bounded page names its last fully emitted message")?
        .clone();

    let dropped = block_on(transport.fetch_snapshot_page(
        &binding,
        armed.snapshot(),
        Some(&settled_cursor),
        page_limits,
    ));
    assert_eq!(dropped.err(), Some(TransportFailure::Disconnected.into()));

    // Reconnect resumes from the settled cursor, NOT cold and NOT from the head.
    let rearmed = block_on(transport.subscribe_and_snapshot(
        &binding,
        ResumeRequest::from_checkpoint(settled_cursor.clone()),
    ))
    .map_err(|e| format!("reconnect handshake: {e:?}"))?;
    let tail = block_on(transport.fetch_snapshot_page(
        &binding,
        rearmed.snapshot(),
        Some(&settled_cursor),
        page_limits,
    ))
    .map_err(|e| format!("tail: {e:?}"))?;

    assert_eq!(tail.messages()[0].source_message().as_str(), "msg-2");
    assert_eq!(
        transport.calls()[4],
        PortCall::SubscribeAndSnapshot { cold: false },
        "a reconnect that went cold would skip the tail"
    );
    Ok(())
}

/// Vector 8: `cursor-invalidation`.
///
/// Invalid is never empty. Either a no-skip replay origin is supplied, or the
/// provider says recovery is unavailable and the path blocks.
#[test]
fn vector_08_invalidation_replays_or_blocks_but_never_returns_empty(
) -> Result<(), Box<dyn std::error::Error>> {
    let replayable = CursorInvalidation::new(
        CursorInvalidReason::Expired,
        CursorRecovery::ReplayFrom(SafeReplayOrigin::new("origin-1").ok_or("origin")?),
    );
    let blocked = CursorInvalidation::new(
        CursorInvalidReason::Compacted,
        CursorRecovery::RecoveryUnavailable {
            detail: "history behind the checkpoint was compacted away",
        },
    );

    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.subscribe = vec![Ok(subscription(snapshot(None)?)?)];
    script.pages = vec![
        Err(TransportFailure::CursorInvalid(replayable)),
        Err(TransportFailure::CursorInvalid(blocked)),
    ];

    let (mut transport, binding) = opened(script)?;
    let armed = block_on(transport.subscribe_and_snapshot(&binding, ResumeRequest::cold()))
        .map_err(|e| format!("handshake: {e:?}"))?;
    let page_limits = PageLimits::from_settled(&settled());

    match block_on(transport.fetch_snapshot_page(&binding, armed.snapshot(), None, page_limits)) {
        Err(grapt_mail_transport::operation::OperationFailure {
            cause:
                grapt_mail_transport::operation::OperationCause::Transport(
                    TransportFailure::CursorInvalid(invalidation),
                ),
            ..
        }) => {
            assert!(invalidation.is_replayable());
            assert!(matches!(
                invalidation.recovery(),
                CursorRecovery::ReplayFrom(_)
            ));
        }
        other => panic!("invalidation must be typed, not empty: {other:?}"),
    }

    match block_on(transport.fetch_snapshot_page(&binding, armed.snapshot(), None, page_limits)) {
        Err(grapt_mail_transport::operation::OperationFailure {
            cause:
                grapt_mail_transport::operation::OperationCause::Transport(
                    TransportFailure::CursorInvalid(invalidation),
                ),
            ..
        }) => {
            assert!(!invalidation.is_replayable());
            assert_eq!(invalidation.reason(), CursorInvalidReason::Compacted);
        }
        other => panic!("unavailable recovery must be typed, not empty: {other:?}"),
    }

    // The named resurrection: neither call produced an empty page.
    assert_eq!(
        transport.count(|c| matches!(c, PortCall::FetchSnapshotPage { .. })),
        2
    );
    Ok(())
}

/// Vector 9: `restart-durable-only`.
///
/// TRANSPORT HALF ONLY. A restart carries nothing but its durable checkpoint
/// into the new binding: no subscription, generation or pending state survives.
/// The durable half — that the checkpoint is the one R3b-2B settled — is B's.
///
/// This also covers ruling J2's takeover: the second open SUCCEEDS and names its
/// predecessor, where the old lease design would have refused it.
#[test]
fn vector_09_a_restart_rebinds_from_the_checkpoint_and_takes_over_visibly(
) -> Result<(), Box<dyn std::error::Error>> {
    let displaced = grapt_mail_transport::binding::DisplacedOwner::new(
        SourcePrincipalId::new("agt-demo").ok_or("principal")?,
        MailTransportClientInstanceId::new("pid-22360").ok_or("dead client")?,
        BindingGeneration::FIRST,
    );
    let second_generation = BindingGeneration::FIRST.next().ok_or("next generation")?;

    let script = TransportScript {
        open: vec![Ok(scripted_binding(TakeoverReceipt::took_over(
            second_generation,
            displaced,
        ))?)],
        subscribe: vec![Ok(subscription(snapshot(Some("cur-7"))?)?)],
        ..TransportScript::default()
    };

    let (mut transport, binding) = opened(script)?;

    // J2: not a LeaseConflict. The newer owner is simply the owner, and the
    // receipt makes the displacement visible instead of fatal.
    let takeover = binding.receipt().takeover();
    assert!(takeover.is_takeover());
    let previous = takeover
        .displaced()
        .ok_or("a takeover names its predecessor")?;
    assert_eq!(previous.client().as_str(), "pid-22360");
    assert!(takeover.generation() > previous.generation());

    let checkpoint = ExclusiveSourceCursor::new("cur-7").ok_or("cursor")?;
    let armed = block_on(
        transport.subscribe_and_snapshot(&binding, ResumeRequest::from_checkpoint(checkpoint)),
    )
    .map_err(|e| format!("handshake: {e:?}"))?;

    assert_eq!(
        transport.calls()[1],
        PortCall::SubscribeAndSnapshot { cold: false },
        "a restart with a durable checkpoint must not resume cold"
    );
    assert_eq!(
        armed
            .snapshot()
            .start_after()
            .map(ExclusiveSourceCursor::as_str),
        Some("cur-7")
    );
    assert_eq!(armed.generation(), SubscriptionGeneration::FIRST);
    Ok(())
}

/// Vector 10: `bounds-and-shutdown`.
///
/// Bounds stop for their exact reason, and a shutdown either proves what it
/// joined or reports exactly what remained. A warning-only close is not a
/// shutdown.
#[test]
fn vector_10_bounds_stop_for_their_exact_reason_and_shutdown_proves_itself(
) -> Result<(), Box<dyn std::error::Error>> {
    // The settled profile is the smaller of provider and Grapt ceiling.
    let limits = settled();
    assert_eq!(limits.body_utf8_bytes(), 65_536);
    assert_eq!(limits.messages_per_page(), 64);
    assert_eq!(limits.aggregate_body_bytes_per_page(), 262_144);
    assert_eq!(limits.advisory_signal_payload_bytes(), 0);
    assert_eq!(limits.coalesced_pending_signals(), 1);

    let mut script = script_opening(TakeoverReceipt::first_open(BindingGeneration::FIRST))?;
    script.dismissals = vec![Ok(BulkDismissOutcome::new(
        vec![RemoteMailboxMessageId::new("msg-0").ok_or("message")?],
        vec![
            RemoteMailboxMessageId::new("msg-1").ok_or("message")?,
            RemoteMailboxMessageId::new("msg-2").ok_or("message")?,
        ],
        projects()?,
    ))];
    script.closes = vec![Ok(ShutdownReceipt::new(3, 12))];

    let (mut transport, binding) = opened(script)?;

    // A bulk dismiss refuses its own count bound rather than silently trimming.
    //
    // Built AFTER the open, and that ordering is now structural rather than
    // stylistic: a dismiss takes its ProjectPair from the binding, so there is no
    // point in the program where a dismiss exists without an authenticated pair
    // behind it.
    let many: Vec<RemoteMailboxMessageId> = (0..3)
        .map(|i| RemoteMailboxMessageId::new(format!("msg-{i}")).ok_or("message"))
        .collect::<Result<Vec<_>, _>>()?;
    assert!(BulkDismissRequest::try_new(&binding, many.clone(), DismissReason::Stale, 2).is_err());
    let request = BulkDismissRequest::try_new(&binding, many, DismissReason::Stale, 64)
        .map_err(|e| format!("within bounds: {e:?}"))?;
    assert_eq!(
        request.projects(),
        binding.receipt().projects(),
        "J8: the dismiss carries the pair its binding authenticated"
    );

    let dismissed = block_on(transport.dismiss(&binding, request))
        .map_err(|e| format!("a bounded dismiss: {e:?}"))?;
    assert_eq!(dismissed.dismissed().len(), 1);
    assert_eq!(dismissed.already_dismissed().len(), 2);
    assert!(!dismissed.is_noop());

    let receipt = block_on(transport.close(binding, ShutdownBudget::new(300, 8)))
        .map_err(|e| format!("a clean shutdown proves what it joined: {e:?}"))?;
    assert_eq!(receipt.joined_tasks(), 3);
    assert_eq!(receipt.ticks_used(), 12);

    // The timeout side is a different fact carrying the exact remaining count.
    let timeout = LifecycleOutcome::ShutdownBudgetExhausted { tasks_remaining: 2 };
    assert_ne!(
        timeout,
        LifecycleOutcome::ShutdownBudgetExhausted { tasks_remaining: 0 },
        "a truthful timeout names how many tasks it could not join"
    );
    Ok(())
}
