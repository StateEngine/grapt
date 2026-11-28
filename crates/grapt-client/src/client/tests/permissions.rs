//! A staged permission list never brings back an answered ask, and never
//! evicts a newer ask for its own older rows.
use super::*;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn listed(client: &mut Client<ContractDouble>) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
    let request = WireRequest::new(Operation::PermissionRequestList, None, None)?;
    Ok(client.connection.call(&request)?.body)
}

#[test]
fn an_answered_send_only_ask_leaves_the_cache_and_a_staged_list() -> TestResult {
    let mut client = client();
    let session = client.create_session()?;
    let ask = seed_ask(&mut client.connection, &session, "write a file");
    client.refresh_permissions()?;
    let staged = client.permission_refresh.revision();
    let list = listed(&mut client)?;
    client.send_permission_reply(&session, &ask, crate::wire::PermissionReplyChoice::Once)?;
    assert!(
        client
            .permissions()
            .iter()
            .all(|open| open.request_id != ask),
        "the answered ask stayed open"
    );
    client.accept_staged_permissions(&list, staged)?;
    assert!(
        client
            .permissions()
            .iter()
            .all(|open| open.request_id != ask),
        "the staged list brought the answered ask back"
    );
    Ok(())
}

#[test]
fn a_newer_ask_outranks_older_staged_rows_at_the_retention_bound() -> TestResult {
    let bounds = Bounds {
        max_retained_permissions: 2,
        ..Bounds::PRODUCTION
    }
    .validate()?;
    let mut client = Client::new(ContractDouble::new(), bounds);
    client.select_first_offer()?;
    let first = client.create_session()?;
    let second = client.create_session()?;
    seed_ask(&mut client.connection, &first, "write a file");
    seed_ask(&mut client.connection, &first, "run a command");
    let staged = client.permission_refresh.revision();
    let list = listed(&mut client)?;
    // A newer read of the second session alone holds its ask.
    let newer = seed_ask(&mut client.connection, &second, "edit a file");
    client.read_snapshot(&second)?;
    client.accept_staged_permissions(&list, staged)?;
    assert!(
        client
            .permissions()
            .iter()
            .any(|open| open.request_id == newer),
        "older staged rows evicted the newer ask"
    );
    assert_eq!(client.permissions().len(), 2);
    Ok(())
}
