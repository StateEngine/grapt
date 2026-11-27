//! Deferred reads through the public request and state consumers.
use crate::wire::{PermissionReplyChoice, SessionWireId};
use crate::{Bounds, Client, ContractDouble, PeriodicRead, PreparedRead, Transport, WireResponse};
type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
fn client() -> Result<Client<ContractDouble>> {
    let mut client = Client::new(ContractDouble::new(), Bounds::PRODUCTION);
    client.refresh_projects()?;
    let project = client.projects().first().ok_or("project")?.clone();
    client.select_project(&project)?;
    let agent = client.agents().first().ok_or("agent")?.id.clone();
    client.select_agent(&agent)?;
    let model = client.models().first().ok_or("model")?.clone();
    client.select_model(&model)?;
    client.create_session()?;
    Ok(client)
}
fn selected(client: &Client<ContractDouble>) -> Result<SessionWireId> {
    client.selected().cloned().ok_or_else(|| "session".into())
}
fn snapshot(
    client: &mut Client<ContractDouble>,
    read: PeriodicRead,
) -> Result<(PreparedRead, WireResponse)> {
    let prepared = client.prepare_read(read)?.ok_or("prepared read")?;
    let response = client.peer_mut().call(&prepared.request)?;
    Ok((prepared, response))
}

#[test]
fn review_periodic_session_forms_discard_after_selection_changes() -> Result {
    let mut client = client()?;
    let first = selected(&client)?;
    client
        .peer_mut()
        .raise_form(&first, "Old question")
        .ok_or("first form")?;
    let (prepared, response) = snapshot(&mut client, PeriodicRead::SessionForms)?;
    let second = client.create_session()?;
    client
        .peer_mut()
        .raise_form(&second, "Current question")
        .ok_or("second form")?;
    client.refresh_forms()?;
    let before = client.forms().to_vec();
    assert!(!client.accept_read(prepared, Ok(response))?);
    assert_eq!(client.forms(), before);
    assert_eq!(client.forms()[0].title, "Current question");
    Ok(())
}

#[test]
fn review_periodic_stale_forms_errors_are_discarded_before_status_validation() -> Result {
    let mut client = client()?;
    let prepared = client
        .prepare_read(PeriodicRead::SessionForms)?
        .ok_or("prepared")?;
    client.create_session()?;
    let result = client.accept_read(
        prepared,
        Ok(WireResponse {
            status: 500,
            body: vec![],
        }),
    );
    assert!(
        matches!(result, Ok(false)),
        "old-scope status must not reach the current Session: {result:?}"
    );
    Ok(())
}

#[test]
fn review_periodic_catalog_cannot_resurrect_an_acknowledged_closed_session() -> Result {
    let mut client = client()?;
    let session = selected(&client)?;
    let (prepared, response) = snapshot(&mut client, PeriodicRead::Sessions)?;
    client.delete_session(&session)?;
    assert!(client.selected().is_none());
    assert!(!client.sessions().iter().any(|row| row.id == session));
    let applied = client.accept_read(prepared, Ok(response))?;
    assert!(
        !client.sessions().iter().any(|row| row.id == session),
        "closed Session resurrected by old periodic answer; applied={applied}"
    );
    assert!(!applied);
    Ok(())
}

#[test]
fn review_periodic_permissions_cannot_reopen_an_acknowledged_ask() -> Result {
    let mut client = client()?;
    let session = selected(&client)?;
    client
        .peer_mut()
        .ask_permission(&session, "Run a local command")
        .ok_or("ask")?;
    client.refresh_permissions()?;
    let ask = client
        .permissions()
        .first()
        .ok_or("permission")?
        .request_id
        .clone();
    let (prepared, response) = snapshot(&mut client, PeriodicRead::Permissions)?;
    client.reply_permission(&session, &ask, PermissionReplyChoice::Once)?;
    assert!(client.permissions().is_empty());
    let applied = client.accept_read(prepared, Ok(response))?;
    assert!(
        client.permissions().is_empty(),
        "acknowledged ask reopened by old periodic answer; applied={applied}"
    );
    assert!(!applied);
    Ok(())
}

#[test]
fn review_periodic_forms_cannot_reopen_an_acknowledged_cancel() -> Result {
    let mut client = client()?;
    let session = selected(&client)?;
    let form = client
        .peer_mut()
        .raise_form(&session, "Question")
        .ok_or("form")?;
    client.refresh_forms()?;
    let (prepared, response) = snapshot(&mut client, PeriodicRead::SessionForms)?;
    client.cancel_form(&session, &form)?;
    assert!(client.forms().is_empty());
    let applied = client.accept_read(prepared, Ok(response))?;
    assert!(
        client.forms().is_empty(),
        "cancelled form reopened by old periodic answer; applied={applied}"
    );
    assert!(!applied);
    Ok(())
}

#[test]
fn review_periodic_session_forms_discard_after_the_session_closes() -> Result {
    let mut client = client()?;
    let session = selected(&client)?;
    client
        .peer_mut()
        .raise_form(&session, "Question")
        .ok_or("form")?;
    let (prepared, response) = snapshot(&mut client, PeriodicRead::SessionForms)?;
    client.delete_session(&session)?;
    assert!(!client.accept_read(prepared, Ok(response))?);
    assert!(client.forms().is_empty());
    assert!(client.prepare_read(PeriodicRead::SessionForms)?.is_none());
    assert!(client.prepare_read(PeriodicRead::LocationForms)?.is_none());
    Ok(())
}

#[test]
fn review_periodic_location_forms_discard_after_the_location_changes() -> Result {
    let mut client = client()?;
    let (prepared, response) = snapshot(&mut client, PeriodicRead::LocationForms)?;
    let other = client
        .projects()
        .iter()
        .find(|project| Some(*project) != client.project())
        .ok_or("second project")?
        .clone();
    client.select_project(&other)?;
    assert!(!client.accept_read(prepared, Ok(response))?);
    assert!(client.location_forms().is_empty());
    Ok(())
}

#[test]
fn review_periodic_malformed_forms_leave_the_current_rows_intact() -> Result {
    let mut client = client()?;
    let session = selected(&client)?;
    client
        .peer_mut()
        .raise_form(&session, "Question")
        .ok_or("form")?;
    client.refresh_forms()?;
    let before = client.forms().to_vec();
    let prepared = client
        .prepare_read(PeriodicRead::SessionForms)?
        .ok_or("prepared")?;
    assert!(client
        .accept_read(
            prepared,
            Ok(WireResponse {
                status: 200,
                body: b"{}".to_vec()
            })
        )
        .is_err());
    assert_eq!(client.forms(), before);
    Ok(())
}
