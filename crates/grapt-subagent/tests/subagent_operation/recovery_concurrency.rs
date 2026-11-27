#[test]
fn contending_claims_settle_exactly_once_across_threads() -> TestResult {
    use std::sync::{Arc, Barrier};
    let dir = std::env::temp_dir().join(format!("grapt-subagent-race-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("sub.db");
    {
        let limits = DeliveryLimits::new(64, 256 * 1024).ok_or("limits")?;
        let mut svc = SubagentService::new(EventStore::open(&db, "0.0.1")?, limits)?;
        open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
        svc.settle(&op("sub_a"), &completed("r0")?, 20)?;
    }
    let barrier = Arc::new(Barrier::new(2));
    let mut handles = Vec::new();
    for worker in 0..2 {
        let path = db.clone();
        let gate = Arc::clone(&barrier);
        handles.push(std::thread::spawn(
            move || -> Result<ClaimOutcome, String> {
                let limits =
                    DeliveryLimits::new(64, 256 * 1024).ok_or_else(|| "limits".to_owned())?;
                let mut svc = SubagentService::new(
                    EventStore::open(&path, "0.0.1").map_err(|e| e.to_string())?,
                    limits,
                )
                .map_err(|e| e.to_string())?;
                let claim = DeliveryClaim {
                    id: DeliveryClaimId::new(format!("dcl_{worker}")).ok_or("claim id")?,
                    route: ClaimRoute::OperationDelivery,
                    parent: "ses_parent"
                        .parse()
                        .map_err(|e: grapt_types::IdError| e.to_string())?,
                    input: format!("msg_{worker}")
                        .parse()
                        .map_err(|e: grapt_types::IdError| e.to_string())?,
                    generation: RuntimeGeneration::new(1),
                };
                let id = SubagentOperationId::new("sub_a").ok_or("op")?;
                gate.wait();
                svc.claim(&id, &claim, 21).map_err(|e| e.to_string())
            },
        ));
    }
    let (mut won, mut lost) = (0, 0);
    for h in handles {
        match h.join() {
            Ok(Ok(ClaimOutcome::Won)) => won += 1,
            Ok(Ok(ClaimOutcome::Lost)) => lost += 1,
            Ok(Err(e)) => panic!("claim failed: {e}"),
            Err(_) => panic!("thread panicked"),
        }
    }
    assert_eq!((won, lost), (1, 1), "exactly one claim wins");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_delivered_operation_survives_service_replacement() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-subagent-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("sub.db");
    let id = op("sub_a");
    {
        let mut store = EventStore::open(&db, "0.0.1")?;
        // Election is explicit even here: the delivery append never elects on
        // this service's behalf.
        store.elect_writer(&AggregateId::Session(ses("ses_parent")))?;
        let limits = DeliveryLimits::new(64, 256 * 1024).ok_or("limits")?;
        let mut svc = SubagentService::new(store, limits)?;
        open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
        svc.settle(&id, &completed("r0")?, 20)?;
        svc.claim(&id, &synthetic_claim("dcl_1", "ses_parent", "msg_x"), 21)?;
        svc.deliver(
            &id,
            &claim_id("dcl_1"),
            RuntimeGeneration::new(1),
            &ev("evt_x"),
            22,
        )?;
    }
    // The replacement is a NEW owner, so it would have to elect before it could
    // append — a legitimate ownership transfer, not a workaround. It elects
    // nothing because it only reads the delivered state back.
    let limits = DeliveryLimits::new(64, 256 * 1024).ok_or("limits")?;
    let reopened = SubagentService::new(EventStore::open(&db, "0.0.1")?, limits)?;
    match reopened.get(&id)?.ok_or("op")?.1 {
        DeliveryStatus::Delivered(held, ClaimRoute::OperationDelivery) => {
            assert_eq!(held.as_str(), "dcl_1")
        }
        other => panic!("expected delivered, got {other:?}"),
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_claim_cannot_target_a_foreign_parent() -> TestResult {
    // The claim's parent must be the operation's own immutable parent route: an
    // operation admitted under parent A cannot claim (or deliver) into parent B.
    // Only parent A is elected: B is never a legitimate target, so nothing here
    // may write it.
    let mut svc = service_for(&["ses_parentA"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parentA")?;
    svc.settle(&id, &completed("r0")?, 20)?;
    let foreign = DeliveryClaim {
        id: claim_id("dcl_1"),
        route: ClaimRoute::OperationDelivery,
        parent: ses("ses_parentB"),
        input: msg("msg_x"),
        generation: RuntimeGeneration::new(1),
    };
    assert!(matches!(
        svc.claim(&id, &foreign, 21),
        Err(SubagentError::ClaimParentMismatch { .. })
    ));
    // Zero writes: still unclaimed, and no delivery can proceed.
    assert_eq!(svc.get(&id)?.ok_or("op")?.1, DeliveryStatus::Unclaimed);
    assert_eq!(
        svc.deliver(
            &id,
            &claim_id("dcl_1"),
            RuntimeGeneration::new(1),
            &ev("evt_x"),
            22,
        )?,
        CasOutcome::NotLive
    );
    assert_eq!(parent_admissions(&svc, "ses_parentB")?, 0);
    Ok(())
}

#[test]
fn a_malformed_persisted_claim_target_fails_closed() -> TestResult {
    // A schema-bypassing corruption of a claimed row's target parent (a
    // non-null but unparseable id) must make get/reopen/deliver return typed
    // Corruption with zero mutation — the decoder never acts on a raw string.
    // The schema CHECK would reject this write, so it is injected past the
    // constraints to prove the DECODER'S independent fail-closed behavior.
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    svc.settle(&id, &completed("r0")?, 20)?;
    svc.claim(&id, &synthetic_claim("dcl_1", "ses_parent", "msg_real"), 21)?;
    let conn = svc.store().connection();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    conn.execute(
        "UPDATE subagent_operation SET target_parent = 'bogus' WHERE operation_id = ?1",
        [id.as_str()],
    )?;
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    assert!(
        matches!(svc.get(&id), Err(SubagentError::Corruption { .. })),
        "get fails closed"
    );
    assert!(
        matches!(
            svc.reopen_if_undelivered(&id, &claim_id("dcl_1"), 22),
            Err(SubagentError::Corruption { .. })
        ),
        "reopen fails closed"
    );
    assert!(
        matches!(
            svc.deliver(
                &id,
                &claim_id("dcl_1"),
                RuntimeGeneration::new(1),
                &ev("evt_x"),
                23,
            ),
            Err(SubagentError::Corruption { .. })
        ),
        "deliver fails closed"
    );
    // Zero mutation: the corrupt target is unchanged and nothing was admitted.
    let target: String = svc.store().connection().query_row(
        "SELECT target_parent FROM subagent_operation WHERE operation_id = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(target, "bogus");
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 0);
    Ok(())
}

#[test]
fn state_and_delivery_unions_are_bound_together() -> TestResult {
    // A running operation can never be delivered. The schema CHECK rejects the
    // direct write; and, injected past the constraints, the decoder still fails
    // closed with zero mutation.
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parent")?;
    // The schema forbids running + delivered directly.
    let direct = svc.store().connection().execute(
        "UPDATE subagent_operation
         SET delivery = 'delivered', claim_id = 'dcl_x', claim_route = 'synthetic',
             target_parent = 'ses_parent', target_input = 'msg_x', time_delivered = 1
         WHERE operation_id = ?1",
        [id.as_str()],
    );
    assert!(
        direct.is_err(),
        "running + delivered is rejected by the schema"
    );
    // Bypass the constraints to prove the decoder's own invariant.
    let conn = svc.store().connection();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    conn.execute(
        "UPDATE subagent_operation
         SET delivery = 'delivered', claim_id = 'dcl_x', claim_route = 'synthetic',
             target_parent = 'ses_parent', target_input = 'msg_x', time_delivered = 1
         WHERE operation_id = ?1",
        [id.as_str()],
    )?;
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    assert!(
        matches!(svc.get(&id), Err(SubagentError::Corruption { .. })),
        "decoder rejects running+delivered"
    );
    // Zero mutation: still running, no admission.
    let status: String = svc.store().connection().query_row(
        "SELECT status FROM subagent_operation WHERE operation_id = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(status, "running");
    Ok(())
}

#[test]
fn a_transition_full_decodes_the_row_before_mutating() -> TestResult {
    // cas_status must strictly decode the WHOLE row (not just the status
    // string) before a write: a constraint-compatible corrupt admission
    // identity fails closed with zero writes.
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
    // model_provider empty + model_id present: satisfies all-or-none but the
    // provider newtype is invalid.
    let conn = svc.store().connection();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    conn.execute(
        "UPDATE subagent_operation SET model_provider = '', model_id = 'gpt' WHERE operation_id = ?1",
        [id.as_str()],
    )?;
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    // The child listing must refuse the same corrupt admission as transitions,
    // using this existing corruption fixture without a new write escape.
    assert!(matches!(
        SubagentService::list_children_on(svc.store(), &ses("ses_parent"), None),
        Err(SubagentError::Corruption { .. })
    ));
    // start() is a cas_status transition; it must fail closed, not blindly CAS.
    assert!(matches!(
        SubagentService::child_context_on(svc.store(), &ses("ses_child1")),
        Err(SubagentError::Corruption { .. })
    ));
    assert!(matches!(
        svc.start(&id, RuntimeGeneration::new(1), 11),
        Err(SubagentError::Corruption { .. })
    ));
    // Zero writes: still admitted.
    let status: String = svc.store().connection().query_row(
        "SELECT status FROM subagent_operation WHERE operation_id = ?1",
        [id.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(status, "admitted");
    Ok(())
}

#[test]
fn recovery_full_decodes_every_row_before_the_bulk_update() -> TestResult {
    // settle_dead_generation must full-decode each selected row: a corrupt
    // admission identity on a dead-generation row fails closed, leaving the good
    // row running.
    let mut svc = service_for(&["ses_parent"])?;
    open_running(&mut svc, "sub_good", "ses_child1", "ses_parent")?;
    open_running(&mut svc, "sub_bad", "ses_child2", "ses_parent")?;
    let conn = svc.store().connection();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    conn.execute(
        "UPDATE subagent_operation SET model_provider = '', model_id = 'gpt' WHERE operation_id = ?1",
        [op("sub_bad").as_str()],
    )?;
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    assert!(matches!(
        svc.settle_dead_generation(RuntimeGeneration::new(2), 30),
        Err(SubagentError::Corruption { .. })
    ));
    // The good row was NOT mutated.
    assert_eq!(
        svc.get(&op("sub_good"))?.ok_or("good")?.0,
        OperationStatus::Running(RuntimeGeneration::new(1))
    );
    Ok(())
}

#[test]
fn a_foreign_but_valid_persisted_target_parent_fails_closed() -> TestResult {
    // A target_parent corrupted to ANOTHER valid SessionId (not the operation's
    // parent) must fail get/reopen/deliver — the decoder binds the target to the
    // immutable parent route.
    let mut svc = service_for(&["ses_parentA"])?;
    let id = op("sub_a");
    open_running(&mut svc, "sub_a", "ses_child1", "ses_parentA")?;
    svc.settle(&id, &completed("r0")?, 20)?;
    svc.claim(
        &id,
        &synthetic_claim("dcl_1", "ses_parentA", "msg_real"),
        21,
    )?;
    let conn = svc.store().connection();
    conn.execute_batch("PRAGMA ignore_check_constraints=ON")?;
    conn.execute(
        "UPDATE subagent_operation SET target_parent = 'ses_parentB' WHERE operation_id = ?1",
        [id.as_str()],
    )?;
    conn.execute_batch("PRAGMA ignore_check_constraints=OFF")?;
    assert!(
        matches!(svc.get(&id), Err(SubagentError::Corruption { .. })),
        "get"
    );
    assert!(
        matches!(
            svc.reopen_if_undelivered(&id, &claim_id("dcl_1"), 22),
            Err(SubagentError::Corruption { .. })
        ),
        "reopen"
    );
    assert!(
        matches!(
            svc.deliver(
                &id,
                &claim_id("dcl_1"),
                RuntimeGeneration::new(1),
                &ev("evt_x"),
                23,
            ),
            Err(SubagentError::Corruption { .. })
        ),
        "deliver"
    );
    assert_eq!(parent_admissions(&svc, "ses_parentB")?, 0);
    Ok(())
}

#[test]
fn an_out_of_range_generation_is_a_typed_refusal() -> TestResult {
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
    assert!(matches!(
        svc.start(&id, RuntimeGeneration::new(u64::MAX), 11),
        Err(SubagentError::GenerationOutOfRange { .. })
    ));
    // Zero writes: still admitted.
    assert_eq!(svc.get(&id)?.ok_or("op")?.0, OperationStatus::Admitted);
    Ok(())
}

#[test]
fn frozen_synthetic_delivery_is_inert_audit_input() -> TestResult {
    // The frozen row remains decodable for migration/audit, but it cannot enter
    // either active pending class and therefore cannot steer a parent Session.
    let path = fixtures::fixture_path(std::path::Path::new(
        "subagent-terminal-claim-delivery-processloss/expected/durable.jsonl",
    ));
    let expected_line = std::fs::read_to_string(&path)?
        .lines()
        .find(|l| l.contains("session.input.admitted"))
        .ok_or("frozen synthetic admission row")?
        .to_owned();
    let expected_data = serde_json::from_str::<serde_json::Value>(&expected_line)?["data"].clone();

    let decoded = InboxEvent::decode("session.input.admitted", 1, &expected_data)?
        .ok_or("legacy synthetic row")?;
    assert!(matches!(
        decoded,
        InboxEvent::LegacySyntheticAdmitted { .. }
    ));
    let mut fold = InboxFold::default();
    fold.apply(decoded, 0);
    assert!(fold.pending.is_empty());
    assert!(fold.operation_deliveries.is_empty());
    Ok(())
}
