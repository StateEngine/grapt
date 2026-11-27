use super::*;
use std::future::poll_fn;

use crate::u3a::{
    dto::NonNegativeU64, FiniteF64, LiveEvent, SessionStatusAction, SessionStatusData,
    SessionStatusInfo, WireId,
};
use hyper::body::Body as _;

#[tokio::test]
async fn a_named_window_attaches_on_subscribe_and_detaches_when_its_stream_ends(
) -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(2, 8, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("window_close"),
    )?;
    let session: grapt_types::SessionId = match "ses_window_close".parse() {
        Ok(session) => session,
        Err(_) => panic!("session fixture"),
    };
    let unwatched = |hub: &DogfoodEventHub, session: &grapt_types::SessionId| {
        hub.windows()
            .unwatched()
            .iter()
            .any(|watched| watched == session)
    };
    assert!(
        !unwatched(&hub, &session),
        "an unopened window is not a loss"
    );

    let window = hub.subscribe_window_at(
        query::EventLocationQuery::Absent,
        None,
        Some(session.clone()),
    )?;
    assert!(
        !unwatched(&hub, &session),
        "a registered window keeps the session watched"
    );

    drop(window);
    assert!(
        unwatched(&hub, &session),
        "the stream ending is what tells the service the window is gone"
    );
    Ok(())
}

