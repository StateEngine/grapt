//! A hinted read may stall or fail without killing input or replaying effects.
use super::*;

#[derive(Debug)]
struct HeldTools {
    inner: ProgressPeer,
    reply: Option<Result<crate::transport::WireResponse, TransportError>>,
    starts: usize,
    cancels: usize,
}
impl Transport for HeldTools {
    fn call(
        &mut self,
        request: &WireRequest,
    ) -> Result<crate::transport::WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for HeldTools {
    fn subscribed(&self) -> bool {
        true
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn begin_tool_read(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        assert_eq!(request.operation(), Operation::MessageList);
        self.starts += 1;
        Ok(None)
    }
    fn poll_tool_read(&mut self) -> Result<Option<crate::transport::WireResponse>, TransportError> {
        self.reply.take().transpose()
    }
    fn cancel_tool_read(&mut self) {
        self.cancels += 1;
    }
}

fn hint() -> FramePoll {
    FramePoll::Frame(serde_json::json!({"id":"evt_hint", "type":"session.tool.progress", "created":1,
        "data":{"sessionID":"ses_000000000001", "assistantMessageID":"msg_000000000001",
        "callID":"call_000000000001", "metadata":{"toolCalls":[{"tool":"fixture.add","status":"running"}]}}})
        .to_string())
}

fn fixture() -> Result<Client<HeldTools>, Box<dyn std::error::Error>> {
    let session = crate::wire::session_id("ses_000000000001")?;
    let inner = ProgressPeer {
        inner: ContractDouble::new(),
        page: progress_page(true)?,
        frames: std::collections::VecDeque::new(),
        reads: 0,
        subscriptions: 0,
        closed: false,
    };
    let mut client = Client::new(
        HeldTools {
            inner,
            reply: None,
            starts: 0,
            cancels: 0,
        },
        Bounds::PRODUCTION,
    );
    client.selected = Some(session.clone());
    client.cursor.adopt_baseline(&session, 0);
    client
        .views
        .insert(session.clone(), SessionView::new(&session));
    client.peer_mut().inner.frames.push_back(hint());
    Ok(client)
}

#[test]
fn held_tool_read_yields_to_live_text_and_never_uses_synchronous_history(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = fixture()?;
    client.pump()?;
    assert_eq!(client.peer_mut().starts, 1);
    for kind in ["session.text.started", "session.text.delta"] {
        let mut frame = serde_json::json!({"id":"evt_text", "type":kind, "created":1,
            "data":{"sessionID":"ses_000000000001", "assistantMessageID":"msg_fixture_answer","ordinal":0, "delta":"UNFINISHED"}});
        if kind == "session.text.started" {
            frame["durable"] =
                serde_json::json!({"aggregateID":"ses_000000000001", "seq":1, "version":1});
        }
        client
            .peer_mut()
            .inner
            .frames
            .push_back(FramePoll::Frame(frame.to_string()));
    }
    for _ in 0..20 {
        client.pump()?;
    }
    assert_eq!(
        client.peer_mut().starts,
        1,
        "pending polls must not replay requests"
    );
    assert_eq!(client.peer_mut().inner.reads, 0);
    assert!(client
        .view()
        .ok_or("view")?
        .slices
        .iter()
        .any(|row| row.text.contains("UNFINISHED")));
    let body = newest_first(&progress_page(true)?)?;
    client.peer_mut().reply = Some(Ok(crate::transport::WireResponse { status: 200, body }));
    client.pump()?;
    let view = client.view().ok_or("view")?;
    assert!(view
        .slices
        .iter()
        .any(|row| row.text.contains("UNFINISHED")));
    assert_eq!(
        view.slices
            .iter()
            .filter(|row| row.tool_id.is_some())
            .count(),
        1
    );
    Ok(())
}

#[test]
fn quiet_tool_reads_schedule_wakes_until_the_reply_then_return_to_timerless_idle(
) -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let mut client = fixture()?;
    assert_eq!(client.recovery_wait_at(Duration::ZERO), None);
    client.pump_at(Duration::ZERO)?;
    for tick in 0..3 {
        let now = Duration::from_millis(tick * 10);
        let delay = client
            .recovery_wait_at(now)
            .ok_or("quiet tool read lost its next backend wake")?;
        assert_eq!(delay, Duration::from_millis(10));
        client.pump_at(now.saturating_add(delay))?;
    }
    assert_eq!(client.peer_mut().starts, 1);
    assert_eq!(client.peer_mut().inner.reads, 0);
    assert!(client.peer_mut().inner.frames.is_empty());
    let body = newest_first(&progress_page(true)?)?;
    client.peer_mut().reply = Some(Ok(crate::transport::WireResponse { status: 200, body }));
    let now = Duration::from_millis(30);
    let delay = client
        .recovery_wait_at(now)
        .ok_or("ready reply needs a scheduled pump without another event")?;
    client.pump_at(now.saturating_add(delay))?;
    assert_eq!(
        client
            .view()
            .ok_or("view")?
            .slices
            .iter()
            .filter(|row| row.tool_id.is_some())
            .count(),
        1
    );
    assert_eq!(client.recovery_wait_at(now.saturating_add(delay)), None);
    assert_eq!(client.peer_mut().starts, 1);
    Ok(())
}

#[test]
fn canceling_a_quiet_tool_read_removes_its_wake() -> Result<(), Box<dyn std::error::Error>> {
    use std::time::Duration;

    let mut client = fixture()?;
    client.pump_at(Duration::ZERO)?;
    assert_eq!(
        client.recovery_wait_at(Duration::ZERO),
        Some(Duration::from_millis(10))
    );
    client.cancel_tool_refresh();
    assert_eq!(client.recovery_wait_at(Duration::ZERO), None);
    assert_eq!(client.peer_mut().cancels, 1);
    Ok(())
}

#[test]
fn hinted_read_timeout_recovers_but_authentication_and_protocol_errors_stay_explicit(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = fixture()?;
    client.pump()?;
    client.peer_mut().reply = Some(Err(TransportError::Unavailable {
        reason: "fixture read deadline".into(),
    }));
    client.pump()?;
    assert!(client.recovery.history.is_some());
    assert!(!client.stream_state().is_live());
    assert_eq!(client.peer_mut().starts, 1);
    assert!(client.peer_mut().cancels > 0);
    for (status, body) in [
        (401, Vec::new()),
        (403, Vec::new()),
        (200, b"invalid page".to_vec()),
    ] {
        let mut client = fixture()?;
        client.pump()?;
        client.peer_mut().reply = Some(Ok(crate::transport::WireResponse { status, body }));
        assert!(client.pump().is_err(), "{status} must remain explicit");
    }
    Ok(())
}

#[test]
fn terminal_history_preempts_and_cancels_a_pending_tool_snapshot(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = fixture()?;
    client.pump()?;
    let cancels = client.peer_mut().cancels;
    client.request_history();
    assert!(client.tool_refresh.is_none());
    assert_eq!(client.peer_mut().cancels, cancels + 1);
    assert_eq!(client.peer_mut().starts, 1);
    Ok(())
}

#[test]
fn continuous_valid_hints_cannot_starve_ready_tool_responses(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = fixture()?;
    client.pump()?;
    let body = newest_first(&progress_page(true)?)?;
    for _ in 0..20 {
        client.peer_mut().inner.frames.push_back(hint());
        client.peer_mut().reply = Some(Ok(crate::transport::WireResponse {
            status: 200,
            body: body.clone(),
        }));
        client.pump()?;
        assert_eq!(
            client
                .view()
                .ok_or("view")?
                .slices
                .iter()
                .filter(|row| row.tool_id.is_some())
                .count(),
            1,
            "a ready response must install even when a newer hint is queued"
        );
    }
    assert!(
        client.peer_mut().starts <= 11,
        "follow-up hints must coalesce"
    );
    assert_eq!(
        client.peer_mut().cancels,
        0,
        "progress must not cancel acquisition"
    );
    Ok(())
}
