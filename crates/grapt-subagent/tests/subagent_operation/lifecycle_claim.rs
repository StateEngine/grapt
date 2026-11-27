#[test]
fn the_lifecycle_is_admit_then_start_then_settle() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    svc.open(
        &admission(
            "sub_a",
            "ses_child1",
            "ses_parent",
            SubagentMode::Background,
        ),
        10,
    )?;
    assert_eq!(
        svc.get(&id)?.ok_or("op")?.0,
        OperationStatus::Admitted,
        "opens Admitted, not Running"
    );
    assert_eq!(
        svc.start(&id, RuntimeGeneration::new(7), 11)?,
        CasOutcome::Applied
    );
    assert_eq!(
        svc.get(&id)?.ok_or("op")?.0,
        OperationStatus::Running(RuntimeGeneration::new(7))
    );
    assert_eq!(svc.settle(&id, &completed("r0")?, 12)?, CasOutcome::Applied);
    let (status, delivery) = svc.get(&id)?.ok_or("op")?;
    assert_eq!(status, OperationStatus::Terminal(completed("r0")?));
    assert_eq!(
        delivery,
        DeliveryStatus::Unclaimed,
        "terminal does NOT imply delivered"
    );
    // start on a non-admitted op does not apply.
    assert_eq!(
        svc.start(&id, RuntimeGeneration::new(8), 13)?,
        CasOutcome::AlreadyTerminal
    );
    Ok(())
}

#[test]
fn an_ambiguous_operation_reconciles_and_never_auto_replays() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    let evidence = match AmbiguityEvidence::new("provider effect status unknown") {
        Some(v) => v,
        None => panic!("evidence"),
    };
    assert_eq!(svc.mark_ambiguous(&id, &evidence, 20)?, CasOutcome::Applied);
    assert_eq!(
        svc.get(&id)?.ok_or("op")?.0,
        OperationStatus::ExternallyAmbiguous(evidence.clone())
    );
    // No auto-replay: settle (running-only) does not apply to an ambiguous op.
    assert_eq!(
        svc.settle(&id, &completed("x")?, 21)?,
        CasOutcome::AlreadyTerminal
    );
    // Only an explicit reconcile moves it to a terminal.
    assert_eq!(
        svc.reconcile(
            &id,
            &SubagentTerminal::Cancelled(SubagentCancelReason::Interrupted),
            22
        )?,
        CasOutcome::Applied
    );
    assert_eq!(
        svc.get(&id)?.ok_or("op")?.0,
        OperationStatus::Terminal(SubagentTerminal::Cancelled(
            SubagentCancelReason::Interrupted
        ))
    );
    Ok(())
}

#[test]
fn a_dead_generation_running_operation_settles_process_loss_and_never_replays() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let live = op("sub_live");
    let dead = op("sub_dead");
    open_running(&mut svc, "sub_live", "ses_child1", "ses_parent")?;
    svc.settle(&live, &completed("r0")?, 12)?;
    open_running(&mut svc, "sub_dead", "ses_child2", "ses_parent")?;

    let settled = svc.settle_dead_generation(RuntimeGeneration::new(2), 30)?;
    assert_eq!(
        settled,
        vec![op("sub_dead")],
        "only the running dead-gen op"
    );
    assert_eq!(
        svc.get(&dead)?.ok_or("dead")?.0,
        OperationStatus::Terminal(SubagentTerminal::Cancelled(
            SubagentCancelReason::ProcessLoss
        ))
    );
    assert_eq!(
        svc.get(&live)?.ok_or("live")?.0,
        OperationStatus::Terminal(completed("r0")?)
    );
    assert!(
        svc.settle_dead_generation(RuntimeGeneration::new(3), 31)?
            .is_empty(),
        "no replay"
    );
    Ok(())
}

// ---- claim + delivery -------------------------------------------------------

