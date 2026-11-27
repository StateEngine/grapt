use super::*;

#[derive(Debug)]
struct ForkPeer {
    inner: ContractDouble,
    requests: Vec<serde_json::Value>,
    fail: bool,
    prompts: usize,
}
impl Transport for ForkPeer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionPrompt {
            self.prompts += 1;
        }
        if request.operation() == Operation::SessionRewindPreview {
            let session = request
                .path()
                .split('/')
                .nth(3)
                .ok_or(TransportError::NotConnected)?;
            return Ok(WireResponse { status:200, body:serde_json::json!({"data":{
                "sessionID":session,"messageID":"msg_selected","tip":12,"prompt":"edit this earlier task"
            }}).to_string().into_bytes() });
        }
        if request.operation() == Operation::SessionFork {
            self.requests.push(
                serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
                    .map_err(|_| TransportError::NotConnected)?,
            );
            if self.fail {
                return Ok(WireResponse {
                    status: 503,
                    body: Vec::new(),
                });
            }
            let parent = grapt_client::wire::SessionWireId::parse(
                request
                    .path()
                    .split('/')
                    .nth(3)
                    .ok_or(TransportError::NotConnected)?
                    .to_owned(),
            )
            .ok_or(TransportError::NotConnected)?;
            let child = self
                .inner
                .create_child_session("Fork", &parent)
                .ok_or(TransportError::NotConnected)?;
            return self.inner.call(
                &WireRequest::new(Operation::SessionGet, Some(&child), None)
                    .map_err(|_| TransportError::NotConnected)?,
            );
        }
        self.inner.call(request)
    }
}
impl Connection for ForkPeer {
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
fn fork_opens_a_child_stages_an_earlier_prompt_and_never_replays_a_refusal() -> TestResult {
    let mut client = Client::new(
        ForkPeer {
            inner: ContractDouble::new(),
            requests: Vec::new(),
            fail: false,
            prompts: 0,
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
        .find(|m| m.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    client.create_session()?;
    let parent = client.selected().cloned().ok_or("parent")?;
    let latest = execute(&mut client, "/fork").ok_or("command")?;
    assert!(latest.consume && latest.replacement.is_none());
    assert_ne!(client.selected(), Some(&parent));
    assert_eq!(client.peer_mut().requests[0], serde_json::json!({}));
    let earlier = execute(&mut client, "/fork 2").ok_or("command")?;
    assert!(earlier.consume);
    assert_eq!(
        earlier.replacement.as_deref(),
        Some("edit this earlier task")
    );
    assert_eq!(
        client.peer_mut().requests[1],
        serde_json::json!({"beforeMessageID":"msg_selected"})
    );
    let selected = client.selected().cloned();
    client.peer_mut().fail = true;
    let failed = execute(&mut client, "/fork").ok_or("command")?;
    assert!(!failed.consume && failed.replacement.is_none());
    assert_eq!(client.selected(), selected.as_ref());
    assert_eq!(client.peer_mut().requests.len(), 3);
    for command in ["/fork 0", "/fork 201", "/fork invalid"] {
        assert!(!execute(&mut client, command).ok_or("command")?.consume);
    }
    assert_eq!(client.peer_mut().requests.len(), 3);
    assert_eq!(client.peer_mut().prompts, 0);
    Ok(())
}
