use super::*;
use crate::{wire::Spelled, ContractDouble};
use std::collections::VecDeque;

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<FramePoll>,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for Peer {
    /// The witness's frames on the double's own positioned stream.
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        if let Some(connect) = self.inner.connect() {
            return Ok(connect);
        }
        match self.frames.pop_front() {
            Some(frame) => Ok(self.inner.serve(frame)),
            None => self.inner.poll_frame(),
        }
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        true
    }
}

fn status(session: &SessionWireId, state: &str, stamp: u64) -> FramePoll {
    let status = if state == "retry" {
        serde_json::json!({"type": "retry", "attempt": 1, "message": "provider retry", "next": 1})
    } else {
        serde_json::json!({"type": state})
    };
    FramePoll::Frame(
        serde_json::json!({
            "id": format!("evt_status_{stamp}"), "type": "session.status", "created": stamp,
            "data": { "sessionID": session.spelled(), "status": status }
        })
        .to_string(),
    )
}

/// A status edge for an observed session, carrying its typed state at
/// committed position `revision`.
fn stated(
    session: &SessionWireId,
    status: &str,
    state: serde_json::Value,
    revision: u64,
) -> FramePoll {
    let mut state = state;
    state["revision"] = revision.into();
    FramePoll::Frame(
        serde_json::json!({
            "id": format!("evt_state_{revision}"), "type": "session.status", "created": revision,
            "data": { "sessionID": session.spelled(), "status": {"type": status}, "state": state }
        })
        .to_string(),
    )
}

#[test]
fn all_eight_children_update_without_switching_or_changing_the_selected_transcript(
) -> Result<(), ClientError> {
    let mut inner = ContractDouble::new();
    let parent = inner
        .create_session("parent")
        .unwrap_or_else(|| panic!("parent"));
    let children: Vec<_> = (0..8)
        .map(|n| {
            inner
                .create_child_session(&format!("child {n}"), &parent)
                .unwrap_or_else(|| panic!("child {n}"))
        })
        .collect();
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.refresh_sessions()?;
    client.select(&parent)?;
    crate::client::tests::prove(&mut client);
    let before = client.view().cloned();
    let cursor = client.stream_state().clone();
    let reconciliations = client.diagnostics().reconciliations;
    for (n, child) in children.iter().enumerate() {
        client
            .peer_mut()
            .frames
            .push_back(status(child, "busy", 100 + n as u64));
        client.peer_mut().frames.push_back(FramePoll::Frame(
            serde_json::json!({
                "id": format!("evt_foreign_{n}"), "type": "session.text.delta", "created": 200 + n,
                "durable": {"aggregateID": child.spelled(), "seq": 99, "version": 1},
                "data": {"sessionID": child.spelled(), "delta": "FOREIGN TEXT"}
            })
            .to_string(),
        ));
    }
    assert!(client.pump_at(std::time::Duration::ZERO)?);
    for child in &children {
        assert_eq!(
            client.observed_activity(child),
            Some(SessionActivity::Running)
        );
        assert!(
            client.view_of(child).is_none(),
            "status does not allocate a foreign transcript"
        );
    }
    assert_eq!(client.view().cloned(), before);
    assert_eq!(client.selected(), Some(&parent));
    assert_eq!(client.stream_state(), &cursor);
    assert_eq!(client.diagnostics().reconciliations, reconciliations);
    for (n, child) in children.iter().enumerate() {
        client.peer_mut().frames.push_back(status(
            child,
            if n % 2 == 0 { "idle" } else { "retry" },
            300 + n as u64,
        ));
        client.peer_mut().frames.push_back(status(child, "busy", 1)); // stale observation
    }
    client.pump_at(std::time::Duration::ZERO)?;
    for (n, child) in children.iter().enumerate() {
        assert_eq!(
            client.observed_activity(child),
            Some(if n % 2 == 0 {
                SessionActivity::Idle
            } else {
                SessionActivity::Retrying
            })
        );
    }
    // Unlisted identities cannot grow or displace the bounded picker observations.
    let listed = client.activity.len();
    for n in 0..300 {
        let id = crate::wire::session_id(&format!("ses_unknown_{n}"))?;
        client.peer_mut().frames.push_back(status(&id, "busy", 400));
        client.pump_at(std::time::Duration::ZERO)?;
    }
    assert_eq!(client.activity.len(), listed);
    client.peer_mut().frames.push_back(FramePoll::Closed);
    client.pump_at(std::time::Duration::ZERO)?;
    assert!(
        client.activity.is_empty(),
        "a closed subscription invalidates its live statuses"
    );
    Ok(())
}

#[test]
fn a_nonselected_child_reports_waiting_for_capacity_without_changing_the_parent(
) -> Result<(), ClientError> {
    let mut inner = ContractDouble::new();
    let parent = inner
        .create_session("parent")
        .unwrap_or_else(|| panic!("parent"));
    let child = inner
        .create_child_session("waiting child", &parent)
        .unwrap_or_else(|| panic!("child"));
    let frame = FramePoll::Frame(serde_json::json!({"id":"evt_capacity_wait","type":"session.status","created":1,
        "data":{"sessionID":child.spelled(),"status":{"type":"retry","attempt":0,"message":"Waiting for provider slot","next":1,
        "action":{"reason":"provider_slot","provider":"fixture","title":"Waiting for provider slot","message":"Wait","label":"Wait","queuePosition":2}}}}).to_string());
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::from([frame]),
        },
        Bounds::PRODUCTION,
    );
    client.refresh_sessions()?;
    client.select(&parent)?;
    crate::client::tests::prove(&mut client);
    let before = client.view().cloned();
    client.pump_at(std::time::Duration::ZERO)?;
    assert_eq!(
        client.observed_activity(&child),
        Some(SessionActivity::WaitingForProviderSlot { position: Some(2) })
    );
    assert_eq!(client.view().cloned(), before);
    Ok(())
}

#[test]
fn compaction_phase_is_busy_and_does_not_fabricate_history_or_completion(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut inner = ContractDouble::new();
    let id = inner.create_session("compacting").ok_or("session")?;
    let mut client = Client::new(
        Peer {
            inner,
            frames: VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.refresh_sessions()?;
    client.select(&id)?;
    crate::client::tests::prove(&mut client);
    let before = client.view().ok_or("view")?.slices.clone();
    let running = |reason: Option<&str>| {
        let mut state = serde_json::json!({"type": "running", "executionID": "exe_compaction"});
        if let Some(reason) = reason {
            state["reason"] = reason.into();
        }
        state
    };
    client
        .peer_mut()
        .frames
        .push_back(stated(&id, "busy", running(Some("compacting")), 10));
    client.pump_at(std::time::Duration::ZERO)?;
    assert_eq!(
        client.observed_activity(&id),
        Some(SessionActivity::Compacting)
    );
    assert_eq!(client.view().ok_or("view")?.idle, Some(false));
    assert_eq!(client.view().ok_or("view")?.slices, before);
    client
        .peer_mut()
        .frames
        .push_back(stated(&id, "busy", running(None), 11));
    client.pump_at(std::time::Duration::ZERO)?;
    assert_eq!(
        client.observed_activity(&id),
        Some(SessionActivity::Running)
    );
    client.peer_mut().frames.push_back(stated(
        &id,
        "idle",
        serde_json::json!({"type": "idle"}),
        12,
    ));
    client.pump_at(std::time::Duration::ZERO)?;
    assert_eq!(client.observed_activity(&id), Some(SessionActivity::Idle));
    assert!(!matches!(
        client.view().ok_or("view")?.execution,
        crate::ExecutionState::Settled { .. }
    ));
    Ok(())
}
