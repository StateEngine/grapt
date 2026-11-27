use super::*;

#[test]
fn stale_generation_has_zero_durable_delivery_changes() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_stale");
    open_running(&mut svc, "sub_stale", "ses_child", "ses_parent")?;
    svc.settle(&id, &completed("result")?, 20)?;
    let stale = DeliveryClaim {
        id: claim_id("dcl_stale"),
        route: ClaimRoute::OperationDelivery,
        parent: ses("ses_parent"),
        input: msg("msg_stale"),
        generation: RuntimeGeneration::new(2),
    };
    assert!(matches!(
        svc.claim(&id, &stale, 21),
        Err(SubagentError::StaleGeneration {
            presented: 2,
            terminal: 1,
            ..
        })
    ));
    assert_eq!(
        svc.get(&id)?.ok_or("operation")?.1,
        DeliveryStatus::Unclaimed
    );
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 0);

    assert_eq!(
        svc.claim(
            &id,
            &synthetic_claim("dcl_exact", "ses_parent", "msg_exact"),
            22,
        )?,
        ClaimOutcome::Won
    );
    assert_eq!(
        svc.deliver(
            &id,
            &claim_id("dcl_exact"),
            RuntimeGeneration::new(2),
            &ev("evt_stale"),
            23,
        )?,
        CasOutcome::NotLive
    );
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 0);
    Ok(())
}

#[test]
fn projection_oversize_rolls_back_event_and_delivery_state() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(ses("ses_parent")))?;
    let limits = DeliveryLimits::new(1, 4).ok_or("delivery limits")?;
    let mut svc = SubagentService::new(store, limits)?;
    let id = op("sub_oversize");
    open_running(&mut svc, "sub_oversize", "ses_child", "ses_parent")?;
    svc.settle(&id, &completed("five!")?, 20)?;
    svc.claim(
        &id,
        &synthetic_claim("dcl_oversize", "ses_parent", "msg_oversize"),
        21,
    )?;
    assert!(matches!(
        svc.deliver(
            &id,
            &claim_id("dcl_oversize"),
            RuntimeGeneration::new(1),
            &ev("evt_oversize"),
            22,
        ),
        Err(SubagentError::Inbox(
            grapt_inbox::InboxError::DeliveryCapacity { .. }
        ))
    ));
    assert!(matches!(
        svc.get(&id)?.ok_or("operation")?.1,
        DeliveryStatus::Delivering(_, ClaimRoute::OperationDelivery)
    ));
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 0);
    let events: i64 = svc.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.admitted.1'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(events, 0);
    Ok(())
}

#[test]
fn operation_family_cannot_be_rebound_to_another_subagent() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let first = admission(
        "sub_a",
        "ses_child_a",
        "ses_parent",
        SubagentMode::Background,
    );
    svc.open(&first, 10)?;
    let mut cross_family = admission(
        "sub_b",
        "ses_child_b",
        "ses_parent",
        SubagentMode::Background,
    );
    cross_family.operation = first.operation.clone();
    assert!(svc.open(&cross_family, 11).is_err());
    let count: i64 = svc.store().connection().query_row(
        "SELECT COUNT(*) FROM subagent_operation",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(
        count, 1,
        "cross-family refusal leaves no second durable row"
    );
    assert!(svc.get(&op("sub_b"))?.is_none());
    Ok(())
}

