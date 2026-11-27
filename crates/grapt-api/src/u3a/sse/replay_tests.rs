//! A resumed positions subscriber receives exactly what an uninterrupted one
//! would have: the frames it missed, no hydration, and never across a barrier.
use super::*;
use crate::u3a::{
    dto::NonNegativeU64, FiniteF64, LiveEvent, SessionStatusData, SessionStatusInfo, WireId,
};
use hyper::body::Body as _;
use std::future::poll_fn;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn hub(pending: usize, event_bytes: usize) -> Result<DogfoodEventHub, SseError> {
    let limits = SseLimits::try_new(SseLimitValues {
        subscribers: 4,
        pending_events: pending,
        event_bytes,
        frame_bytes: MAX_SSE_FRAME_BYTES,
    })?;
    let id = WireId::parse("evt_replay_connected".into()).ok_or(SseError::Encode)?;
    let hub = DogfoodEventHub::try_new(limits, ServerConnectedEvent::new(id))?;
    hub.install_status_hydrator(Box::new(|| vec![status("hydrated", "x")]));
    Ok(hub)
}

fn status(suffix: &str, message: &str) -> DogfoodEvent {
    let id = WireId::<grapt_types::EventId>::parse(format!("evt_{suffix}"));
    let session = WireId::<grapt_types::SessionId>::parse("ses_replay".into());
    let (Some(id), Some(session), Ok(created)) = (id, session, FiniteF64::try_new(1.0)) else {
        panic!("status fixture");
    };
    let info = SessionStatusInfo::retry(
        NonNegativeU64::from(1),
        message.to_owned(),
        None,
        NonNegativeU64::from(2),
    );
    DogfoodEvent::SessionStatus(LiveEvent::new(
        id,
        created,
        SessionStatusData::new(session, info),
    ))
}

fn positions(hub: &DogfoodEventHub, resume: Option<&str>) -> Result<SseBody, SseError> {
    hub.subscribe_window(
        query::EventLocationQuery::Absent,
        None,
        None,
        Vec::new(),
        true,
        resume,
    )
}

async fn frame(body: &mut SseBody) -> Option<String> {
    match poll_fn(|context| Pin::new(&mut *body).poll_frame(context)).await {
        Some(Ok(frame)) => frame
            .into_data()
            .ok()
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned()),
        _ => None,
    }
}

/// Every frame already queued, without waiting for more.
fn queued(body: &mut SseBody) -> Vec<String> {
    let mut frames = Vec::new();
    while let Ok(bytes) = body.receiver.try_recv() {
        frames.push(String::from_utf8_lossy(&bytes).into_owned());
    }
    frames
}

fn id_of(frame: &str) -> &str {
    frame
        .strip_prefix("id: ")
        .and_then(|rest| rest.split('\n').next())
        .unwrap_or_default()
}

#[tokio::test]
async fn a_dropped_subscriber_resumes_with_exactly_the_frames_it_missed() -> TestResult {
    let hub = hub(16, MAX_SSE_EVENT_BYTES)?;
    let mut live = positions(&hub, None)?;
    let _connected = frame(&mut live).await.ok_or("connected")?;
    let _hydrated = frame(&mut live).await.ok_or("hydration")?;
    hub.publish(&status("one", "one"))?;
    hub.publish(&status("two", "two"))?;
    let one = frame(&mut live).await.ok_or("one")?;
    drop(live);
    hub.publish(&status("three", "three"))?;
    let cursor = id_of(&one).to_owned();
    let mut resumed = positions(&hub, Some(&cursor))?;
    let frames = queued(&mut resumed);
    assert_eq!(frames.len(), 3, "the echo, then two and three: {frames:?}");
    assert_eq!(
        id_of(&frames[0]),
        cursor,
        "the certificate echoes the cursor"
    );
    assert!(frames[0].contains("server.connected"));
    assert!(frames[1].contains("evt_two") && frames[2].contains("evt_three"));
    assert!(
        !frames.iter().any(|frame| frame.contains("evt_hydrated")),
        "no hydration"
    );
    assert_eq!(hub.replay_counts().resumed, 1);
    assert_eq!(hub.replay_counts().replayed_frames, 2);
    Ok(())
}

#[tokio::test]
async fn every_barrier_refuses_a_resume_across_it_and_subscribes_afresh() -> TestResult {
    let barriers: [fn(&DogfoodEventHub); 3] = [
        DogfoodEventHub::invalidate,
        DogfoodEventHub::reconcile,
        DogfoodEventHub::shutdown,
    ];
    for (index, barrier) in barriers.into_iter().enumerate() {
        let hub = hub(16, MAX_SSE_EVENT_BYTES)?;
        let mut live = positions(&hub, None)?;
        let connected = frame(&mut live).await.ok_or("connected")?;
        let cursor = id_of(&connected).to_owned();
        // The client is already disconnected when the barrier falls.
        drop(live);
        barrier(&hub);
        hub.publish(&status("after", "after"))?;
        let mut fresh = positions(&hub, Some(&cursor))?;
        let frames = queued(&mut fresh);
        assert_ne!(id_of(&frames[0]), cursor, "barrier {index}: a fresh id");
        assert!(
            frames.iter().any(|frame| frame.contains("evt_hydrated")),
            "barrier {index}: hydrated"
        );
        assert!(
            !frames.iter().any(|frame| frame.contains("evt_after")),
            "barrier {index}: no replay"
        );
        assert_eq!(hub.replay_counts().refused_barrier, 1, "barrier {index}");
        assert_eq!(hub.replay_counts().resumed, 0, "barrier {index}");
    }
    Ok(())
}

