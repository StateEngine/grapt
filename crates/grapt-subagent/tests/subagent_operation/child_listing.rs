use super::*;

#[test]
fn child_listing_is_parent_scoped_paginated_and_keeps_terminal_children() -> TestResult {
    let mut svc = service_for(&["ses_parent", "ses_other"])?;
    for index in 0..66 {
        svc.open(
            &admission(
                &format!("sub_{index:03}"),
                &format!("ses_child_{index}"),
                "ses_parent",
                SubagentMode::Background,
            ),
            10,
        )?;
    }
    open_running(&mut svc, "sub_other", "ses_foreign", "ses_other")?;
    svc.start(&op("sub_000"), RuntimeGeneration::new(1), 11)?;
    svc.settle(
        &op("sub_000"),
        &SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted),
        12,
    )?;
    let first = SubagentService::list_children_on(svc.store(), &ses("ses_parent"), None)?;
    assert_eq!(first.children.len(), 64);
    assert_eq!(first.children[0].child, ses("ses_child_0"));
    assert_eq!(first.children[0].status, "cancelled");
    assert_eq!(first.children[1].status, "admitted");
    assert_eq!(first.next, Some(op("sub_063")));
    let second =
        SubagentService::list_children_on(svc.store(), &ses("ses_parent"), first.next.as_ref())?;
    assert_eq!(second.children.len(), 2);
    assert_eq!(second.children[0].operation, op("sub_064"));
    assert!(second.next.is_none());
    let other =
        SubagentService::list_children_on(svc.store(), &ses("ses_other"), first.next.as_ref())?;
    assert_eq!(other.children.len(), 1);
    assert_eq!(other.children[0].child, ses("ses_foreign"));
    assert_eq!(other.children[0].status, "running");
    Ok(())
}

#[test]
fn listing_does_not_create_schema_and_refuses_oversized_rows() -> TestResult {
    let store = EventStore::open_in_memory()?;
    assert!(
        SubagentService::list_children_on(&store, &ses("ses_parent"), None)?
            .children
            .is_empty()
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM sqlite_master WHERE name = 'subagent_operation'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 0);
    let mut svc = service_for(&["ses_parent"])?;
    let mut oversized = admission(
        "sub_large",
        "ses_child",
        "ses_parent",
        SubagentMode::Foreground,
    );
    oversized.description = "x".repeat(1_048_577);
    svc.open(&oversized, 10)?;
    assert!(matches!(
        SubagentService::list_children_on(svc.store(), &ses("ses_parent"), None),
        Err(SubagentError::Corruption { .. })
    ));
    Ok(())
}
