use super::*;

#[test]
fn refresh_preserves_a_choice_made_while_the_catalog_was_pending() -> Result {
    let (mut client, session, _profile) = fixture(true)?;
    client.attach_launch_session(&session)?;
    client.advance_catalog();
    let other = client
        .models()
        .iter()
        .find(|m| Some(*m) != client.model() && m.enabled)
        .cloned()
        .ok_or("second model")?;
    client.select_model(&other)?;
    client.connection.blocked = false;
    for _ in 0..6 {
        client.advance_catalog();
    }
    assert_eq!(
        client.model().map(|m| (&m.id, &m.provider_id, &m.variant)),
        Some((&other.id, &other.provider_id, &other.variant))
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn replacing_a_cache_symlink_does_not_change_its_target() -> Result {
    let (client, _, profile) = fixture(true)?;
    let project = client.project().ok_or("project")?;
    let snapshot =
        disk::load(&profile.0, project, client.bounds.max_response_bytes)?.ok_or("cache")?;
    let target = profile.0.join("retained.txt");
    std::fs::write(&target, b"retained content")?;
    let cache = profile.0.join("tui-catalog.json");
    std::fs::remove_file(&cache)?;
    std::os::unix::fs::symlink(&target, &cache)?;
    disk::save(
        &profile.0,
        project,
        &snapshot,
        client.bounds.max_response_bytes,
    )?;
    assert_eq!(std::fs::read(&target)?, b"retained content");
    assert!(!std::fs::symlink_metadata(cache)?.file_type().is_symlink());
    Ok(())
}