#[tokio::test]
async fn mail_attention_hydration_and_live_frames_keep_owner_location_scope() -> Result<(), SseError>
{
    use crate::u3a::{MailAttentionData, MailAttentionInfo};
    let hub = DogfoodEventHub::try_new(
        limits(2, 8, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("mail_scope"),
    )?;
    let location = grapt_types::LocationRef::new("/work/owned", None)
        .unwrap_or_else(|e| panic!("location: {e}"));
    let other_location = grapt_types::LocationRef::new("/work/foreign", None)
        .unwrap_or_else(|e| panic!("location: {e}"));
    let event = DogfoodEvent::MailAttention(LiveEvent::new(
        WireId::parse("evt_mail_scope".into()).unwrap_or_else(|| panic!("event")),
        FiniteF64::try_new(1.0).unwrap_or_else(|e| panic!("time: {e}")),
        MailAttentionData::new(
            status_session("mail_scope"),
            MailAttentionInfo::Unavailable,
            location.clone(),
        ),
    ));
    let hydrate = event.clone();
    assert!(hub.install_status_hydrator(Box::new(move || vec![
        status_event("original_status", SessionStatusInfo::idle()),
        hydrate.clone()
    ])));
    let interest = |directory: &str| {
        query::event_location(Some(&format!("location%5Bdirectory%5D={directory}")))
            .unwrap_or_else(|e| panic!("query: {e:?}"))
    };
    let mut selected = hub.subscribe_at(interest("/work/owned"))?;
    let mut foreign = hub.subscribe_at(interest("/work/foreign"))?;
    for body in [&mut selected, &mut foreign] {
        assert!(next_frame_text(body).await.contains("server.connected"));
        assert!(
            next_frame_text(body).await.contains("session.status"),
            "mail must not displace original status hydration"
        );
    }
    assert!(next_frame_text(&mut selected)
        .await
        .contains("session.mail.attention"));
    assert!(
        foreign.receiver.try_recv().is_err(),
        "foreign scoped subscriber received mail hydration"
    );
    assert_eq!(
        hub.publish_at(&event, &other_location)?.delivered,
        1,
        "caller-supplied location must not replace the Session owner"
    );
    assert_eq!(
        selected.receiver.len(),
        1,
        "live mail must already be queued for its owner before awaiting a frame"
    );
    let frame = next_frame_text(&mut selected).await;
    assert!(frame.contains("/work/owned") && !frame.contains("/work/foreign"));
    assert!(
        foreign.receiver.try_recv().is_err(),
        "foreign scoped subscriber received live mail"
    );
    Ok(())
}

fn connected(suffix: &str) -> ServerConnectedEvent {
    let raw = format!("evt_{suffix}");
    let Some(id) = WireId::parse(raw) else {
        panic!("event identity fixture was invalid");
    };
    ServerConnectedEvent::new(id)
}

fn limits(
    subscribers: usize,
    pending_events: usize,
    event_bytes: usize,
    frame_bytes: usize,
) -> Result<SseLimits, SseError> {
    SseLimits::try_new(SseLimitValues {
        subscribers,
        pending_events,
        event_bytes,
        frame_bytes,
    })
}

async fn next(body: &mut SseBody) -> Option<Result<Frame<Bytes>, io::Error>> {
    poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await
}

#[tokio::test]
async fn scoped_events_do_not_fill_another_locations_queue() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(3, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("initial"),
    )?;
    let location = match grapt_types::LocationRef::new("/work/project", None) {
        Ok(location) => location,
        Err(error) => panic!("location fixture: {error}"),
    };
    let interest = |directory: &str| {
        let raw = format!("location%5Bdirectory%5D={directory}");
        match query::event_location(Some(&raw)) {
            Ok(interest) => interest,
            Err(error) => panic!("interest fixture: {error:?}"),
        }
    };
    let mut selected = hub.subscribe_at(interest("/work/project"))?;
    let mut other = hub.subscribe_at(interest("/work/other"))?;
    let mut all = hub.subscribe()?;
    for body in [&mut selected, &mut other, &mut all] {
        assert!(matches!(next(body).await, Some(Ok(_))));
    }
    let event = DogfoodEvent::ServerConnected(connected("scoped"));
    for _ in 0..3 {
        assert_eq!(hub.publish_at(&event, &location)?.delivered, 2);
        let frame = match next(&mut selected).await {
            Some(Ok(frame)) => frame,
            _ => panic!("scoped frame absent"),
        };
        let bytes = frame
            .into_data()
            .unwrap_or_else(|_| panic!("scoped frame not data"));
        let text = std::str::from_utf8(&bytes).unwrap_or_else(|error| panic!("frame: {error}"));
        let data = text
            .strip_prefix("event: message\ndata: ")
            .unwrap_or_else(|| panic!("frame prefix"));
        let value: serde_json::Value =
            serde_json::from_str(data.trim()).unwrap_or_else(|error| panic!("frame JSON: {error}"));
        assert_eq!(value["location"]["directory"], "/work/project");
        assert!(value["location"].get("workspaceID").is_none());
        assert!(matches!(next(&mut all).await, Some(Ok(_))));
        poll_fn(|context| {
            assert!(Pin::new(&mut other).poll_frame(context).is_pending());
            Poll::Ready(())
        })
        .await;
    }
    assert_eq!(hub.publish(&event)?.delivered, 3);
    assert!(matches!(next(&mut other).await, Some(Ok(_))));
    Ok(())
}

