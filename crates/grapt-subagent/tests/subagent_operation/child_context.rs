#[test]
fn child_context_preserves_parent_mode_and_current_lifecycle() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    for (id, child, mode) in [
        ("sub_foreground", "ses_foreground", SubagentMode::Foreground),
        ("sub_background", "ses_background", SubagentMode::Background),
    ] {
        svc.open(&admission(id, child, "ses_parent", mode), 10)?;
        let context = SubagentService::child_context_on(svc.store(), &ses(child))?
            .ok_or("missing child context")?;
        assert_eq!(context.operation, op(id));
        assert_eq!(context.parent, ses("ses_parent"));
        assert_eq!(context.mode, mode);
        assert_eq!(context.status, OperationStatus::Admitted);

        svc.start(&op(id), RuntimeGeneration::new(7), 11)?;
        let running = SubagentService::child_context_on(svc.store(), &ses(child))?
            .ok_or("missing running context")?;
        assert_eq!(
            running.status,
            OperationStatus::Running(RuntimeGeneration::new(7))
        );
        assert_eq!(running.parent, context.parent);
        assert_eq!(running.mode, context.mode);
        assert_eq!(
            SubagentService::child_operation_on(svc.store(), &ses(child))?,
            Some((running.operation, running.status))
        );
        svc.settle(
            &op(id),
            &SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted),
            12,
        )?;
        assert!(matches!(
            SubagentService::child_context_on(svc.store(), &ses(child))?
                .ok_or("missing terminal context")?
                .status,
            OperationStatus::Terminal(SubagentTerminal::Cancelled(
                SubagentCancelReason::Interrupted
            ))
        ));
    }
    assert_eq!(
        SubagentService::child_context_on(svc.store(), &ses("ses_unknown"))?,
        None
    );
    Ok(())
}

#[test]
fn reading_child_context_does_not_install_an_absent_schema() -> TestResult {
    let store = EventStore::open_in_memory()?;
    assert_eq!(
        SubagentService::child_context_on(&store, &ses("ses_unknown"))?,
        None
    );
    let installed: bool = store.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name = 'subagent_operation')",
        [],
        |row| row.get(0),
    )?;
    assert!(!installed);
    Ok(())
}