#[test]
fn acknowledgment_and_cleanup_are_separate_durable_transitions() -> TestResult {
    let mut svc = service_for(&["ses_parent"])?;
    let id = op("sub_ack");
    open_running(&mut svc, "sub_ack", "ses_child", "ses_parent")?;
    svc.settle(&id, &completed("result")?, 20)?;
    svc.claim(
        &id,
        &synthetic_claim("dcl_ack", "ses_parent", "msg_ack"),
        21,
    )?;
    svc.deliver(
        &id,
        &claim_id("dcl_ack"),
        RuntimeGeneration::new(1),
        &ev("evt_ack"),
        22,
    )?;
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 1);
    assert_eq!(
        svc.acknowledge(&id, &claim_id("dcl_foreign"), RuntimeGeneration::new(1), 23,)?,
        CasOutcome::NotLive
    );
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 1);
    assert_eq!(
        svc.acknowledge(&id, &claim_id("dcl_ack"), RuntimeGeneration::new(1), 24,)?,
        CasOutcome::Applied
    );
    assert!(matches!(
        svc.get(&id)?.ok_or("operation")?.1,
        DeliveryStatus::Acknowledged(_, ClaimRoute::OperationDelivery, _)
    ));
    assert_eq!(parent_admissions(&svc, "ses_parent")?, 0);
    assert_eq!(
        svc.cleanup(&id, &claim_id("dcl_ack"), RuntimeGeneration::new(1), 25,)?,
        CasOutcome::Applied
    );
    assert!(matches!(
        svc.get(&id)?.ok_or("operation")?.1,
        DeliveryStatus::Cleaned(_, ClaimRoute::OperationDelivery, _)
    ));
    let (events, artifacts): (i64, i64) = svc.store().connection().query_row(
        "SELECT
             (SELECT COUNT(*) FROM event WHERE id = 'evt_ack'),
             (SELECT COUNT(*) FROM subagent_operation
              WHERE operation_id = 'sub_ack' AND artifact_id IS NOT NULL)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((events, artifacts), (1, 1));
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn predecessor_active_schema_is_transactionally_rebuilt_and_restart_safe() -> TestResult {
    let dir = std::env::temp_dir().join(format!(
        "grapt-subagent-predecessor-migration-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("sub.db");
    {
        let mut store = EventStore::open(&db, "0.0.1")?;
        store.elect_writer(&AggregateId::Session(ses("ses_parent")))?;
        store.connection().execute_batch(
            "CREATE TABLE subagent_operation (
                operation_id TEXT PRIMARY KEY,
                family_operation_id TEXT NOT NULL UNIQUE,
                child_session_id TEXT NOT NULL,
                parent_session_id TEXT NOT NULL,
                agent TEXT NOT NULL,
                model_provider TEXT,
                model_id TEXT,
                model_variant TEXT,
                description TEXT NOT NULL,
                mode TEXT NOT NULL CHECK (mode IN ('background', 'foreground')),
                tool_call_id TEXT,
                effective_graph_id TEXT NOT NULL,
                status TEXT NOT NULL CHECK (status IN
                    ('admitted', 'running', 'completed', 'failed', 'cancelled', 'ambiguous')),
                generation INTEGER,
                run_generation INTEGER,
                output TEXT,
                artifact_id TEXT,
                artifact_length INTEGER,
                artifact_digest TEXT,
                artifact_disposition TEXT,
                error TEXT,
                reason TEXT CHECK (reason IS NULL OR reason IN ('process-loss', 'interrupted')),
                evidence TEXT,
                delivery TEXT NOT NULL CHECK (delivery IN ('unclaimed', 'delivering', 'delivered')),
                claim_id TEXT,
                claim_generation INTEGER,
                claim_route TEXT CHECK (claim_route IS NULL OR claim_route IN ('synthetic', 'tool-result')),
                target_parent TEXT,
                target_input TEXT,
                time_delivered INTEGER,
                time_acknowledged INTEGER,
                time_cleaned INTEGER,
                time_created INTEGER NOT NULL,
                time_updated INTEGER NOT NULL,
                CHECK ((status = 'running') = (generation IS NOT NULL)),
                CHECK ((status = 'completed') = (output IS NOT NULL)),
                CHECK ((status = 'completed') = (artifact_id IS NOT NULL)),
                CHECK ((artifact_id IS NULL) = (artifact_length IS NULL)),
                CHECK ((artifact_id IS NULL) = (artifact_digest IS NULL)),
                CHECK ((artifact_id IS NULL) = (artifact_disposition IS NULL)),
                CHECK ((status = 'failed') = (error IS NOT NULL)),
                CHECK ((status = 'cancelled') = (reason IS NOT NULL)),
                CHECK ((status = 'ambiguous') = (evidence IS NOT NULL)),
                CHECK ((delivery = 'unclaimed') = (claim_id IS NULL)),
                CHECK ((claim_id IS NULL) = (claim_route IS NULL)),
                CHECK ((claim_id IS NULL) = (claim_generation IS NULL)),
                CHECK ((claim_id IS NULL) = (target_parent IS NULL)),
                CHECK ((claim_id IS NULL) = (target_input IS NULL)),
                CHECK ((delivery = 'delivered') = (time_delivered IS NOT NULL)),
                CHECK (time_acknowledged IS NULL OR delivery = 'delivered'),
                CHECK (time_cleaned IS NULL OR time_acknowledged IS NOT NULL),
                CHECK (delivery = 'unclaimed'
                       OR status IN ('completed', 'failed', 'cancelled')),
                CHECK (target_parent IS NULL OR target_parent = parent_session_id),
                CHECK ((model_provider IS NULL) = (model_id IS NULL)),
                CHECK (model_variant IS NULL OR model_id IS NOT NULL)
             );
             INSERT INTO subagent_operation
               (operation_id, family_operation_id, child_session_id, parent_session_id,
                agent, description, mode, effective_graph_id, status, run_generation,
                output, artifact_id, artifact_length, artifact_digest, artifact_disposition,
                delivery, claim_id, claim_generation, claim_route, target_parent, target_input,
                time_delivered, time_created, time_updated)
             VALUES
               ('sub_pred_completed', 'op_pred_completed', 'ses_child_completed', 'ses_parent',
                'test', 'completed', 'background', 'efg_test', 'completed', 1,
                'done', 'art_pred', 4, 'digest_pred', 'complete',
                'delivered', 'dcl_pred_completed', 1, 'synthetic', 'ses_parent',
                'msg_pred_completed', 20, 10, 20);
             INSERT INTO subagent_operation
               (operation_id, family_operation_id, child_session_id, parent_session_id,
                agent, description, mode, effective_graph_id, status, run_generation, error,
                delivery, claim_id, claim_generation, claim_route, target_parent, target_input,
                time_delivered, time_created, time_updated)
             VALUES
               ('sub_pred_failed', 'op_pred_failed', 'ses_child_failed', 'ses_parent',
                'test', 'failed', 'background', 'efg_test', 'failed', 1, 'provider refused',
                'delivered', 'dcl_pred_failed', 1, 'synthetic', 'ses_parent',
                'msg_pred_failed', 21, 10, 21);
             INSERT INTO subagent_operation
               (operation_id, family_operation_id, child_session_id, parent_session_id,
                agent, description, mode, effective_graph_id, status, run_generation, reason,
                delivery, claim_id, claim_generation, claim_route, target_parent, target_input,
                time_created, time_updated)
             VALUES
               ('sub_pred_cancelled', 'op_pred_cancelled', 'ses_child_cancelled', 'ses_parent',
                'test', 'cancelled', 'background', 'efg_test', 'cancelled', 1, 'process-loss',
                'delivering', 'dcl_pred_cancelled', 1, 'synthetic', 'ses_parent',
                'msg_pred_cancelled', 10, 22);
             INSERT INTO subagent_operation
               (operation_id, family_operation_id, child_session_id, parent_session_id,
                agent, description, mode, effective_graph_id, status, run_generation, evidence,
                delivery, time_created, time_updated)
             VALUES
               ('sub_pred_ambiguous', 'op_pred_ambiguous', 'ses_child_ambiguous', 'ses_parent',
                'test', 'ambiguous', 'background', 'efg_test', 'ambiguous', 1,
                'effect may have committed', 'unclaimed', 10, 23);",
        )?;
        let limits = DeliveryLimits::new(8, 4096).ok_or("delivery limits")?;
        let mut svc = SubagentService::new(store, limits)?;
        let settlements: (
            Option<String>,
            Option<String>,
            Option<String>,
            Option<String>,
        ) = svc.store().connection().query_row(
            "SELECT
                   (SELECT delivery_settlement FROM subagent_operation
                    WHERE operation_id = 'sub_pred_completed'),
                   (SELECT delivery_settlement FROM subagent_operation
                    WHERE operation_id = 'sub_pred_failed'),
                   (SELECT delivery_settlement FROM subagent_operation
                    WHERE operation_id = 'sub_pred_cancelled'),
                   (SELECT delivery_settlement FROM subagent_operation
                    WHERE operation_id = 'sub_pred_ambiguous')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )?;
        assert_eq!(
            settlements,
            (Some("completed".into()), Some("failed".into()), None, None)
        );

        assert_eq!(
            svc.deliver(
                &op("sub_pred_cancelled"),
                &claim_id("dcl_pred_cancelled"),
                RuntimeGeneration::new(1),
                &ev("evt_pred_cancelled"),
                30,
            )?,
            CasOutcome::Applied
        );
        assert_eq!(
            svc.claim(
                &op("sub_pred_ambiguous"),
                &synthetic_claim("dcl_pred_ambiguous", "ses_parent", "msg_pred_ambiguous"),
                31,
            )?,
            ClaimOutcome::Won
        );
        assert_eq!(
            svc.deliver(
                &op("sub_pred_ambiguous"),
                &claim_id("dcl_pred_ambiguous"),
                RuntimeGeneration::new(1),
                &ev("evt_pred_ambiguous"),
                32,
            )?,
            CasOutcome::Applied
        );
    }

    let limits = DeliveryLimits::new(8, 4096).ok_or("delivery limits")?;
    let reopened = SubagentService::new(EventStore::open(&db, "0.0.1")?, limits)?;
    assert!(matches!(
        reopened
            .get(&op("sub_pred_completed"))?
            .ok_or("completed")?
            .1,
        DeliveryStatus::Delivered(_, ClaimRoute::OperationDelivery)
    ));
    assert!(matches!(
        reopened.get(&op("sub_pred_failed"))?.ok_or("failed")?.1,
        DeliveryStatus::Failed(_, ClaimRoute::OperationDelivery)
    ));
    assert!(matches!(
        reopened
            .get(&op("sub_pred_cancelled"))?
            .ok_or("cancelled")?
            .1,
        DeliveryStatus::Failed(_, ClaimRoute::OperationDelivery)
    ));
    assert!(matches!(
        reopened
            .get(&op("sub_pred_ambiguous"))?
            .ok_or("ambiguous")?
            .1,
        DeliveryStatus::Quarantined(_, ClaimRoute::OperationDelivery)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn legacy_subagent_table_is_preserved_as_inert_audit() -> TestResult {
    let store = EventStore::open_in_memory()?;
    store.connection().execute_batch(
        "CREATE TABLE subagent_operation (
             operation_id TEXT PRIMARY KEY,
             status TEXT NOT NULL,
             delivery TEXT NOT NULL,
             output TEXT
         );
         INSERT INTO subagent_operation(operation_id, status, delivery, output)
         VALUES ('sub_legacy', 'completed', 'delivered', 'synthetic row');",
    )?;
    let limits = DeliveryLimits::new(1, 1024).ok_or("delivery limits")?;
    let svc = SubagentService::new(store, limits)?;
    let legacy: i64 = svc.store().connection().query_row(
        "SELECT COUNT(*) FROM subagent_operation_legacy_synthetic
         WHERE operation_id = 'sub_legacy'",
        [],
        |row| row.get(0),
    )?;
    let active: i64 = svc.store().connection().query_row(
        "SELECT COUNT(*) FROM subagent_operation",
        [],
        |row| row.get(0),
    )?;
    assert_eq!((legacy, active), (1, 0));
    assert!(svc.get(&op("sub_legacy"))?.is_none());
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn failed_process_loss_and_quarantine_are_durable_parent_debts() -> TestResult {
    let dir = std::env::temp_dir().join(format!(
        "grapt-subagent-settlement-restart-{}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("sub.db");
    let failed = op("sub_failed_debt");
    let lost = op("sub_process_loss_debt");
    let ambiguous = op("sub_quarantine_debt");
    {
        let mut store = EventStore::open(&db, "0.0.1")?;
        store.elect_writer(&AggregateId::Session(ses("ses_parent")))?;
        let limits = DeliveryLimits::new(8, 4096).ok_or("delivery limits")?;
        let mut svc = SubagentService::new(store, limits)?;

        open_running(
            &mut svc,
            "sub_failed_debt",
            "ses_child_failed",
            "ses_parent",
        )?;
        svc.settle(
            &failed,
            &SubagentTerminal::Failed {
                error: "provider refused".to_owned(),
            },
            20,
        )?;

        open_running(
            &mut svc,
            "sub_process_loss_debt",
            "ses_child_lost",
            "ses_parent",
        )?;
        assert_eq!(
            svc.settle_dead_generation(RuntimeGeneration::new(2), 21)?,
            vec![lost.clone()]
        );

        open_running(
            &mut svc,
            "sub_quarantine_debt",
            "ses_child_ambiguous",
            "ses_parent",
        )?;
        let evidence = AmbiguityEvidence::new("provider effect may have committed")
            .ok_or("ambiguity evidence")?;
        assert_eq!(
            svc.mark_ambiguous(&ambiguous, &evidence, 22)?,
            CasOutcome::Applied
        );

        for (id, claim, input, event) in [
            (&failed, "dcl_failed", "msg_failed", "evt_failed"),
            (&lost, "dcl_lost", "msg_lost", "evt_lost"),
            (
                &ambiguous,
                "dcl_quarantined",
                "msg_quarantined",
                "evt_quarantined",
            ),
        ] {
            assert_eq!(
                svc.claim(id, &synthetic_claim(claim, "ses_parent", input), 23)?,
                ClaimOutcome::Won
            );
            assert_eq!(
                svc.deliver(
                    id,
                    &claim_id(claim),
                    RuntimeGeneration::new(1),
                    &ev(event),
                    24,
                )?,
                CasOutcome::Applied
            );
        }
        assert_eq!(parent_admissions(&svc, "ses_parent")?, 3);
        let speech: i64 = svc.store().connection().query_row(
            "SELECT COUNT(*) FROM event
             WHERE type = 'session.input.admitted.1'
               AND json_extract(data, '$.input.type') IN ('user', 'synthetic')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(speech, 0, "settlement debt is never synthetic speech");
    }

    let limits = DeliveryLimits::new(8, 4096).ok_or("delivery limits")?;
    let mut reopened = SubagentService::new(EventStore::open(&db, "0.0.1")?, limits)?;
    assert!(matches!(
        reopened.get(&failed)?.ok_or("failed operation")?.1,
        DeliveryStatus::Failed(_, ClaimRoute::OperationDelivery)
    ));
    assert!(matches!(
        reopened.get(&lost)?.ok_or("lost operation")?.1,
        DeliveryStatus::Failed(_, ClaimRoute::OperationDelivery)
    ));
    assert!(matches!(
        reopened.get(&ambiguous)?.ok_or("ambiguous operation")?.1,
        DeliveryStatus::Quarantined(_, ClaimRoute::OperationDelivery)
    ));
    assert_eq!(parent_admissions(&reopened, "ses_parent")?, 3);

    assert_eq!(
        reopened.acknowledge(
            &failed,
            &claim_id("dcl_failed"),
            RuntimeGeneration::new(1),
            30,
        )?,
        CasOutcome::Applied
    );
    assert!(matches!(
        reopened.get(&failed)?.ok_or("failed operation")?.1,
        DeliveryStatus::Acknowledged(_, ClaimRoute::OperationDelivery, DeliverySettlement::Failed)
    ));
    assert_eq!(parent_admissions(&reopened, "ses_parent")?, 2);
    assert_eq!(
        reopened.cleanup(
            &failed,
            &claim_id("dcl_failed"),
            RuntimeGeneration::new(1),
            31,
        )?,
        CasOutcome::Applied
    );
    assert!(matches!(
        reopened.get(&failed)?.ok_or("failed operation")?.1,
        DeliveryStatus::Cleaned(_, ClaimRoute::OperationDelivery, DeliverySettlement::Failed)
    ));
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
