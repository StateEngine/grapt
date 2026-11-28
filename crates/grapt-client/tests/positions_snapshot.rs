//! Only a positioned connect proves a stream; an incomplete snapshot is retried.
use grapt_client::{
    double::ContractDouble,
    wire::{SessionWireId, Spelled},
    Bounds, Client, Connection, FramePoll, Operation, Transport, TransportError, WireRequest,
    WireResponse,
};
use serde_json::json;
use std::{collections::VecDeque, time::Duration};
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<FramePoll>,
    snapshot: WireResponse,
    snapshots: usize,
    page_reads: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionSnapshot {
            self.snapshots += 1;
            return Ok(self.snapshot.clone());
        }
        if request.operation() == Operation::MessageList {
            self.page_reads += 1;
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(self.frames.pop_front().unwrap_or(FramePoll::Idle))
    }
    fn resubscribe(&mut self) {}
    fn subscribed(&self) -> bool {
        true
    }
}
fn client(first: &'static str) -> TestResult<(Client<Peer>, SessionWireId)> {
    let mut inner = ContractDouble::new();
    let session = inner
        .create_session("snapshot mode")
        .ok_or("session bound")?;
    let frames = [first, "server.connected"]
        .into_iter()
        .enumerate()
        .map(|(index, kind)| FramePoll::Positioned {
            id: format!("epoch-{index}"),
            data: json!({"id":format!("evt_mode_{index}"),"type":kind,"data":{}}).to_string(),
        })
        .collect();
    let mut client = Client::new(
        Peer {
            inner,
            frames,
            snapshot: WireResponse {
                status: 503,
                body: serde_json::to_vec(&json!({
                    "revision":{"aggregate":session.spelled(),"seq":100},"rows":[],
                    "cursor":{},"permissions":[],"forms":[],"execution":{},"provisional":[]
                }))?,
            },
            snapshots: 0,
            page_reads: 0,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client
        .models()
        .iter()
        .find(|row| row.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    client.pump_at(Duration::ZERO)?;
    Ok((client, session))
}

#[test]
fn snapshot_503_retries_without_reading_a_message_page() -> TestResult {
    let (mut client, session) = client("server.connected")?;
    client.select(&session)?;
    for tick in 1..=5 {
        client.pump_at(Duration::from_secs(tick))?;
    }
    assert!(client.peer_mut().snapshots >= 2, "no bounded retry");
    assert_eq!(client.peer_mut().page_reads, 0);
    let before = client.peer_mut().snapshots;
    client.peer_mut().snapshot.status = 200;
    for tick in 6..=15 {
        client.pump_at(Duration::from_secs(tick))?;
    }
    assert!(client.peer_mut().snapshots > before);
    assert!(
        client.view().is_some(),
        "a recovered snapshot never installed"
    );
    assert_eq!(client.peer_mut().page_reads, 0);
    Ok(())
}

#[test]
fn a_first_frame_that_is_not_the_connect_proves_nothing() -> TestResult {
    let (mut client, session) = client("server.heartbeat")?;
    client.select(&session)?;
    assert_eq!(
        client.peer_mut().snapshots,
        0,
        "no read on an unproven stream"
    );
    assert_eq!(
        client.peer_mut().page_reads,
        0,
        "and no message page instead"
    );
    Ok(())
}
