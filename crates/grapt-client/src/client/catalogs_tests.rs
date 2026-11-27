//! Startup questions remain usable while an MCP catalog read is pending.
use super::*;
use crate::{wire, ContractDouble};

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    pending: Option<WireRequest>,
    location: Option<wire::ProjectRow>,
    answered: bool,
    form_id: &'static str,
    human_wait: bool,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        assert!(
            !matches!(
                request.operation(),
                Operation::McpList | Operation::McpInspect
            ),
            "MCP inspection must not block the form worker"
        );
        if request.operation() == Operation::FormRequestList {
            let body = serde_json::json!({"data":{"id":self.form_id,"sessionID":"global","title":"External interaction","metadata":{"kind":"mcp-url-elicitation","server":"ask","startup":true,"startupPending":!self.answered,"message":"Connect","host":"example.test","url":"https://example.test/connect?secret=human-only","elicitationId":"external-9"},"fields":[{"key":"decision","type":"string","required":true,"custom":false,"options":[{"value":"accept","label":"Continue manually"},{"value":"decline","label":"Decline"}]}]}});
            let row = wire::decode_form(
                &serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?,
            )
            .map_err(|_| TransportError::NotConnected)?;
            return Ok(WireResponse {
                status: 200,
                body: wire::encode_form_requests(
                    self.location.as_ref().ok_or(TransportError::NotConnected)?,
                    &[row],
                )
                .map_err(|_| TransportError::NotConnected)?,
            });
        }
        if request.operation() == Operation::GlobalFormReply {
            let answer =
                wire::decode_form_reply(request.body().ok_or(TransportError::NotConnected)?)
                    .map_err(|_| TransportError::NotConnected)?;
            assert_eq!(
                answer,
                BTreeMap::from([("decision".into(), wire::FormValue::Text("accept".into()))])
            );
            self.answered = true;
            return Ok(WireResponse {
                status: 200,
                body: b"{}".to_vec(),
            });
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn begin_operator(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        if request.operation() == Operation::McpConfiguration {
            let body: serde_json::Value =
                serde_json::from_slice(request.body().ok_or(TransportError::NotConnected)?)
                    .map_err(|_| TransportError::NotConnected)?;
            assert_eq!(
                body,
                serde_json::json!({"action":"set_enabled","enabled":true})
            );
            return Ok(Some(WireResponse {
                status: 200,
                body: b"{}".to_vec(),
            }));
        }
        assert!(self.pending.is_none());
        self.pending = Some(request.clone());
        Ok(None)
    }
    fn poll_operator(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if !self.answered {
            return Ok(None);
        }
        self.inner
            .call(&self.pending.take().ok_or(TransportError::NotConnected)?)
            .map(Some)
    }
    fn cancel_operator(&mut self) {
        self.pending = None;
    }
    fn operator_human_wait(&mut self, waiting: bool) {
        self.human_wait = waiting;
    }
    fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
        Ok(FramePoll::Idle)
    }
    fn resubscribe(&mut self) {}
    fn subscribed(&self) -> bool {
        true
    }
}

#[test]
fn pending_mcp_inspection_allows_presession_answers_and_fences_navigation(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            pending: None,
            location: None,
            answered: false,
            form_id: "frm_startup",
            human_wait: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    client.peer_mut().location = Some(project.clone());
    client.refresh_mcp_inspection()?;
    assert!(client.mcp_catalog_pending());
    assert!(client.selected().is_none());
    let request = client.peer_mut().pending.as_ref().ok_or("pending")?;
    assert!(request
        .query()
        .iter()
        .any(|(key, value)| key == "location[directory]" && value == &project.worktree));
    assert!(!request.query().iter().any(|(key, _)| key == "session"));
    assert!(
        client.refresh_mcp().is_err(),
        "one bounded MCP operation at a time"
    );
    client.pump()?;
    assert!(client.mcp_catalog_pending());
    client.refresh_location_forms()?;
    let form = client
        .location_forms()
        .first()
        .cloned()
        .ok_or("startup question")?;
    let location = client.form_location().ok_or("location")?;
    client.answer_location_form(
        &location,
        &form.id,
        &BTreeMap::from([("decision".into(), wire::FormValue::Text("accept".into()))]),
    )?;
    client.pump()?;
    assert!(!client.mcp_catalog_pending());
    assert!(!client.mcp_servers().is_empty());
    client.refresh_mcp()?;
    let session = client
        .peer_mut()
        .inner
        .create_session("Navigation")
        .ok_or("session")?;
    client.select(&session)?;
    assert!(!client.mcp_catalog_pending());
    assert!(client.peer_mut().pending.is_none());
    client.refresh_mcp_inspection()?;
    let request = client.peer_mut().pending.as_ref().ok_or("pending")?;
    assert!(request.query().iter().any(|(key, _)| key == "session"));
    assert!(!request
        .query()
        .iter()
        .any(|(key, _)| key == "location[directory]"));
    let next = client
        .projects()
        .iter()
        .find(|row| *row != &project)
        .cloned()
        .ok_or("other project")?;
    client.select_project(&next)?;
    assert!(!client.mcp_catalog_pending());
    assert!(client.peer_mut().pending.is_none());
    assert!(client.mcp_servers().is_empty());
    Ok(())
}

