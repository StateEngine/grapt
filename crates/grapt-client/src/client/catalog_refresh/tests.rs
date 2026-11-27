use super::*;
use crate::double::ContractDouble;
use crate::wire::Spelled;
use std::sync::atomic::{AtomicU64, Ordering};
mod commands;
mod creation;
mod drafts;
mod repository;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
static NEXT: AtomicU64 = AtomicU64::new(0);

struct Profile(PathBuf);
impl Profile {
    fn new() -> Result<Self> {
        let n = NEXT.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!("grapt-catalog-{}-{n}", std::process::id()));
        std::fs::create_dir(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    saved: Vec<u8>,
    pending: Option<WireRequest>,
    blocked: bool,
    enabled: bool,
    fail: bool,
    prompts: usize,
    catalog_starts: usize,
    cancellations: usize,
}
fn is_catalog(operation: Operation) -> bool {
    matches!(
        operation,
        Operation::AgentList | Operation::ModelList | Operation::ModelDefault
    )
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> std::result::Result<WireResponse, TransportError> {
        assert!(
            !(self.blocked && is_catalog(request.operation())),
            "catalog read held up startup"
        );
        if request.operation() == Operation::SessionGet {
            return Ok(WireResponse {
                status: 200,
                body: self.saved.clone(),
            });
        }
        if request.operation() == Operation::SessionPrompt {
            self.prompts += 1;
        }
        self.inner.call(request)
    }
}
impl Connection for Peer {
    fn subscribed(&self) -> bool {
        self.inner.subscribed()
    }
    fn resubscribe(&mut self) {
        self.inner.resubscribe();
    }
    fn poll_frame(&mut self) -> std::result::Result<FramePoll, TransportError> {
        self.inner.poll_frame()
    }
    fn begin_catalog(
        &mut self,
        request: &WireRequest,
    ) -> std::result::Result<Option<WireResponse>, TransportError> {
        assert!(
            self.pending.is_none(),
            "only one catalog request may be pending"
        );
        self.catalog_starts += 1;
        self.pending = Some(request.clone());
        Ok(None)
    }
    fn poll_catalog(&mut self) -> std::result::Result<Option<WireResponse>, TransportError> {
        if self.blocked {
            return Ok(None);
        }
        let request = self.pending.take().ok_or(TransportError::NotConnected)?;
        if self.fail {
            return Ok(Some(WireResponse {
                status: 503,
                body: Vec::new(),
            }));
        }
        let mut response = self.inner.call(&request)?;
        if request.operation() == Operation::ModelList {
            let mut body: serde_json::Value = serde_json::from_slice(&response.body)
                .unwrap_or_else(|error| panic!("model fixture: {error}"));
            body["data"][0]["name"] = serde_json::json!("Refreshed model");
            body["data"][0]["enabled"] = serde_json::json!(self.enabled);
            response.body =
                serde_json::to_vec(&body).unwrap_or_else(|error| panic!("model fixture: {error}"));
        }
        Ok(Some(response))
    }
    fn cancel_catalog(&mut self) {
        if self.pending.take().is_some() {
            self.cancellations += 1;
        }
    }
}

fn fixture(cached: bool) -> Result<(Client<Peer>, SessionWireId, Profile)> {
    let mut inner = ContractDouble::new();
    let session = inner.create_session("Prepared").ok_or("session bound")?;
    let mut client = Client::new(
        Peer {
            inner,
            saved: Vec::new(),
            pending: None,
            blocked: false,
            enabled: true,
            fail: false,
            prompts: 0,
            catalog_starts: 0,
            cancellations: 0,
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let project = client.project().ok_or("project")?.clone();
    let agent = client.agent().ok_or("agent")?.clone();
    let model = client.model().ok_or("model")?.clone();
    let mut info = serde_json::to_value(crate::wire::session_info(
        &session,
        "Prepared",
        &project.worktree,
        None,
    )?)?;
    info["projectID"] = serde_json::json!(project.id.spelled());
    info["agent"] = serde_json::json!(agent.spelled());
    info["model"] =
        serde_json::json!({"id":model.id.spelled(), "providerID":model.provider_id.spelled()});
    client.connection.saved = serde_json::to_vec(&serde_json::json!({"data":info}))?;
    let profile = Profile::new()?;
    if cached {
        let mut bodies = Vec::new();
        for operation in [
            Operation::AgentList,
            Operation::ModelList,
            Operation::ModelDefault,
        ] {
            let request = WireRequest::new(operation, None, None)?
                .with_query("location[directory]", &project.worktree);
            bodies.push(client.connection.inner.call(&request)?.body);
        }
        let snapshot = Snapshot {
            agents: bodies.remove(0),
            models: bodies.remove(0),
            default: bodies.remove(0),
            choice: Choice::current(&client),
        };
        disk::save(
            &profile.0,
            &project,
            &snapshot,
            client.bounds.max_response_bytes,
        )?;
    }
    client.set_catalog_profile(&profile.0);
    client.connection.blocked = true;
    Ok((client, session, profile))
}

#[test]
fn a_cache_miss_restores_the_saved_selection_after_refresh() -> Result {
    let (mut client, session, profile) = fixture(false)?;
    let project = client.project().ok_or("project")?;
    let request = WireRequest::new(Operation::AgentList, None, None)?
        .with_query("location[directory]", &project.worktree);
    let agent = crate::wire::decode_agents(&client.connection.inner.call(&request)?.body)?
        .into_iter()
        .find(|row| row.hidden)
        .ok_or("hidden agent")?
        .id;
    let mut saved: serde_json::Value = serde_json::from_slice(&client.connection.saved)?;
    saved["data"]["agent"] = serde_json::json!(agent.spelled());
    client.connection.saved = serde_json::to_vec(&saved)?;
    client.attach_launch_session(&session)?;
    assert!(!client.ready_to_chat());
    client.connection.blocked = false;
    for _ in 0..6 {
        client.advance_catalog();
    }
    assert!(client.ready_to_chat());
    assert_eq!(client.agent(), Some(&agent));
    assert!(!client.agents().iter().any(|row| row.id == agent));
    assert!(client.select_agent(&agent).is_err());
    assert_eq!(
        client.model().map(|m| m.name.as_str()),
        Some("Refreshed model")
    );
    let project = client.project().ok_or("project")?;
    assert!(disk::load(&profile.0, project, client.bounds.max_response_bytes)?.is_some());
    assert_eq!(client.connection.prompts, 0);
    Ok(())
}

#[test]
fn refresh_failure_keeps_the_cached_choice_and_a_disabled_refresh_does_not_default() -> Result {
    for fail in [true, false] {
        let (mut client, session, _profile) = fixture(true)?;
        client.attach_launch_session(&session)?;
        let chosen = client.model().cloned();
        client.connection.blocked = false;
        client.connection.fail = fail;
        client.connection.enabled = false;
        for _ in 0..6 {
            client.advance_catalog();
        }
        if fail {
            assert_eq!(client.model(), chosen.as_ref());
            assert!(client
                .catalog_status()
                .is_some_and(|n| n.contains("failed")));
        } else {
            assert!(client.model().is_none());
            assert!(!client.ready_to_chat());
        }
    }
    Ok(())
}

#[test]
fn changing_project_cancels_the_old_refresh_and_cannot_reuse_its_cache() -> Result {
    let (mut client, session, _profile) = fixture(true)?;
    client.attach_launch_session(&session)?;
    client.advance_catalog();
    assert!(client.connection.pending.is_some());
    let mut other = client.project().ok_or("project")?.clone();
    other.worktree.push_str("/different");
    client.select_project(&other)?;
    assert_eq!(client.connection.cancellations, 1);
    assert!(client.models().is_empty());
    assert!(client.agent().is_none());
    client.advance_catalog();
    assert!(client.connection.pending.as_ref().is_some_and(|r| r
        .query()
        .iter()
        .any(|(k, v)| k == "location[directory]" && v == &other.worktree)));
    Ok(())
}

#[test]
fn corrupt_or_oversized_cache_is_visible_and_never_prevents_typing() -> Result {
    for bytes in [b"invalid".as_slice(), &[b'x'; 40_000]] {
        let (mut client, session, profile) = fixture(false)?;
        std::fs::write(profile.0.join("tui-catalog.json"), bytes)?;
        client.attach_launch_session(&session)?;
        assert!(client.models().is_empty());
        assert!(client.catalog_status().is_some_and(|n| n.contains("cache")));
    }
    Ok(())
}
