//! Operator revocation preserves the selected exact resource across wrapped rows.
use grapt_client::contract::Operation;
use grapt_client::transport::{FramePoll, Transport, TransportError, WireRequest, WireResponse};
use grapt_client::{Bounds, Client, Connection, ContractDouble};
use serde_json::{json, Value};

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    resource: String,
    revoked: Vec<Value>,
    prompts: usize,
}

impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        let body = match request.operation() {
            Operation::SandboxGrantList => {
                let grants = if self.revoked.len() < 2 {
                    vec![json!({"scope":"session","resource":self.resource})]
                } else {
                    Vec::new()
                };
                json!({"data":grants})
            }
            Operation::SandboxGrantRevoke => {
                let request: Value = serde_json::from_slice(request.body().unwrap_or_default())
                    .map_err(|error| TransportError::Io {
                        reason: error.to_string(),
                    })?;
                self.revoked.push(request);
                json!({"state":if self.revoked.len() == 1 {"pending"} else {"complete"},
                    "message":if self.revoked.len() == 1 {"Withdrawal pending"} else {"Consent withdrawn"}})
            }
            Operation::SessionPrompt => {
                self.prompts += 1;
                return self.inner.call(request);
            }
            _ => return self.inner.call(request),
        };
        Ok(WireResponse {
            status: 200,
            body: serde_json::to_vec(&body).map_err(|error| TransportError::Io {
                reason: error.to_string(),
            })?,
        })
    }
}

impl Connection for Peer {
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
}

#[test]
fn native_permissions_keys_revoke_the_exact_resource_and_refuse_a_stale_session(
) -> Result<(), Box<dyn std::error::Error>> {
    use crate::{
        backend::{commands, Backend, Command},
        keys::handle_key,
        ui::Ui,
    };
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use std::sync::mpsc;
    let resource = format!("/workspace/{}literal[*].txt", "long-folder/".repeat(30));
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            resource: resource.clone(),
            revoked: Vec::new(),
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
        .find(|row| row.enabled)
        .cloned()
        .ok_or("model")?;
    client.select_model(&model)?;
    let session = client.create_session()?;
    let (sender, receiver) = crate::command_channel();
    let updates = crate::backend::Handoff::default();
    let (wake, _) = mpsc::sync_channel(1);
    let backend = Backend {
        commands: sender,
        updates,
        wake,
    };
    let mut ui = Ui::new(false);
    ui.launch.readiness = crate::launch::Readiness::Ready;
    ui.prompt_ready = true;
    ui.session_update(Some(session.clone()), false);
    ui.composer.insert_str("/permissions");
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    let text = loop {
        match receiver.try_recv()? {
            Command::Submit(text) => break text,
            Command::Commands => {}
            other => return Err(format!("Unexpected operator command: {other:?}").into()),
        }
    };
    let done = commands::execute(&mut client, &text).ok_or("permissions command missing")?;
    ui.command_update(done);
    assert!(ui.grants_pane.is_some());
    handle_key(
        KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    handle_key(
        KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE),
        &mut ui,
        &backend,
    )?;
    let Command::RevokeGrant(request) = receiver.try_recv()? else {
        return Err("No operator revocation".into());
    };
    assert_eq!(request.session, session);
    let stale = super::Request {
        session: request.session.clone(),
        grant: request.grant.clone(),
    };
    ui.command_update(commands::revoke_grant_now(&mut client, request));
    assert!(client.sandbox_grants()?.is_some_and(|rows| rows.is_empty()));
    assert_eq!(
        client.peer_mut().revoked,
        vec![json!({"scope":"session","resource":resource}); 2]
    );
    client.create_session()?;
    let refused = commands::revoke_grant_now(&mut client, stale);
    assert!(refused.notice.contains("Session changed"));
    assert_eq!(client.peer_mut().revoked.len(), 2);
    assert_eq!(
        client.peer_mut().prompts,
        0,
        "Operator revocation submitted a model prompt"
    );
    Ok(())
}
