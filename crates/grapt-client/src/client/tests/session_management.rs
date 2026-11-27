use super::*;
use crate::transport::{FramePoll, Transport, WireResponse};

#[derive(Debug, Default)]
enum Fault {
    #[default]
    None,
    Refuse,
    MalformedAck,
    LookupUnavailable,
    LostReply,
}
#[derive(Debug)]
struct Peer {
    inner: ContractDouble,
    fault: Fault,
    search: Option<Vec<(String, String)>>,
}
impl Transport for Peer {
    fn call(&mut self, request: &WireRequest) -> Result<WireResponse, TransportError> {
        if request.operation() == Operation::SessionGet
            && matches!(self.fault, Fault::LookupUnavailable)
        {
            self.fault = Fault::None;
            return Ok(WireResponse {
                status: 503,
                body: Vec::new(),
            });
        }
        if request.operation() == Operation::SessionDelete {
            if matches!(self.fault, Fault::Refuse) {
                self.fault = Fault::None;
                return Ok(WireResponse {
                    status: 403,
                    body: Vec::new(),
                });
            }
            if matches!(self.fault, Fault::MalformedAck) {
                self.fault = Fault::None;
                return Ok(WireResponse {
                    status: 200,
                    body: b"not an acknowledgement".to_vec(),
                });
            }
            let response = self.inner.call(request)?;
            if matches!(self.fault, Fault::LostReply) {
                self.fault = Fault::None;
                return Err(TransportError::AfterSend {
                    reason: "Deletion committed; reply lost".into(),
                });
            }
            return Ok(response);
        }
        let mut response = self.inner.call(request)?;
        if request.operation() == Operation::SessionList {
            if let Some((_, search)) = request.query().iter().find(|(key, _)| key == "search") {
                self.search = Some(request.query().to_vec());
                let mut value: serde_json::Value =
                    serde_json::from_slice(&response.body).map_err(|e| {
                        TransportError::Unavailable {
                            reason: e.to_string(),
                        }
                    })?;
                if let Some(rows) = value
                    .get_mut("data")
                    .and_then(serde_json::Value::as_array_mut)
                {
                    rows.retain(|row| {
                        row["title"].as_str().is_some_and(|title| {
                            title.to_lowercase().contains(&search.to_lowercase())
                        })
                    });
                }
                response.body =
                    serde_json::to_vec(&value).map_err(|e| TransportError::Unavailable {
                        reason: e.to_string(),
                    })?;
            }
        }
        Ok(response)
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
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn kss_session_management_targets_rows_and_preserves_state_until_deletion_is_acknowledged(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut client = Client::new(
        Peer {
            inner: ContractDouble::new(),
            fault: Fault::None,
            search: None,
        },
        Bounds::PRODUCTION,
    );
    client.select_first_offer()?;
    let first = client.create_session()?;
    let second = client.create_session()?;
    client.submit_prompt("already queued before deletion")?;
    let third = client.create_session()?;
    client.select(&first)?;
    client.rename_session(&second, " Find me ")?;
    assert_eq!(client.selected(), Some(&first));
    let catalog = client.sessions().to_vec();
    let found = client.search_sessions("find")?;
    assert_eq!(found.len(), 1);
    assert_eq!(found[0].id, second);
    assert_eq!(found[0].detail.directory.as_deref(), Some("/work"));
    assert!(found[0].detail.updated_ms.is_some());
    assert_eq!(client.sessions(), catalog);
    let query = client
        .peer_mut()
        .search
        .as_ref()
        .ok_or("search was not sent")?;
    assert!(query
        .iter()
        .any(|row| row == &("search".into(), "find".into())));
    assert!(query
        .iter()
        .any(|row| row == &("limit".into(), "128".into())));
    client.peer_mut().fault = Fault::Refuse;
    assert!(client.delete_session(&second).is_err());
    assert!(client.sessions().iter().any(|row| row.id == second));
    assert!(client.view_of(&second).is_some());
    assert_eq!(client.selected(), Some(&first));
    client.peer_mut().fault = Fault::MalformedAck;
    assert!(
        client.delete_session(&second).is_err(),
        "malformed success cannot confirm deletion"
    );
    assert!(client.sessions().iter().any(|row| row.id == second));
    client.delete_session(&second)?;
    client.pump()?;
    assert!(client.sessions().iter().all(|row| row.id != second));
    assert!(
        client.view_of(&second).is_none(),
        "confirmed deletion must evict cached history"
    );
    assert_eq!(
        client.selected(),
        Some(&first),
        "deleting another row must retain attachment"
    );
    client.peer_mut().inner.restart();
    client.refresh_sessions()?;
    assert!(client.sessions().iter().all(|row| row.id != second));
    client.peer_mut().fault = Fault::LostReply;
    assert!(client.delete_session(&first).is_err());
    assert_eq!(
        client.selected(),
        Some(&first),
        "an unconfirmed result cannot detach the view"
    );
    assert!(client.view_of(&first).is_some());
    client.peer_mut().fault = Fault::LookupUnavailable;
    assert!(client.resume_session(&first).is_err());
    assert_eq!(
        client.selected(),
        Some(&first),
        "an unreadable lookup cannot retire a session"
    );
    assert!(client.view_of(&first).is_some());
    assert!(client.resume_session(&first).is_err());
    assert!(
        client.selected().is_none() && client.view_of(&first).is_none(),
        "explicit not-found retires the stale view"
    );
    client.peer_mut().inner.restart();
    let mut reopened = Client::new(client.connection, Bounds::PRODUCTION);
    reopened.select_first_offer()?;
    reopened.refresh_sessions()?;
    assert!(reopened
        .sessions()
        .iter()
        .all(|row| row.id != first && row.id != second));
    assert!(
        reopened.resume_session(&first).is_err(),
        "durable deletion must survive reconnect"
    );
    assert!(reopened.selected().is_none() && reopened.view_of(&first).is_none());
    reopened.select(&third)?;
    reopened.delete_session(&third)?;
    assert!(reopened.selected().is_none() && reopened.view_of(&third).is_none());
    let mut bounded = Client::new(
        ContractDouble::new(),
        Bounds {
            max_retained_sessions: 1,
            ..Bounds::PRODUCTION
        },
    );
    bounded.select_first_offer()?;
    bounded.create_session()?;
    let outside_page = bounded.create_session()?;
    bounded.resume_session(&outside_page)?;
    bounded.refresh_sessions()?;
    assert_eq!(bounded.sessions().len(), 1);
    assert_eq!(
        bounded.sessions()[0].id,
        outside_page,
        "a selected search result must survive a bounded catalog refresh"
    );

    Ok(())
}