#[tokio::test]
async fn connection_frame_has_no_replay_id_or_retry_field() {
    let limits = match limits(1, 2, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("connected")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    let mut body = match hub.subscribe() {
        Ok(body) => body,
        Err(error) => panic!("subscription failed: {error}"),
    };
    let frame = match next(&mut body).await {
        Some(Ok(frame)) => frame,
        Some(Err(error)) => panic!("connected frame failed: {error}"),
        None => panic!("connected frame was absent"),
    };
    let bytes = match frame.into_data() {
        Ok(bytes) => bytes,
        Err(_) => panic!("connected frame was not data"),
    };
    let text = match std::str::from_utf8(&bytes) {
        Ok(text) => text,
        Err(error) => panic!("connected frame was not UTF-8: {error}"),
    };
    assert!(text.starts_with("event: message\ndata: {\"type\":\"server.connected\""));
    assert!(!text.contains("\nid:"));
    assert!(!text.contains("\nretry:"));
}

#[tokio::test]
async fn a_slow_subscriber_overflows_and_fails_instead_of_replaying() {
    let limits = match limits(1, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("initial")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    let mut body = match hub.subscribe() {
        Ok(body) => body,
        Err(error) => panic!("subscription failed: {error}"),
    };
    assert!(matches!(next(&mut body).await, Some(Ok(_))));

    let first = DogfoodEvent::ServerConnected(connected("first"));
    let second = DogfoodEvent::ServerConnected(connected("second"));
    let first_receipt = match hub.publish(&first) {
        Ok(receipt) => receipt,
        Err(error) => panic!("first publish failed: {error}"),
    };
    assert_eq!(first_receipt.delivered, 1);
    let second_receipt = match hub.publish(&second) {
        Ok(receipt) => receipt,
        Err(error) => panic!("second publish failed: {error}"),
    };
    assert_eq!(second_receipt.overflowed, 1);
    assert!(matches!(next(&mut body).await, Some(Err(_))));
    assert!(next(&mut body).await.is_none());
}

#[tokio::test]
async fn invalidated_projection_closes_old_stream_and_new_subscription_starts_clean(
) -> Result<(), SseError> {
    let limits = limits(1, 4, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?;
    let hub = DogfoodEventHub::try_new(limits, connected("invalidated"))?;
    let mut old = hub.subscribe()?;
    assert!(matches!(next(&mut old).await, Some(Ok(_))));
    hub.invalidate();
    assert!(matches!(next(&mut old).await, Some(Err(_))));
    assert!(next(&mut old).await.is_none());
    let mut fresh = hub.subscribe()?;
    assert!(matches!(next(&mut fresh).await, Some(Ok(_))));
    Ok(())
}

#[tokio::test]
async fn reconciliation_drains_valid_frames_then_closes_without_replaying() -> Result<(), SseError>
{
    let hub = DogfoodEventHub::try_new(
        limits(1, 4, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("initial"),
    )?;
    let mut old = hub.subscribe()?;
    assert!(matches!(next(&mut old).await, Some(Ok(_))));
    let terminal = DogfoodEvent::ServerConnected(connected("terminal"));
    assert_eq!(hub.publish(&terminal)?.delivered, 1);
    hub.reconcile();
    let frame = match next(&mut old).await {
        Some(Ok(frame)) => frame,
        other => panic!("reconciliation discarded a valid queued frame: {other:?}"),
    };
    let bytes = match frame.into_data() {
        Ok(bytes) => bytes,
        Err(_) => panic!("queued frame had no data"),
    };
    assert!(String::from_utf8_lossy(&bytes).contains("evt_terminal"));
    poll_fn(|context| {
        assert!(
            matches!(Pin::new(&mut old).poll_frame(context), Poll::Ready(None)),
            "reconciliation must close after its last queued frame"
        );
        Poll::Ready(())
    })
    .await;
    let mut fresh = hub.subscribe()?;
    let frame = match next(&mut fresh).await {
        Some(Ok(frame)) => frame,
        other => panic!("fresh subscription did not connect: {other:?}"),
    };
    let bytes = match frame.into_data() {
        Ok(bytes) => bytes,
        Err(_) => panic!("connected frame had no data"),
    };
    assert!(!String::from_utf8_lossy(&bytes).contains("evt_terminal"));
    Ok(())
}

#[test]
fn subscriber_and_queue_bounds_are_checked() {
    assert!(matches!(
        limits(0, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES),
        Err(SseError::ZeroBound { .. })
    ));
    assert!(matches!(
        limits(
            MAX_SSE_SUBSCRIBERS + 1,
            1,
            MAX_SSE_EVENT_BYTES,
            MAX_SSE_FRAME_BYTES,
        ),
        Err(SseError::BoundTooLarge { .. })
    ));
    assert!(matches!(
        limits(
            1,
            MAX_SSE_PENDING_EVENTS + 1,
            MAX_SSE_EVENT_BYTES,
            MAX_SSE_FRAME_BYTES,
        ),
        Err(SseError::BoundTooLarge { .. })
    ));

    let limits = match limits(1, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("bound")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    let _first = match hub.subscribe() {
        Ok(subscription) => subscription,
        Err(error) => panic!("first subscription failed: {error}"),
    };
    assert!(matches!(hub.subscribe(), Err(SseError::SubscriberBound)));
}

#[test]
fn event_and_frame_byte_bounds_are_reachable_before_their_allocations() {
    let event = connected("eventbytes");
    let production = match U3aLimits::production() {
        Ok(limits) => limits,
        Err(error) => panic!("production DTO limits failed: {error}"),
    };
    let encoded = match encode_page(&DogfoodEvent::ServerConnected(event.clone()), production) {
        Ok(encoded) => encoded,
        Err(error) => panic!("connected event fixture did not encode: {error}"),
    };
    let Some(event_bytes) = encoded.len().checked_sub(1) else {
        panic!("connected event fixture encoded to zero bytes");
    };
    let event_limited = match limits(1, 1, event_bytes, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("event-byte limits failed: {error}"),
    };
    assert!(matches!(
        DogfoodEventHub::try_new(event_limited, event),
        Err(SseError::EventBytes)
    ));

    let frame_event = DogfoodEvent::ServerConnected(connected("framebytes"));
    let production_sse = match SseLimits::production() {
        Ok(limits) => limits,
        Err(error) => panic!("production SSE limits failed: {error}"),
    };
    let exact_frame = match encode_sse(&frame_event, production_sse) {
        Ok(frame) => frame.len(),
        Err(error) => panic!("frame fixture did not encode: {error}"),
    };
    let exact_limits = match limits(1, 1, MAX_SSE_EVENT_BYTES, exact_frame) {
        Ok(limits) => limits,
        Err(error) => panic!("exact frame-byte limits failed: {error}"),
    };
    let exact = match encode_sse(&frame_event, exact_limits) {
        Ok(frame) => frame,
        Err(error) => panic!("exact frame-byte limit refused its frame: {error}"),
    };
    assert_eq!(
        exact.len(),
        exact_frame,
        "exact frame-byte limit changed the encoded length"
    );

    let Some(one_below) = exact_frame.checked_sub(1) else {
        panic!("frame fixture encoded to zero bytes");
    };
    let frame_limited = match limits(1, 1, MAX_SSE_EVENT_BYTES, one_below) {
        Ok(limits) => limits,
        Err(error) => panic!("one-below frame-byte limits failed: {error}"),
    };
    assert!(
        matches!(
            encode_sse(&frame_event, frame_limited),
            Err(SseError::FrameBytes)
        ),
        "one-below frame-byte limit did not return FrameBytes"
    );
}

#[test]
fn configured_frame_bound_is_not_replaced_by_the_global_maximum() {
    let frame_limited = match limits(1, 1, MAX_SSE_EVENT_BYTES, 1) {
        Ok(limits) => limits,
        Err(error) => panic!("configured frame-byte limits failed: {error}"),
    };
    let event = DogfoodEvent::ServerConnected(connected("configuredframebound"));
    assert!(
        matches!(encode_sse(&event, frame_limited), Err(SseError::FrameBytes)),
        "configured frame-byte bound was replaced by the global maximum"
    );
}

#[test]
fn host_subscription_overflow_cancel_and_reconnect_are_explicit() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(2, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("host_initial"),
    )?;
    let old = hub.subscribe_host()?;
    assert!(old.next_frame()?.is_some());
    let first = DogfoodEvent::ServerConnected(connected("host_first"));
    let second = DogfoodEvent::ServerConnected(connected("host_second"));
    assert_eq!(hub.publish(&first)?.delivered, 1);
    assert_eq!(hub.publish(&second)?.overflowed, 1);
    assert!(matches!(
        old.next_frame(),
        Err(SseError::SubscriberOverflow)
    ));

    let fresh = hub.subscribe_host()?;
    let frame = fresh.next_frame()?.ok_or(SseError::Encode)?;
    assert!(String::from_utf8_lossy(&frame).contains("evt_host_initial"));
    fresh.cancel();
    assert!(fresh.next_frame()?.is_none());
    Ok(())
}

#[tokio::test]
async fn every_way_the_hub_ends_subscribers_is_reported_with_its_reason() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(4, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("observed"),
    )?;
    let seen = Arc::new(Mutex::new(Vec::<(StreamEndCause, usize, String)>::new()));
    let record = Arc::clone(&seen);
    assert!(hub.observe_stream_ends(Box::new(move |end| {
        if let Ok(mut seen) = record.lock() {
            seen.push((end.cause, end.subscribers, end.reason.to_owned()));
        }
    })));
    // Only one observer may be installed.
    assert!(!hub.observe_stream_ends(Box::new(|_| {})));

    // Global recovery is counted and reported even with no subscribers.
    hub.reconcile_because(ReconcileReason::UnpublishedAdvisoryNotice, "idle");

    let _first = hub.subscribe()?;
    let _second = hub.subscribe()?;
    hub.reconcile_because(
        ReconcileReason::UnrepresentableFrame,
        "frame exceeded bound",
    );
    let _third = hub.subscribe()?;
    hub.invalidate_because("publication refused");
    let _fourth = hub.subscribe()?;
    let event = DogfoodEvent::ServerConnected(connected("overflowing"));
    hub.publish(&event)?;
    hub.publish(&event)?;
    let _fifth = hub.subscribe()?;
    hub.invalidate();

    let seen = seen.lock().map(|seen| seen.clone()).unwrap_or_default();
    assert_eq!(
        seen,
        vec![
            (StreamEndCause::Reconcile, 0, "idle".to_owned()),
            (
                StreamEndCause::Reconcile,
                2,
                "frame exceeded bound".to_owned()
            ),
            (
                StreamEndCause::Invalidate,
                1,
                "publication refused".to_owned()
            ),
            (StreamEndCause::Overflow, 1, "server.connected".to_owned()),
            (StreamEndCause::Invalidate, 1, "unstated".to_owned()),
        ]
    );
    Ok(())
}

#[test]
fn each_global_recovery_reason_counts_once_even_without_subscribers() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(4, 1, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("counts"),
    )?;
    assert_eq!(hub.reconcile_counts(), ReconcileCounts::default());
    hub.reconcile_because(
        ReconcileReason::UnpublishedAdvisoryNotice,
        "notice absent from stream",
    );
    assert_eq!(
        hub.reconcile_counts(),
        ReconcileCounts {
            unpublished_advisory_notice: 1,
            ..ReconcileCounts::default()
        }
    );
    hub.reconcile_because(ReconcileReason::UnrepresentableFrame, "frame cannot fit");
    assert_eq!(
        hub.reconcile_counts(),
        ReconcileCounts {
            unpublished_advisory_notice: 1,
            unrepresentable_frame: 1,
            projection_failure: 0
        }
    );
    hub.invalidate_because("projection unavailable");
    assert_eq!(
        hub.reconcile_counts(),
        ReconcileCounts {
            unpublished_advisory_notice: 1,
            unrepresentable_frame: 1,
            projection_failure: 1
        }
    );
    // A subscriber-local overflow is already diagnosed separately, not global recovery.
    let _subscriber = hub.subscribe()?;
    hub.publish(&DogfoodEvent::ServerConnected(connected("overflow_counts")))?;
    assert_eq!(
        hub.reconcile_counts(),
        ReconcileCounts {
            unpublished_advisory_notice: 1,
            unrepresentable_frame: 1,
            projection_failure: 1
        }
    );
    Ok(())
}
fn status_session(suffix: &str) -> WireId<grapt_types::SessionId> {
    match WireId::<grapt_types::SessionId>::parse(format!("ses_{suffix}")) {
        Some(id) => id,
        None => panic!("session identity fixture was invalid"),
    }
}

fn status_event(suffix: &str, status: SessionStatusInfo) -> DogfoodEvent {
    let id = match WireId::<grapt_types::EventId>::parse(format!("evt_{suffix}")) {
        Some(id) => id,
        None => panic!("event identity fixture was invalid"),
    };
    let created = match FiniteF64::try_new(1.0) {
        Ok(created) => created,
        Err(error) => panic!("finite fixture failed: {error}"),
    };
    DogfoodEvent::SessionStatus(LiveEvent::new(
        id,
        created,
        SessionStatusData::new(status_session(suffix), status),
    ))
}

async fn next_frame_text(body: &mut SseBody) -> String {
    let frame = match next(body).await {
        Some(Ok(frame)) => frame,
        Some(Err(error)) => panic!("frame stream failed: {error}"),
        None => panic!("frame stream ended"),
    };
    frame_text(frame)
}

fn frame_text(frame: Frame<Bytes>) -> String {
    let bytes = frame
        .into_data()
        .unwrap_or_else(|_| panic!("frame was not data"));
    std::str::from_utf8(&bytes)
        .unwrap_or_else(|error| panic!("frame was not UTF-8: {error}"))
        .to_string()
}

#[test]
fn session_status_wire_shape_matches_the_frozen_schema() {
    let idle = status_event("shape_idle", SessionStatusInfo::idle());
    let json =
        serde_json::to_string(&idle).unwrap_or_else(|error| panic!("encode failed: {error}"));
    assert_eq!(
        json,
        r#"{"type":"session.status","id":"evt_shape_idle","created":1.0,"data":{"sessionID":"ses_shape_idle","status":{"type":"idle"}}}"#
    );
    let retry = status_event(
        "shape_retry",
        SessionStatusInfo::retry(
            NonNegativeU64::from(2),
            "provider is warming up".to_string(),
            Some(SessionStatusAction::new(
                "rate_limited".to_string(),
                "openai".to_string(),
                "Retry".to_string(),
                "Try again".to_string(),
                "Retry now".to_string(),
                None,
            )),
            NonNegativeU64::from(5),
        ),
    );
    let json =
        serde_json::to_string(&retry).unwrap_or_else(|error| panic!("encode failed: {error}"));
    assert_eq!(
        json,
        r#"{"type":"session.status","id":"evt_shape_retry","created":1.0,"data":{"sessionID":"ses_shape_retry","status":{"type":"retry","attempt":2,"message":"provider is warming up","action":{"reason":"rate_limited","provider":"openai","title":"Retry","message":"Try again","label":"Retry now"},"next":5}}}"#
    );
    let parsed: SessionStatusInfo = serde_json::from_str(
        r#"{"type":"retry","attempt":2,"message":"provider is warming up","action":{"reason":"rate_limited","provider":"openai","title":"Retry","message":"Try again","label":"Retry now","link":"https://x"},"next":5}"#,
    )
    .unwrap_or_else(|error| panic!("decode failed: {error}"));
    assert_eq!(
        parsed,
        SessionStatusInfo::retry(
            NonNegativeU64::from(2),
            "provider is warming up".to_string(),
            Some(SessionStatusAction::new(
                "rate_limited".to_string(),
                "openai".to_string(),
                "Retry".to_string(),
                "Try again".to_string(),
                "Retry now".to_string(),
                Some("https://x".to_string()),
            )),
            NonNegativeU64::from(5),
        )
    );
}

#[test]
fn session_status_info_rejects_shapes_outside_the_frozen_schema() {
    let reject = |raw: &str| {
        assert!(
            serde_json::from_str::<SessionStatusInfo>(raw).is_err(),
            "accepted shape outside the frozen schema: {raw}"
        );
    };
    reject(r#"{"type":"idle","extra":1}"#);
    reject(r#"{"type":"busy","extra":1}"#);
    reject(r#"{"type":"retry","attempt":0,"message":"m"}"#);
    reject(r#"{"type":"retry","attempt":-1,"message":"m","next":1}"#);
    reject(r#"{"type":"retry","attempt":0,"message":"m","next":1,"extra":true}"#);
    reject(
        r#"{"type":"retry","attempt":0,"message":"m","next":1,"action":{"reason":"r","provider":"p","title":"t","message":"m"}}"#,
    );
    reject(
        r#"{"type":"retry","attempt":0,"message":"m","next":1,"action":{"reason":"r","provider":"p","title":"t","message":"m","label":"l","extra":1}}"#,
    );
    reject(r#"{"type":"weird"}"#);
    reject(r#"{"type":"idle" ,"type":"busy"}"#);
}

#[tokio::test]
async fn subscribers_receive_status_hydration_before_any_live_edge() {
    let limits = match limits(1, 4, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("initial")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    let busy = status_event("hyd_busy", SessionStatusInfo::busy());
    let idle = status_event("hyd_idle", SessionStatusInfo::idle());
    assert!(hub.install_status_hydrator(Box::new(move || { vec![busy.clone(), idle.clone()] })));
    assert!(!hub.install_status_hydrator(Box::new(Vec::new)));
    let mut body = match hub.subscribe() {
        Ok(body) => body,
        Err(error) => panic!("subscription failed: {error}"),
    };
    assert!(next_frame_text(&mut body)
        .await
        .contains("server.connected"));
    let second = next_frame_text(&mut body).await;
    assert!(second.contains(r#""type":"session.status""#));
    assert!(second.contains(r#""status":{"type":"busy"}"#));
    assert!(second.contains("evt_hyd_busy"));
    let third = next_frame_text(&mut body).await;
    assert!(third.contains(r#""status":{"type":"idle"}"#));
    assert!(third.contains("evt_hyd_idle"));
    let live = status_event("hyd_live", SessionStatusInfo::busy());
    assert_eq!(
        hub.publish(&live)
            .unwrap_or_else(|error| panic!("publish: {error}"))
            .delivered,
        1
    );
    assert!(next_frame_text(&mut body).await.contains("evt_hyd_live"));
}

#[tokio::test]
async fn hydration_frames_that_do_not_fit_drop_without_failing_the_stream() {
    let limits = match limits(1, 2, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES) {
        Ok(limits) => limits,
        Err(error) => panic!("SSE limits failed: {error}"),
    };
    let hub = match DogfoodEventHub::try_new(limits, connected("initial")) {
        Ok(hub) => hub,
        Err(error) => panic!("event hub failed: {error}"),
    };
    let frames: Vec<DogfoodEvent> = (0..3)
        .map(|index| status_event(&format!("hyd_drop_{index}"), SessionStatusInfo::busy()))
        .collect();
    assert!(hub.install_status_hydrator(Box::new(move || frames.clone())));
    let mut body = match hub.subscribe() {
        Ok(body) => body,
        Err(error) => panic!("subscription failed: {error}"),
    };
    assert!(next_frame_text(&mut body)
        .await
        .contains("server.connected"));
    assert!(next_frame_text(&mut body).await.contains("evt_hyd_drop_0"));
    poll_fn(|context| {
        assert!(Pin::new(&mut body).poll_frame(context).is_pending());
        Poll::Ready(())
    })
    .await;
}

#[test]
fn compaction_phase_is_additive_without_changing_legacy_busy_status(
) -> Result<(), Box<dyn std::error::Error>> {
    let ordinary = serde_json::to_value(SessionStatusData::new(
        status_session("ordinary_phase"),
        SessionStatusInfo::busy(),
    ))?;
    assert!(ordinary.get("phase").is_none());
    let compacting = serde_json::to_value(
        SessionStatusData::new(
            status_session("compacting_phase"),
            SessionStatusInfo::busy(),
        )
        .with_phase(Some(crate::u3a::SessionStatusPhase::Compacting)),
    )?;
    assert_eq!(compacting["phase"], "compacting");
    let legacy: SessionStatusInfo = serde_json::from_value(compacting["status"].clone())?;
    assert_eq!(legacy, SessionStatusInfo::busy());
    Ok(())
}

fn provisional_block(ordinal: u64, kind: ProvisionalKind) -> ProvisionalBlock {
    match "msg_omitted".parse::<grapt_types::MessageId>() {
        Ok(message) => ProvisionalBlock {
            message,
            kind,
            ordinal,
        },
        Err(error) => panic!("message fixture: {error:?}"),
    }
}

/// An oversized block is omitted by identity: closing an unrelated retained
/// block must not make the snapshot complete while it is still live; its own
/// close does.
#[test]
fn an_omitted_block_keeps_the_snapshot_incomplete_until_it_closes() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(limits(3, 8, 1024, 1056)?, connected("omitted"))?;
    let session = match "ses_omitted".parse::<grapt_types::SessionId>() {
        Ok(session) => session,
        Err(error) => panic!("session fixture: {error:?}"),
    };
    let reasoning = provisional_block(0, ProvisionalKind::Reasoning);
    let text = provisional_block(0, ProvisionalKind::Text);
    for block in [&reasoning, &text] {
        hub.change_provisional(ProvisionalChange::Open {
            session: &session,
            block,
        })?;
    }
    let chunk = "r".repeat(400);
    for _ in 0..3 {
        hub.change_provisional(ProvisionalChange::Append {
            session: &session,
            block: &reasoning,
            text: &chunk,
        })?;
    }
    hub.change_provisional(ProvisionalChange::Close {
        session: &session,
        block: &text,
    })?;
    assert!(
        hub.provisional(&session).is_err(),
        "a live omitted block left the snapshot looking complete"
    );
    hub.change_provisional(ProvisionalChange::Close {
        session: &session,
        block: &reasoning,
    })?;
    assert_eq!(hub.provisional(&session)?, Vec::new());
    Ok(())
}

/// A block past the per-session capacity is omitted the same way.
#[test]
fn a_block_past_capacity_keeps_the_snapshot_incomplete_until_it_closes() -> Result<(), SseError> {
    let hub = DogfoodEventHub::try_new(
        limits(3, 8, MAX_SSE_EVENT_BYTES, MAX_SSE_FRAME_BYTES)?,
        connected("capacity"),
    )?;
    let session = match "ses_capacity".parse::<grapt_types::SessionId>() {
        Ok(session) => session,
        Err(error) => panic!("session fixture: {error:?}"),
    };
    let blocks: Vec<_> = (0..=crate::u3a::MAX_PROVISIONAL_BLOCKS as u64)
        .map(|ordinal| provisional_block(ordinal, ProvisionalKind::Reasoning))
        .collect();
    for block in &blocks {
        hub.change_provisional(ProvisionalChange::Open {
            session: &session,
            block,
        })?;
    }
    assert!(hub.provisional(&session).is_err());
    let (omitted, retained) = blocks.split_last().unwrap_or_else(|| panic!("blocks"));
    hub.change_provisional(ProvisionalChange::Close {
        session: &session,
        block: &retained[0],
    })?;
    assert!(
        hub.provisional(&session).is_err(),
        "an unrelated close cleared the latch"
    );
    hub.change_provisional(ProvisionalChange::Close {
        session: &session,
        block: omitted,
    })?;
    assert_eq!(hub.provisional(&session)?.len(), retained.len() - 1);
    Ok(())
}
