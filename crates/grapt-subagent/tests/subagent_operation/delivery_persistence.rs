#[test]
fn delivery_requires_the_exact_claim_id_and_cotransacts() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    svc.settle(&id, &completed("r0")?, 20)?;
    svc.claim(
        &id,
        &synthetic_claim("dcl_1", "ses_parent", "msg_deliver"),
        21,
    )?;
    // A foreign claim id cannot deliver.
    assert_eq!(
        svc.deliver(
            &id,
            &claim_id("dcl_wrong"),
            RuntimeGeneration::new(1),
            &ev("evt_a"),
            22,
        )?,
        CasOutcome::NotLive
    );
    assert_eq!(
        parent_admissions(&svc, "ses_parent")?,
        0,
        "no admission under a wrong claim"
    );
    // The exact claim delivers, admitting exactly one operation-delivery fact.
    assert_eq!(
        svc.deliver(
            &id,
            &claim_id("dcl_1"),
            RuntimeGeneration::new(1),
            &ev("evt_b"),
            23,
        )?,
        CasOutcome::Applied
    );
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 1);
    match svc.get(&id)?.ok_or("op")?.1 {
        DeliveryStatus::Delivered(held, ClaimRoute::OperationDelivery) => {
            assert_eq!(held.as_str(), "dcl_1")
        }
        other => panic!("expected delivered under dcl_1, got {other:?}"),
    }
    // Duplicate delivery is refused and admits nothing more.
    assert_eq!(
        svc.deliver(
            &id,
            &claim_id("dcl_1"),
            RuntimeGeneration::new(1),
            &ev("evt_c"),
            24,
        )?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 1);
    Ok(())
}

#[test]
fn the_delivery_payload_is_derived_from_stored_facts() -> TestResult {
    // The operation-delivery fact is derived FROM the stored terminal, so
    // it cannot contradict them — the caller supplies no text/metadata.
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
    svc.start(&id, RuntimeGeneration::new(1), 11)?;
    svc.settle(&id, &completed("bounded result")?, 20)?;
    svc.claim(
        &id,
        &synthetic_claim("dcl_1", "ses_parent", "msg_child1"),
        21,
    )?;
    svc.deliver(
        &id,
        &claim_id("dcl_1"),
        RuntimeGeneration::new(1),
        &ev("evt_a"),
        22,
    )?;
    let data: String = svc.store().connection().query_row(
        "SELECT data FROM event WHERE aggregate_id = 'ses_parent' AND type = 'session.input.admitted.1'",
        [],
        |r| r.get(0),
    )?;
    let v: serde_json::Value = serde_json::from_str(&data)?;
    assert_eq!(v["input"]["type"], "operation-delivery");
    assert_eq!(v["input"]["subagentOperationID"], "sub_a");
    assert_eq!(v["input"]["operationID"], "op_sub_a");
    assert_eq!(v["input"]["generation"], 1);
    assert_eq!(v["input"]["projection"], "bounded result");
    assert!(v["input"].get("delivery").is_none());
    Ok(())
}

#[test]
fn a_tool_result_claim_cannot_be_delivered_as_an_operation_fact() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_fg");
    svc.open(
        &admission(
            "sub_fg",
            "ses_child1",
            "ses_parent",
            SubagentMode::Foreground,
        ),
        10,
    )?;
    svc.start(&id, RuntimeGeneration::new(1), 11)?;
    svc.settle(&id, &completed("r0")?, 20)?;
    let tool_claim = DeliveryClaim {
        id: claim_id("dcl_1"),
        route: ClaimRoute::ToolResult,
        parent: ses("ses_parent"),
        input: msg("msg_x"),
        generation: RuntimeGeneration::new(1),
    };
    assert_eq!(svc.claim(&id, &tool_claim, 21)?, ClaimOutcome::Won);
    assert!(matches!(
        svc.deliver(
            &id,
            &claim_id("dcl_1"),
            RuntimeGeneration::new(1),
            &ev("evt_a"),
            22,
        ),
        Err(SubagentError::WrongRouteForDelivery { .. })
    ));
    Ok(())
}

