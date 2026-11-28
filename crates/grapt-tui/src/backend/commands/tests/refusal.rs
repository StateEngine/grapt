//! A command catalog loaded before the service is refused keeps no rows.
use super::*;

#[test]
fn a_loaded_command_catalog_gives_way_to_the_refusal() -> TestResult {
    let mut client = fresh_draft()?;
    let mut completion = crate::slash::Completion::default();
    completion.catalog(crate::slash::Catalog::from_client(&client));
    assert!(completion.commands().any(|(name, _)| name == "focus"));

    client.peer_mut().refuse_subscribe = Some("stream positions");
    assert!(
        client.pump().is_err(),
        "the refused subscribe was not reported"
    );
    client.open_commands();
    client.refresh_commands();
    let Some(View::Text(lines)) = run(&mut client, "/commands")?.view else {
        return Err("list absent".into());
    };
    assert!(
        !lines.iter().any(|line| line.contains("/focus")),
        "{lines:?}"
    );
    assert!(lines
        .iter()
        .any(|line| line.starts_with("Commands unavailable")));
    assert!(!lines.iter().any(|line| line.contains("Reloading")));
    completion.catalog(crate::slash::Catalog::from_client(&client));
    assert_eq!(completion.commands().count(), 0);
    Ok(())
}