#[tokio::test]
async fn an_event_that_does_not_encode_is_not_a_barrier() -> TestResult {
    let hub = hub(16, 1_024)?;
    let mut live = positions(&hub, None)?;
    let connected = frame(&mut live).await.ok_or("connected")?;
    drop(live);
    let oversized = status("oversized", &"x".repeat(2_048));
    assert!(matches!(hub.publish(&oversized), Err(SseError::EventBytes)));
    hub.publish(&status("fits", "fits"))?;
    let mut resumed = positions(&hub, Some(id_of(&connected)))?;
    let frames = queued(&mut resumed);
    assert_eq!(
        frames.len(),
        2,
        "skipped exactly as a live subscriber skipped it"
    );
    assert!(frames[1].contains("evt_fits"));
    assert_eq!(hub.replay_counts().barriers, 0);
    Ok(())
}

#[tokio::test]
async fn a_replay_past_the_opening_batch_is_refused_with_no_echo() -> TestResult {
    let hub = hub(3, MAX_SSE_EVENT_BYTES)?;
    let mut live = positions(&hub, None)?;
    let connected = frame(&mut live).await.ok_or("connected")?;
    let cursor = id_of(&connected).to_owned();
    drop(live);
    for suffix in ["a", "b", "c"] {
        hub.publish(&status(suffix, suffix))?;
    }
    let mut fresh = positions(&hub, Some(&cursor))?;
    let frames = queued(&mut fresh);
    assert_ne!(id_of(&frames[0]), cursor, "no echo before a refusal");
    assert!(frames.iter().any(|frame| frame.contains("evt_hydrated")));
    assert_eq!(hub.replay_counts().refused_over_batch, 1);
    Ok(())
}

#[tokio::test]
async fn the_replay_follows_the_resuming_requests_interest() -> TestResult {
    let hub = hub(16, MAX_SSE_EVENT_BYTES)?;
    let mut live = positions(&hub, None)?;
    let connected = frame(&mut live).await.ok_or("connected")?;
    drop(live);
    let owned = grapt_types::LocationRef::new("/work/owned", None)?;
    let foreign = grapt_types::LocationRef::new("/work/foreign", None)?;
    hub.publish_at(&status("foreign", "foreign"), &foreign)?;
    hub.publish_at(&status("owned", "owned"), &owned)?;
    let interest = query::event_location(Some("location%5Bdirectory%5D=/work/owned"))
        .map_err(|_| "interest")?;
    let mut resumed = hub.subscribe_window(
        interest,
        None,
        None,
        Vec::new(),
        true,
        Some(id_of(&connected)),
    )?;
    let frames = queued(&mut resumed);
    assert_eq!(frames.len(), 2, "{frames:?}");
    assert!(frames[1].contains("evt_owned"));
    Ok(())
}

#[tokio::test]
async fn a_legacy_subscriber_naming_an_id_is_never_resumed() -> TestResult {
    let hub = hub(16, MAX_SSE_EVENT_BYTES)?;
    let mut live = positions(&hub, None)?;
    let connected = frame(&mut live).await.ok_or("connected")?;
    drop(live);
    let mut legacy = hub.subscribe_window(
        query::EventLocationQuery::Absent,
        None,
        None,
        Vec::new(),
        false,
        Some(id_of(&connected)),
    )?;
    let frames = queued(&mut legacy);
    assert!(
        !frames[0].starts_with("id: "),
        "legacy frames carry no position"
    );
    assert_eq!(
        hub.replay_counts(),
        grapt_stream::replay::ReplayCounts::default()
    );
    Ok(())
}

#[test]
fn replay_counts_remain_readable_while_a_publish_holds_subscribers() -> TestResult {
    use std::sync::mpsc::sync_channel;
    use std::time::Duration;

    let hub = Arc::new(hub(8, MAX_SSE_EVENT_BYTES)?);
    let before = hub.replay_counts();
    let (entered, waiting) = sync_channel(1);
    let (release, released) = sync_channel(1);
    let writer_hub = Arc::clone(&hub);
    let writer = std::thread::spawn(move || {
        writer_hub.publish_if(&status("held_publish", "pending"), || {
            entered
                .send(())
                .unwrap_or_else(|error| panic!("publish barrier: {error}"));
            released
                .recv_timeout(Duration::from_secs(10))
                .unwrap_or_else(|error| panic!("publish release: {error}"));
            true
        })
    });
    let entered = waiting.recv_timeout(Duration::from_secs(2));
    let (reported, report) = sync_channel(1);
    let reader_hub = Arc::clone(&hub);
    let reader = std::thread::spawn(move || reported.send(reader_hub.replay_counts()));
    let observed = report.recv_timeout(Duration::from_secs(2));
    // Release and join both workers even when the old read waited for publication.
    let released = release.send(());
    let written = writer.join();
    let read = reader.join();
    assert!(
        entered.is_ok(),
        "publisher never entered its held-lock barrier"
    );
    assert!(released.is_ok(), "publisher left before its release");
    assert!(written.is_ok_and(|result| result.is_ok()));
    assert!(read.is_ok_and(|result| result.is_ok()));
    assert_eq!(
        observed.ok(),
        Some(before),
        "health counts waited for publication"
    );
    Ok(())
}
