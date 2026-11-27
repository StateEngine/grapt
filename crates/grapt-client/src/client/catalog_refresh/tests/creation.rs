use super::*;

#[test]
fn catalog_responses_obey_the_common_client_bound_before_decoding() -> Result {
    let (mut client, session, _profile) = fixture(true)?;
    client.attach_launch_session(&session)?;
    let chosen = client.model.clone();
    client.bounds.max_response_bytes = 1;
    client.connection.blocked = false;
    for _ in 0..2 {
        client.advance_catalog();
    }
    assert_eq!(client.model, chosen);
    assert!(client
        .catalog_status()
        .is_some_and(|n| n.contains("1-byte bound")));
    Ok(())
}
