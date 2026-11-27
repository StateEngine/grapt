//! Session context is canonical; Git diagnosis cannot authorize or block it.
use super::*;

#[test]
fn status_waits_for_held_commands_without_replacing_the_catalog_reader() -> Result {
    let (mut client, session, _profile) = fixture(false)?;
    client.attach_launch_session(&session)?;
    client.connection.blocked = false;
    for _ in 0..6 {
        client.advance_catalog();
    }
    client.enable_commands();
    client.advance_commands();
    client.connection.blocked = true;
    client.refresh_status();
    let starts = client.connection.catalog_starts;
    for _ in 0..8 {
        client.advance_catalog();
        client.advance_commands();
    }
    assert_eq!(client.connection.catalog_starts, starts);
    assert_eq!(
        client
            .connection
            .pending
            .as_ref()
            .ok_or("held commands")?
            .operation(),
        Operation::CommandInspect
    );
    client.connection.blocked = false;
    assert!(client.advance_commands());
    assert!(!client.commands_pending());
    assert!(client.commands_status().is_none());
    client.advance_catalog();
    assert_eq!(
        client
            .connection
            .pending
            .as_ref()
            .ok_or("status metadata")?
            .operation(),
        Operation::ProjectCurrent
    );
    assert!(client.advance_catalog());
    assert!(client.connection.pending.is_none());
    assert!(client.repository_status().is_none());
    assert!(client.ready_to_chat());
    Ok(())
}

#[test]
fn saved_session_startup_defers_optional_git_until_status_and_failure_keeps_choices() -> Result {
    let (mut client, session, _profile) = fixture(false)?;
    client.attach_launch_session(&session)?;
    assert!(client.catalog_pending());
    assert!(!client.catalog_validated());
    assert!(!client.ready_to_chat());
    client.connection.blocked = false;
    for _ in 0..3 {
        client.advance_catalog();
        let request = client
            .connection
            .pending
            .as_ref()
            .ok_or("catalog request")?;
        assert!(request
            .query()
            .iter()
            .any(|(key, value)| key == "session" && value == session.spelled()));
        assert!(
            client.advance_catalog(),
            "a completed phase schedules the next one"
        );
    }
    assert!(!client.catalog_pending());
    assert!(client.catalog_validated());
    assert!(client.ready_to_chat());
    let model = client.model().cloned();
    for _ in 0..8 {
        client.pump()?;
    }
    assert!(client.connection.pending.is_none());
    assert!(client.ready_to_chat());
    assert_eq!(client.model(), model.as_ref());
    // The ordinary status flow opts in to metadata; idle startup never does.
    client.refresh_status();
    client.advance_catalog();
    let request = client
        .connection
        .pending
        .as_ref()
        .ok_or("metadata request")?;
    assert_eq!(request.operation(), Operation::ProjectCurrent);
    assert!(!request.query().iter().any(|(key, _)| key == "session"));
    // Hold the optional Git request across ordinary live/input pumps.
    client.connection.blocked = true;
    for _ in 0..8 {
        client.pump()?;
    }
    assert!(client.ready_to_chat());
    assert_eq!(client.connection.prompts, 0);
    client.connection.blocked = false;
    client.connection.fail = true;
    client.advance_catalog();
    assert!(client
        .repository_status()
        .is_some_and(|note| note.contains("metadata unavailable")));
    assert!(client.ready_to_chat());
    assert_eq!(client.model(), model.as_ref());
    Ok(())
}

#[test]
fn project_switch_cancels_optional_metadata_and_drops_session_coordinate() -> Result {
    let (mut client, session, _profile) = fixture(false)?;
    client.attach_launch_session(&session)?;
    client.connection.blocked = false;
    for _ in 0..6 {
        client.advance_catalog();
    }
    client.refresh_status();
    client.advance_catalog();
    assert_eq!(
        client
            .connection
            .pending
            .as_ref()
            .ok_or("metadata")?
            .operation(),
        Operation::ProjectCurrent
    );
    let other = client
        .projects()
        .iter()
        .find(|row| Some(*row) != client.project())
        .cloned()
        .ok_or("other project")?;
    client.select_project(&other)?;
    assert!(client.catalog_refresh.binding.is_none());
    client.advance_catalog();
    let request = client.connection.pending.as_ref().ok_or("new catalog")?;
    assert!(!request.query().iter().any(|(key, _)| key == "session"));
    assert!(client.repository_status().is_none());
    Ok(())
}