#[test]
fn reopen_binds_the_stored_claim_parent_and_input() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    svc.settle(&id, &completed("r0")?, 20)?;
    svc.claim(&id, &synthetic_claim("dcl_1", "ses_parent", "msg_real"), 21)?;
    // A wrong claim id cannot reopen.
    assert!(!svc.reopen_if_undelivered(&id, &claim_id("dcl_wrong"), 22)?);
    // The bound claim reopens because the parent recorded no delivery for its
    // stored input.
    assert!(svc.reopen_if_undelivered(&id, &claim_id("dcl_1"), 23)?);
    assert_eq!(svc.get(&id)?.ok_or("op")?.1, DeliveryStatus::Unclaimed);
    // Re-claim, deliver, then reopen must NOT reopen (parent recorded it).
    svc.claim(
        &id,
        &synthetic_claim("dcl_2", "ses_parent", "msg_real2"),
        24,
    )?;
    svc.deliver(
        &id,
        &claim_id("dcl_2"),
        RuntimeGeneration::new(1),
        &ev("evt_a"),
        25,
    )?;
    assert!(
        !svc.reopen_if_undelivered(&id, &claim_id("dcl_2"), 26)?,
        "parent recorded delivery"
    );
    Ok(())
}

// ---- integrity --------------------------------------------------------------

#[test]
fn recovery_decodes_all_ids_before_mutating() -> TestResult {
    // A corrupt (empty) operation id among the dead-generation rows must fail
    // closed BEFORE any UPDATE — the good running row stays running.
    let mut svc = service_for(&["ses_parent"])?;
    open_running(&mut svc, "sub_good", "ses_child1", "ses_parent")?;
    // Inject a running row with an empty operation id (bypassing `open`).
    svc.store().connection().execute(
        "INSERT INTO subagent_operation
         (operation_id, family_operation_id, child_session_id, parent_session_id, agent, description, mode,
          effective_graph_id, status, generation, delivery, time_created, time_updated)
         VALUES ('', 'op_bad', 'ses_c', 'ses_parent', 'test', 'd', 'background', 'efg_x',
                 'running', 1, 'unclaimed', 1, 1)",
        [],
    )?;
    let err = svc.settle_dead_generation(RuntimeGeneration::new(2), 30);
    assert!(matches!(err, Err(SubagentError::CorruptOperationId { .. })));
    // The good row was NOT mutated (still running, not cancelled).
    assert_eq!(
        svc.get(&op("sub_good"))?.ok_or("good")?.0,
        OperationStatus::Running(RuntimeGeneration::new(1))
    );
    Ok(())
}

#[test]
fn check_constraints_reject_impossible_rows() -> TestResult {
    let svc = service_for(&["ses_p"])?;
    // running without a generation.
    let bad = svc.store().connection().execute(
        "INSERT INTO subagent_operation
         (operation_id, family_operation_id, child_session_id, parent_session_id, agent, description, mode,
          effective_graph_id, status, delivery, time_created, time_updated)
         VALUES ('sub_x', 'op_x', 'ses_c', 'ses_p', 'test', 'd', 'background', 'efg_x',
                 'running', 'unclaimed', 1, 1)",
        [],
    );
    assert!(bad.is_err(), "running must carry a generation");
    // delivered without time_delivered / claim.
    let bad2 = svc.store().connection().execute(
        "INSERT INTO subagent_operation
         (operation_id, family_operation_id, child_session_id, parent_session_id, agent, description, mode,
          effective_graph_id, status, output, delivery, time_created, time_updated)
         VALUES ('sub_y', 'op_y', 'ses_c', 'ses_p', 'test', 'd', 'background', 'efg_x',
                 'completed', 'r0', 'delivered', 1, 1)",
        [],
    );
    assert!(
        bad2.is_err(),
        "delivered must carry a claim and a delivery time"
    );
    Ok(())
}

#[test]
fn a_failed_admission_rolls_back_the_whole_delivery_atomically() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    for (id, child) in [("sub_a", "ses_child1"), ("sub_b", "ses_child2")] {
        open_running(&mut svc, id, child, "ses_parent")?;
        svc.settle(&op(id), &completed("r0")?, 20)?;
    }
    svc.claim(
        &op("sub_a"),
        &synthetic_claim("dcl_a", "ses_parent", "msg_a"),
        21,
    )?;
    svc.claim(
        &op("sub_b"),
        &synthetic_claim("dcl_b", "ses_parent", "msg_b"),
        21,
    )?;
    // A delivers with evt_shared → commits.
    svc.deliver(
        &op("sub_a"),
        &claim_id("dcl_a"),
        RuntimeGeneration::new(1),
        &ev("evt_shared"),
        22,
    )?;
    // B reuses evt_shared → append fails, the whole delivery rolls back.
    assert!(svc
        .deliver(
            &op("sub_b"),
            &claim_id("dcl_b"),
            RuntimeGeneration::new(1),
            &ev("evt_shared"),
            23,
        )
        .is_err());
    match svc.get(&op("sub_b"))?.ok_or("b")?.1 {
        DeliveryStatus::Delivering(held, _) => assert_eq!(held.as_str(), "dcl_b"),
        other => panic!("b must still be delivering, got {other:?}"),
    }
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 1, "only A survived");
    Ok(())
}
