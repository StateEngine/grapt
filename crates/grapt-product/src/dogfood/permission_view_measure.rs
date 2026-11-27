use super::*;

#[test]
fn reader_limit_falls_back_without_retaining_another_generation(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = grapt_store::EventStore::open_in_memory()?;
    grapt_permission::PermissionHttpState::initialize(&mut store)?;
    let view = PermissionView::default();
    *view.published.lock().map_err(|_| "view lock poisoned")? =
        build(store.connection())?.map(Arc::new);
    let client = grapt_types::AuthenticatedClientId::new("client_view").ok_or("client")?;
    let owner = PermissionOwner {
        subject: "subject",
        client: &client,
    };
    view.readers.store(MAX_READERS, Ordering::Release);
    assert!(view.poll(&owner, None).is_none());
    assert_eq!(
        view.readers.load(Ordering::Acquire),
        MAX_READERS,
        "overload leaked a lease"
    );
    view.readers.store(0, Ordering::Release);
    let page = view
        .poll(&owner, None)
        .ok_or("available snapshot not used")?
        .map_err(|error| format!("{error:?}"))?;
    assert!(page.requests.is_empty());
    assert_eq!(
        view.readers.load(Ordering::Acquire),
        0,
        "completed poll retained a lease"
    );
    let absent: SessionId = "ses_absent".parse()?;
    assert!(matches!(
        view.poll(&owner, Some(&absent)),
        Some(Err(PollError::MissingSession(_)))
    ));
    assert_eq!(
        view.readers.load(Ordering::Acquire),
        0,
        "error retained a lease"
    );
    Ok(())
}

#[test]
#[ignore = "requires the privately retained Luna store; opened read-only"]
fn retained_luna_store_generation_allocation() -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::var_os("GRAPT_PERMISSION_VIEW_STORE")
        .ok_or("set GRAPT_PERMISSION_VIEW_STORE to the retained read-only evidence store")?;
    let connection = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let events: i64 = connection.query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(events, 432, "retained Luna evidence changed");
    let generation = build(&connection)?.ok_or("retained Luna snapshot unavailable")?;
    println!("permission view: retained Luna events={events}, Sessions={}, allocation_bytes={}, max_bytes={MAX_GENERATION_BYTES}",
        generation.sessions.len(), generation.allocation_bytes);
    assert!(generation.allocation_bytes <= MAX_GENERATION_BYTES);
    Ok(())
}
