use super::*;
use crate::{
    transport::{FramePoll, Transport},
    ContractDouble,
};
use std::time::Duration;

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    request: Option<WireRequest>,
    blocked: bool,
    wrong: bool,
    starts: usize,
    frame: Option<String>,
    startup: Option<(crate::wire::ProjectRow, &'static str)>,
    human_wait: bool,
    resource_updated: Option<bool>,
    inspections: usize,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if let Some((location, id)) = &self.startup {
            if request.operation() == Operation::FormRequestList {
                let form = crate::wire::decode_form(&serde_json::to_vec(&serde_json::json!({
                    "data":{"id":id,"sessionID":"global","title":"Connect",
                    "metadata":{"kind":"mcp-url-elicitation","startup":true,"startupPending":self.blocked},
                    "fields":[{"key":"decision","type":"string","required":true,"custom":true}]}
                })).map_err(|_| TransportError::NotConnected)?)
                .map_err(|_| TransportError::NotConnected)?;
                return Ok(WireResponse {
                    status: 200,
                    body: crate::wire::encode_form_requests(location, &[form])
                        .map_err(|_| TransportError::NotConnected)?,
                });
            }
            if request.operation() == Operation::GlobalFormReply {
                self.blocked = false;
                return Ok(WireResponse {
                    status: 200,
                    body: b"{}".to_vec(),
                });
            }
        }
        if self.resource_updated.is_some() {
            assert!(
                !matches!(
                    request.operation(),
                    Operation::McpContent | Operation::SessionPrompt
                ),
                "update hint fetched resource content or submitted a prompt"
            );
        }
        let mut response = self.inner.call(request)?;
        if request.operation() == Operation::McpInspect {
            self.inspections += 1;
            if let Some(updated) = self.resource_updated {
                let mut body: serde_json::Value = serde_json::from_slice(&response.body)
                    .map_err(|_| TransportError::NotConnected)?;
                body["subscriptions"] =
                    serde_json::json!([{"server":"book","uri":"memo://guide","updated":updated}]);
                response.body =
                    serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
            }
        }
        Ok(response)
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
        match self.frame.take() {
            Some(frame) => Ok(self.inner.serve(FramePoll::Frame(frame))),
            None => self.inner.poll_frame(),
        }
    }
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> Result<Option<WireResponse>, TransportError> {
        assert!(self.request.is_none(), "only one catalog reader");
        self.starts += 1;
        self.request = Some(request.clone());
        Ok(None)
    }
    fn poll_catalog(&mut self) -> Result<Option<WireResponse>, TransportError> {
        if self.blocked {
            return Ok(None);
        }
        let request = self.request.take().ok_or(TransportError::NotConnected)?;
        let mut response = self.inner.call(&request)?;
        if request.operation() == Operation::CommandInspect {
            let mut body: serde_json::Value =
                serde_json::from_slice(&response.body).map_err(|_| TransportError::NotConnected)?;
            body["mcp_scope"] = "a".repeat(64).into();
            if self.wrong {
                body["location"]["directory"] = "/wrong-workspace".into();
            }
            response.body = serde_json::to_vec(&body).map_err(|_| TransportError::NotConnected)?;
        }
        Ok(Some(response))
    }
    fn cancel_catalog(&mut self) {
        self.request = None;
    }
    fn catalog_human_wait(&mut self, waiting: bool) {
        self.human_wait = waiting;
    }
}
#[test]
fn delayed_commands_keep_quiet_stream_awake_and_fence_wrong_locations(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            request: None,
            blocked: true,
            wrong: false,
            starts: 0,
            frame: None,
            startup: None,
            human_wait: false,
            resource_updated: None,
            inspections: 0,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    // Establish the initial stream before checking later catalog hints.
    client.pump()?;
    for _ in 0..8 {
        assert!(!client.advance_commands());
    }
    assert_eq!(
        client.peer_mut().starts,
        0,
        "idle startup needs no commands"
    );
    client.enable_commands();
    assert!(client.advance_commands());
    assert!(!client.advance_commands());
    assert_eq!(client.peer_mut().starts, 1);
    assert!(client.recovery_wait_at(Duration::ZERO).is_some());
    client.peer_mut().wrong = true;
    client.peer_mut().blocked = false;
    assert!(client.advance_commands());
    assert!(client.commands().is_empty());
    assert!(client
        .commands_status()
        .is_some_and(|note| note.contains("selected workspace")));
    client.peer_mut().wrong = false;
    client.refresh_commands();
    client.advance_commands();
    client.advance_commands();
    assert!(client.commands_status().is_none());
    assert!(client.command_catalog.settled);
    // Repeated slash edits must retain a completed catalog and its status.
    let starts = client.peer_mut().starts;
    client.enable_commands();
    assert!(!client.advance_commands());
    assert!(client.commands_status().is_none());
    assert_eq!(client.peer_mut().starts, starts);
    client.open_commands();
    assert!(
        !client.command_catalog.settled,
        "reopening rescans without a native watcher"
    );
    client.advance_commands();
    client.advance_commands();
    assert!(client.command_catalog.settled);
    // A global advisory can invalidate only its selected command owner.
    client.command_catalog.scope = Some("a".repeat(64));
    for (scope, expected) in [("b", false), ("a", true)] {
        client.peer_mut().frame = Some(serde_json::json!({"type":"mcp.catalog.changed","id":"evt_catalog_changed","created":1,"data":{"scope":scope.repeat(64)}}).to_string());
        client.pump()?;
        assert_eq!(!client.command_catalog.settled, expected);
    }
    client.peer_mut().frame = Some(serde_json::json!({"type":"mcp.catalog.changed","id":"evt_catalog_during_read","created":2,"data":{"scope":"a".repeat(64)}}).to_string());
    client.pump()?;
    client.advance_commands();
    assert!(
        !client.command_catalog.settled,
        "an in-flight hint requires one subsequent read"
    );
    client.refresh_commands();
    client.advance_commands();
    client.advance_commands();
    let selected = client.form_location().ok_or("command location")?.directory;
    for (directory, expected) in [("/other-workspace", false), (selected.as_str(), true)] {
        client.peer_mut().frame = Some(
            serde_json::json!({
                "type":"skills.changed","id":"evt_skills_changed","created":3,
                "location":{"directory":directory,"workspaceID":null},"data":{}
            })
            .to_string(),
        );
        client.pump()?;
        assert_eq!(!client.command_catalog.settled, expected);
    }
    // pump() already began the read for the matching hint above. Hold that
    // read open while delivering another change, then release its response.
    client.peer_mut().blocked = true;
    client.peer_mut().frame = Some(
        serde_json::json!({
            "type":"skills.changed","id":"evt_skills_during_read","created":4,
            "location":{"directory":selected,"workspaceID":null},"data":{}
        })
        .to_string(),
    );
    client.pump()?;
    client.peer_mut().blocked = false;
    client.advance_commands();
    assert!(!client.command_catalog.settled);
    Ok(())
}

