use super::*;

#[test]
fn command_and_model_refresh_serialize_and_cancel_the_previous_reader() -> Result {
    let (mut client, _, _profile) = fixture(false)?;
    client.enable_commands();
    client.advance_commands();
    assert_eq!(
        client
            .connection
            .pending
            .as_ref()
            .map(WireRequest::operation),
        Some(Operation::CommandInspect)
    );
    client.start_catalog_refresh()?;
    assert!(client.connection.pending.is_none());
    assert_eq!(client.connection.cancellations, 1);
    client.advance_catalog();
    client.advance_commands();
    assert_eq!(
        client
            .connection
            .pending
            .as_ref()
            .map(WireRequest::operation),
        Some(Operation::AgentList)
    );
    client.connection.blocked = false;
    for _ in 0..8 {
        client.advance_catalog();
        client.advance_commands();
    }
    assert!(client.commands_status().is_none());
    assert!(client.connection.pending.is_none());
    Ok(())
}
