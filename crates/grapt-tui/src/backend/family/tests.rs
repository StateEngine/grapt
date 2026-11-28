use super::*;
use grapt_client::{
    transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse},
    wire::{assistant_message, encode_messages, DurableFinish, MessageItem, Spelled},
    Bounds, Connection, ContractDouble, Operation,
};

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    finished: Vec<(SessionWireId, DurableFinish)>,
}

impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if !matches!(
            request.operation(),
            Operation::MessageList | Operation::SessionSnapshot
        ) {
            return self.inner.call(request);
        }
        let rows = self
            .finished
            .iter()
            .find(|(id, _)| request.path().contains(id.spelled()))
            .map(|(_, finish)| {
                assistant_message(
                    "msg_family_result",
                    &[MessageItem::Text("saved child transcript".into())],
                    Some(*finish),
                    None,
                    None,
                )
                .map(|row| vec![row])
            })
            .transpose()
            .map_err(|_| TransportError::NotConnected)?
            .unwrap_or_default();
        let mut body = encode_messages(rows, None).map_err(|_| TransportError::NotConnected)?;
        if request.operation() == Operation::SessionSnapshot {
            body = grapt_client::double::page_snapshot(request, &body, 0)
                .map_err(|_| TransportError::NotConnected)?;
        }
        Ok(WireResponse { status: 200, body })
    }
}

impl Connection for Peer {
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}

#[test]
fn saved_child_completion_retires_without_live_events_and_keeps_transcript_access(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut inner = ContractDouble::new();
    let main = inner.create_session("parent").ok_or("main")?;
    let completed = inner.create_child_session("done", &main).ok_or("child")?;
    let failed = inner.create_child_session("failed", &main).ok_or("child")?;
    let waiting = inner
        .create_child_session("waiting", &main)
        .ok_or("child")?;
    let mut client = Client::new(
        Peer {
            inner,
            finished: vec![
                (completed.clone(), DurableFinish::Stop),
                (failed.clone(), DurableFinish::Error),
                (waiting.clone(), DurableFinish::ToolCalls),
            ],
        },
        Bounds::PRODUCTION,
    );
    client.enable_paged_history();
    client.select(&main)?;
    client.refresh_sessions()?;
    let mut family = Family::default();
    for _ in 0..client.sessions().len() {
        if !family.refresh(&mut client) {
            break;
        }
    }
    let rows: Vec<_> = client
        .sessions()
        .iter()
        .map(|row| Agent {
            display_name: None,
            row: row.clone(),
            status: family.status(&client, row),
            current: row.id == main,
            context: family.context(&client, row),
        })
        .collect();
    let status = |id: &SessionWireId| {
        rows.iter()
            .find(|row| &row.row.id == id)
            .map(|row| row.status.as_str())
    };
    assert_eq!(status(&completed), Some("completed"));
    assert_eq!(status(&failed), Some("failed"));
    assert_eq!(
        status(&waiting),
        Some("unreported"),
        "a tool-call step is not a finished child"
    );
    assert_eq!(
        client.selected(),
        Some(&main),
        "family reads must not switch sessions"
    );
    let mut switcher = crate::subagents::Switcher::default();
    switcher.observe(&rows, tokio::time::Instant::now());
    assert!(
        switcher.height(&rows, 40) > 0,
        "waiting and unseen failure stay reachable"
    );
    assert!(client.sessions().iter().any(|row| row.id == completed));
    client.select(&completed)?;
    // The stream's positioned connect, then the snapshot it permits.
    for _ in 0..2 {
        client.pump()?;
    }
    assert_eq!(client.selected(), Some(&completed));
    assert!(matches!(
        client.view().ok_or("view")?.execution,
        grapt_client::ExecutionState::Settled {
            outcome: grapt_client::Outcome::Succeeded,
            ..
        }
    ));
    Ok(())
}