#[test]
fn command_discovery_accounts_only_its_new_startup_question(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            request: None,
            blocked: true,
            wrong: false,
            starts: 0,
            frame: None,
            startup: None,
            human_wait: false,
            resource_updated: None,
            inspections: 0,
        },
        Bounds::PRODUCTION,
    );
    client.refresh_projects()?;
    let project = client.projects().first().cloned().ok_or("project")?;
    client.select_project(&project)?;
    client.peer_mut().startup = Some((project.clone(), "frm_old_startup"));
    client.refresh_location_forms()?;
    client.enable_commands();
    client.advance_commands();
    client.advance_commands();
    assert!(
        !client.peer_mut().human_wait,
        "old question extended a new request"
    );
    client.peer_mut().startup = Some((project, "frm_new_startup"));
    client.refresh_location_forms()?;
    client.advance_commands();
    assert!(
        client.peer_mut().human_wait,
        "startup human time spent active allowance"
    );
    assert!(client.command_catalog.in_flight());
    let form = client
        .location_forms()
        .first()
        .ok_or("startup question")?
        .id
        .clone();
    client.answer_location_form(
        &client.form_location().ok_or("location")?,
        &form,
        &std::collections::BTreeMap::from([(
            "decision".into(),
            crate::wire::FormValue::Text("decline".into()),
        )]),
    )?;
    client.advance_commands();
    assert!(!client.peer_mut().human_wait);
    assert!(client.command_catalog.settled);
    assert!(client.commands_status().is_none());
    Ok(())
}

#[test]
fn a_resource_hint_refreshes_only_metadata_and_session_selection_hides_old_interests(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            request: None,
            blocked: false,
            wrong: false,
            starts: 0,
            frame: None,
            startup: None,
            human_wait: false,
            resource_updated: Some(false),
            inspections: 0,
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    client.create_session()?;
    // Connection recovery also refreshes commands. Establish this first so
    // the later resource hint measures metadata invalidation independently.
    client.pump()?;
    client.enable_commands();
    client.advance_commands();
    client.advance_commands();
    let command_reads = client.peer_mut().starts;
    client.refresh_mcp_inspection()?;
    assert_eq!(
        client.mcp_watches.first().map(|row| row.updated),
        Some(false)
    );
    client.peer_mut().resource_updated = Some(true);
    client.peer_mut().frame = Some(serde_json::json!({"type":"mcp.resource.changed","id":"evt_resource_update","created":1,"data":{"scope":"a".repeat(64)}}).to_string());
    client.pump()?;
    assert_eq!(client.peer_mut().inspections, 2);
    assert_eq!(client.peer_mut().starts, command_reads);
    assert_eq!(
        client.mcp_watches.first().map(|row| row.updated),
        Some(true)
    );
    assert!(!client.mcp_resource_lines().is_empty());
    client.peer_mut().frame = Some(
        serde_json::json!({"type":"server.connected","id":"evt_reconnected","data":{}}).to_string(),
    );
    client.pump()?;
    assert_eq!(client.peer_mut().starts, command_reads + 1);
    client.advance_commands();
    assert!(client.command_catalog.settled);
    client.create_session()?;
    assert!(client.mcp_resource_lines().is_empty());
    Ok(())
}
