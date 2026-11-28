//! A notification between preparing a form read and consuming its answer.
use crate::{
    Bounds, Client, Connection, ContractDouble, FramePoll, Operation, PeriodicRead, Transport,
    TransportError, WireRequest, WireResponse,
};
use std::collections::VecDeque;
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    frames: VecDeque<String>,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> std::result::Result<WireResponse, TransportError> {
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn poll_frame(&mut self) -> std::result::Result<FramePoll, TransportError> {
        match self.frames.pop_front() {
            Some(frame) => Ok(self.inner.serve(FramePoll::Frame(frame))),
            None => self.inner.poll_frame(),
        }
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
}
#[test]
fn review_periodic_a_form_hint_pumped_before_an_old_answer_still_requests_a_fresh_read() -> Result {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            frames: VecDeque::new(),
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    let session = client.create_session()?;
    for _ in 0..8 {
        if !client.pump()? {
            break;
        }
    }
    client.refresh_forms()?;
    assert!(!client.take_session_form_refresh());
    let prepared = client
        .prepare_read(PeriodicRead::SessionForms)?
        .ok_or("read")?;
    let response = client.peer_mut().call(&prepared.request)?;
    let form = client
        .peer_mut()
        .inner
        .raise_form(&session, "Created during the read")
        .ok_or("form")?;
    let request = WireRequest::new(Operation::SessionFormList, Some(&session), None)?;
    let rows: serde_json::Value = serde_json::from_slice(&client.peer_mut().call(&request)?.body)?;
    let row = rows["data"]
        .as_array()
        .and_then(|rows| rows.first())
        .ok_or("form row")?
        .clone();
    let frame = serde_json::json!({"id":"evt_read_interleaving","type":"form.created","created":1,
        "durable":{"version":1,"seq":1,"aggregateID":session},"data":{"sessionID":session,"form":row}}).to_string();
    assert!(crate::wire::session_form_changed(&frame, &session));
    client.peer_mut().frames.push_back(frame);
    assert!(client.pump()?);
    // The host pumps first, then consumes the worker's answer in the same pass.
    let applied = client.accept_read(prepared, Ok(response))?;
    let hinted = client.take_session_form_refresh();
    assert!(
        !applied || hinted || client.forms().iter().any(|row| row.id == form),
        "the old answer erased a later form hint: applied={applied}, hinted={hinted}"
    );
    client.refresh_forms()?;
    assert!(client.forms().iter().any(|row| row.id == form));
    Ok(())
}