#[test]
fn a_claim_route_must_match_the_operation_mode() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    // Background op cannot be claimed by the tool-result route.
    open_running(&mut svc, "sub_bg", "ses_child1", "ses_parent")?;
    svc.settle(&op("sub_bg"), &completed("r0")?, 20)?;
    let misrouted = DeliveryClaim {
        id: claim_id("dcl_1"),
        route: ClaimRoute::ToolResult,
        parent: ses("ses_parent"),
        input: msg("msg_x"),
        generation: RuntimeGeneration::new(1),
    };
    assert!(matches!(
        svc.claim(&op("sub_bg"), &misrouted, 21),
        Err(SubagentError::MisroutedClaim { .. })
    ));
    // Foreground op cannot be claimed by the operation-delivery route.
    svc.open(
        &admission(
            "sub_fg",
            "ses_child2",
            "ses_parent",
            SubagentMode::Foreground,
        ),
        10,
    )?;
    svc.start(&op("sub_fg"), RuntimeGeneration::new(1), 11)?;
    svc.settle(&op("sub_fg"), &completed("r0")?, 20)?;
    assert!(matches!(
        svc.claim(
            &op("sub_fg"),
            &synthetic_claim("dcl_2", "ses_parent", "msg_y"),
            21
        ),
        Err(SubagentError::MisroutedClaim { .. })
    ));
    Ok(())
}

#[test]
fn the_claim_is_an_exactly_once_arbiter() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    // Cannot claim a running (non-terminal) op.
    assert_eq!(
        svc.claim(&id, &synthetic_claim("dcl_1", "ses_parent", "msg_x"), 15)?,
        ClaimOutcome::Lost
    );
    svc.settle(&id, &completed("r0")?, 20)?;
    assert_eq!(
        svc.claim(&id, &synthetic_claim("dcl_1", "ses_parent", "msg_x"), 21)?,
        ClaimOutcome::Won
    );
    assert_eq!(
        svc.claim(&id, &synthetic_claim("dcl_2", "ses_parent", "msg_z"), 22)?,
        ClaimOutcome::Lost
    );
    match svc.get(&id)?.ok_or("op")?.1 {
        DeliveryStatus::Delivering(held, ClaimRoute::OperationDelivery) => {
            assert_eq!(held.as_str(), "dcl_1")
        }
        other => panic!("expected delivering under dcl_1, got {other:?}"),
    }
    Ok(())
}

#[test]
fn the_same_lifecycle_runs_against_a_borrowed_store() -> TestResult {
    // The `*_on` half of the service, which exists because a product that keeps
    // its one `EventStore` inside an `Inbox` can only ever LEND it out — it
    // cannot hand over ownership, and `EventStore` is not `Clone`. Without
    // these, the whole subagent domain is unreachable from the composition
    // root, which is exactly why it had no production caller.
    //
    // The assertion is that the borrowed path is the SAME lifecycle, not a
    // parallel one: admit, start, settle, with terminal still not implying
    // delivered. A second implementation that drifted from the owning form
    // would be a second state machine over one table.
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session("ses_parent".parse()?))?;
    SubagentService::ensure_schema_on(&store)?;

    let id = op("sub_borrowed");
    SubagentService::open_on(
        &store,
        &admission(
            "sub_borrowed",
            "ses_child1",
            "ses_parent",
            SubagentMode::Foreground,
        ),
        10,
    )?;
    assert_eq!(
        SubagentService::start_on(&mut store, &id, RuntimeGeneration::new(7), 11)?,
        CasOutcome::Applied,
        "an admitted operation starts"
    );
    assert_eq!(
        SubagentService::settle_on(&mut store, &id, &completed("r0")?, 12)?,
        CasOutcome::Applied,
        "and a running one settles"
    );

    // Read back through the OWNING form, over the same database. The two forms
    // must agree, or the factoring introduced a second truth.
    let limits = DeliveryLimits::new(64, 256 * 1024).ok_or("delivery limits")?;
    let svc = SubagentService::new(store, limits)?;
    let (status, delivery) = svc.get(&id)?.ok_or("op")?;
    assert_eq!(status, OperationStatus::Terminal(completed("r0")?));
    assert_eq!(
        delivery,
        DeliveryStatus::Unclaimed,
        "terminal does NOT imply delivered, on this path either"
    );

    // And the CAS still refuses a transition from a terminal state.
    let mut store = svc;
    assert_eq!(
        store.start(&id, RuntimeGeneration::new(8), 13)?,
        CasOutcome::AlreadyTerminal
    );
    Ok(())
}
