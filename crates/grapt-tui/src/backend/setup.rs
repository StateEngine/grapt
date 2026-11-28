//! Bootstrap only through the shared Client's existing catalog/session owners.
use grapt_client::{
    wire::{AgentRow, SessionWireId},
    Client, ClientError, Connection,
};

mod directory;
pub use directory::Directory;

pub const RETRY_DELAY: std::time::Duration = std::time::Duration::from_secs(1);

/// Paused-startup status that says whether a retry is already scheduled.
pub fn paused(error: &ClientError, retrying: bool) -> String {
    if retrying {
        format!("Startup paused; retrying automatically, Ctrl+O retries now; {error}")
    } else {
        format!("Startup paused. Ctrl+O retry reads; {error}")
    }
}

/// The status line while startup is loading: a saved Session restores its
/// choices, a new draft loads the models.
pub fn loading_status(saved: bool) -> &'static str {
    if saved {
        "Restoring saved choices; draft editable"
    } else {
        "Loading models…; draft editable"
    }
}

pub fn attach<C: Connection>(
    client: &mut Client<C>,
    session: Option<&SessionWireId>,
) -> Result<crate::launch::Readiness, ClientError> {
    if let Some(session) = session {
        return crate::launch::attach(client, session);
    }
    client.refresh_projects()?;
    client.refresh_sessions()?;
    client.begin_directory_draft()?;
    // The directory is explicit; agent/model choice stays with the user.
    // Opening this draft creates no session and sends no prompt.
    Ok(if client.catalog_pending() {
        crate::launch::Readiness::Loading
    } else {
        crate::launch::Readiness::Manual
    })
}

/// Retry a bootstrap read after explicit retry or first provider connection.
pub fn retry<C: Connection>(
    client: &mut Client<C>,
    directory: &mut Option<Directory>,
    readiness: &mut crate::launch::Readiness,
    session: Option<&SessionWireId>,
    saved_directory: Option<&str>,
) -> Result<(), ClientError> {
    if *readiness == crate::launch::Readiness::Unresolved {
        if let Some(directory) = directory {
            directory.retry();
        }
    } else if *readiness == crate::launch::Readiness::Paused {
        // The first offer may be the read that failed; attach checks against it.
        if let (Some(session), Some(saved)) = (session, saved_directory) {
            client.offer_saved_directory(session, saved)?;
        }
        *readiness = attach(client, session)?;
    }
    Ok(())
}

/// Explicit Session navigation owns startup even when the old read is unfinished.
pub fn leave_directory<C: Connection>(client: &mut Client<C>, directory: &mut Option<Directory>) {
    if let Some(mut previous) = directory.take() {
        previous.cancel(client);
    }
}

pub fn roster<C: Connection>(client: &Client<C>) -> Option<Vec<AgentRow>> {
    (!client.catalog_pending()
        && client.selected().is_none()
        && client.agent().is_none()
        && !client.agents().is_empty())
    .then(|| client.agents().to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn opening_a_directory_requires_explicit_agent_and_model_before_creation(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut client = Client::new(
            grapt_client::ContractDouble::new(),
            grapt_client::Bounds::PRODUCTION,
        );
        client.offer_directory("/work/project-a")?;
        assert_eq!(attach(&mut client, None)?, crate::launch::Readiness::Manual);
        assert!(
            client.selected().is_none() && client.agent().is_none() && client.model().is_none()
        );
        assert!(client.create_session().is_err());
        let agent = roster(&client)
            .ok_or("roster")?
            .first()
            .ok_or("agent")?
            .id
            .clone();
        client.select_agent(&agent)?;
        assert!(client.create_session().is_err());
        let model = client.models().first().ok_or("model")?.clone();
        client.select_model(&model)?;
        assert!(client.ready_to_chat());
        assert!(client.selected().is_none());
        client.create_draft_session()?;
        assert!(client.selected().is_some());
        Ok(())
    }
}

#[cfg(test)]
mod uncertain {
    use super::*;
    use grapt_client::{
        Bounds, ContractDouble, FramePoll, Operation, Transport, TransportError, WireRequest,
        WireResponse,
    };
    #[derive(Debug)]
    struct Peer {
        inner: ContractDouble,
        creates: usize,
    }
    impl Transport for Peer {
        fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
            let response = self.inner.call(request)?;
            if request.operation() == Operation::SessionCreate {
                self.creates += 1;
                return Err(TransportError::AfterSend {
                    reason: "created reply lost".into(),
                });
            }
            Ok(response)
        }
    }
    impl Connection for Peer {
        fn poll_frame(&mut self) -> Result<FramePoll, TransportError> {
            self.inner.poll_frame()
        }
        fn subscribed(&self) -> bool {
            self.inner.subscribed()
        }
        fn resubscribe(&mut self) {
            self.inner.resubscribe();
        }
    }
    #[test]
    fn lost_first_creation_keeps_the_shared_no_replay_guard(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let mut client = Client::new(
            Peer {
                inner: ContractDouble::new(),
                creates: 0,
            },
            Bounds::PRODUCTION,
        );
        client.offer_directory("/work/project-a")?;
        attach(&mut client, None)?;
        let agent = client.agents().first().ok_or("agent")?.id.clone();
        let model = client.models().first().ok_or("model")?.clone();
        client.select_agent(&agent)?;
        client.select_model(&model)?;
        assert!(client.create_draft_session().is_err());
        assert!(client.create_draft_session().is_err());
        assert_eq!(client.peer_mut().creates, 1);
        Ok(())
    }
}