#[test]
fn mcp_inspection_and_side_questions_share_one_cancellable_operator_lane(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            pending: None,
            location: None,
            answered: false,
            form_id: "frm_startup",
            human_wait: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    let session = client
        .peer_mut()
        .inner
        .create_session("Side")
        .ok_or("session")?;
    client.select(&session)?;
    client.refresh_mcp_inspection()?;
    assert!(client.operator_request_pending());
    assert!(client
        .begin_side_question("Explain the current change")
        .is_err());
    assert!(client.mcp_catalog_pending());
    client.cancel_operator_request();
    assert!(!client.mcp_catalog_pending());
    assert!(client.peer_mut().pending.is_none());
    assert!(client
        .begin_side_question("Explain the current change")?
        .is_none());
    assert!(client.refresh_mcp_inspection().is_err());
    assert!(client.side_question_pending());
    client.cancel_operator_request();
    assert!(!client.operator_request_pending());
    assert!(client.peer_mut().pending.is_none());
    Ok(())
}

#[test]
fn enable_catalog_refresh_accounts_only_its_new_startup_human_wait(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            pending: None,
            location: None,
            answered: false,
            form_id: "frm_stale",
            human_wait: false,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    client.peer_mut().location = Some(project);
    client.refresh_location_forms()?;
    assert!(client.begin_mcp_enabled("ask", true)?.is_none());
    assert!(client.poll_mcp_auth()?.is_none());
    assert!(
        !client.peer_mut().human_wait,
        "stale form paused a new acquisition"
    );
    client.peer_mut().form_id = "frm_fresh";
    client.refresh_location_forms()?;
    assert!(client.poll_mcp_auth()?.is_none());
    assert!(
        client.peer_mut().human_wait,
        "enable refresh charged human time as active time"
    );
    let form = client
        .location_forms()
        .first()
        .ok_or("startup form")?
        .id
        .clone();
    client.answer_location_form(
        &client.form_location().ok_or("location")?,
        &form,
        &BTreeMap::from([("decision".into(), wire::FormValue::Text("accept".into()))]),
    )?;
    assert!(client.poll_mcp_auth()?.is_some());
    assert!(!client.peer_mut().human_wait);
    assert!(!client.mcp_auth_pending());
    assert!(client.mcp_refusal().is_none());
    Ok(())
}

#[test]
fn primary_picker_excludes_subagent_only_rows_but_preserves_saved_sessions(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    let rows = [
        ("build", grapt_api::u3a::AgentInfoDtoMode::Primary),
        ("worker", grapt_api::u3a::AgentInfoDtoMode::Subagent),
        ("custom", grapt_api::u3a::AgentInfoDtoMode::All),
    ]
    .into_iter()
    .map(|(name, mode)| {
        Ok(wire::AgentRow {
            id: wire::agent_id(name)?,
            name: name.into(),
            description: None,
            mode,
            hidden: false,
            source: None,
        })
    })
    .collect::<Result<Vec<_>, wire::WireError>>()?;
    let child = wire::agent_id("worker")?;
    client.install_agents(rows);
    assert_eq!(client.agents().len(), 2);
    assert!(client.agents().iter().all(|agent| agent.id != child));
    assert!(client.saved_agent_offered(&child));
    Ok(())
}
