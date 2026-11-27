//! A real reader-pool refusal must survive the production planning boundary.
use super::sustained_defects::product_plan;
use super::*;
use grapt_runtime::provider_source::PlanRefusalCause;
use grapt_runtime::AttemptPlan;
use grapt_store::read_pool::{ReadPoolError, READ_CAPACITY};

#[test]
fn local_capacity_after_planning_is_typed_and_admission_recovers_with_one_free_reader(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_runtime::provider_source::ContextAdmissionRefusal;
    let fixture = Fixture::new("planning-late-reader-contention")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "inspect")?;
    let planned = plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )?;
    let pool = lock(fixture.inbox()?)
        .store()
        .read_pool()?
        .ok_or("file reader pool absent")?;
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout()?);
    }
    assert_eq!(
        plan.context_admission(&planned),
        Err(ContextAdmissionRefusal::CapacityBusy)
    );
    held.pop().ok_or("held reader missing")?.finish()?;
    assert_eq!(plan.context_admission(&planned), Ok(None));
    for lease in held {
        lease.finish()?;
    }
    Ok(())
}

#[test]
fn local_capacity_product_plan_reconciles_background_results_through_the_writer(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_subagent::{SubagentAdmission, SubagentMode, SubagentService, SubagentTerminal};
    use grapt_types::{
        CasOutcome, EffectiveGraphId, OperationId, RuntimeGeneration, SubagentOperationId,
    };
    let fixture = Fixture::new("planning-background-writer")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let operation = SubagentOperationId::new("background_capacity_result").ok_or("operation")?;
    {
        let mut inbox = lock(fixture.inbox()?);
        let store = inbox.store_mut();
        SubagentService::ensure_schema_on(store)?;
        SubagentService::open_on(
            store,
            &SubagentAdmission {
                operation: OperationId::new("op_capacity_result").ok_or("operation family")?,
                subagent: operation.clone(),
                child: "ses_capacity_result".parse()?,
                parent: fixture.session.clone(),
                agent: grapt_types::AgentId::new("worker").ok_or("agent")?,
                model: None,
                description: "background inspection".into(),
                mode: SubagentMode::Background,
                tool_call: None,
                effective_graph: EffectiveGraphId::new("efg_capacity_result").ok_or("graph")?,
            },
            10,
        )?;
        assert_eq!(
            SubagentService::start_on(store, &operation, RuntimeGeneration::new(1), 11)?,
            CasOutcome::Applied
        );
        assert_eq!(
            SubagentService::settle_on(
                store,
                &operation,
                &SubagentTerminal::Failed {
                    error: "retained background result".into()
                },
                12
            )?,
            CasOutcome::Applied
        );
    }
    let mut plan = product_plan(&fixture, fixture.input.clone(), "inspect")?;
    let pool = lock(fixture.inbox()?)
        .store()
        .read_pool()?
        .ok_or("file reader pool absent")?;
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY - 1 {
        held.push(pool.checkout()?);
    }
    let planned = plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )?;
    let mail = planned
        .operation_mail
        .ok_or("the background result was not prepared")?;
    assert_eq!(mail.contents().len(), 1);
    assert!(mail.contents()[0].contains("retained background result"));
    // Read committed publication through the one free independent snapshot.
    // Preparing a request must neither use a query-only writer nor consume mail.
    let reader = pool.checkout()?;
    assert!(
        grapt_inbox::operation_mail::OperationMailBatch::has_unpresented_results(
            reader.store(),
            &fixture.session,
        )?
    );
    reader.finish()?;
    for lease in held {
        lease.finish()?;
    }
    Ok(())
}

#[test]
fn local_capacity_product_plan_keeps_busy_typed_and_reuses_its_input_after_release(
) -> Result<(), String> {
    let fixture = Fixture::new("planning-reader-contention")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "inspect")?;
    let pool = lock(fixture.inbox()?)
        .store()
        .read_pool()
        .map_err(|error| error.to_string())?
        .ok_or("file reader pool absent")?;
    let mut held = Vec::new();
    for _ in 0..READ_CAPACITY {
        held.push(pool.checkout().map_err(|error| error.to_string())?);
    }
    assert!(matches!(pool.checkout(), Err(ReadPoolError::Busy)));
    let step = StepNumber::new(1).ok_or("step")?;
    for _ in 0..3 {
        let refused = plan
            .plan(step, 1, grapt_runtime::HistoryGeneration::new(1))
            .err()
            .ok_or("a saturated reader pool produced a request")?;
        assert_eq!(
            refused.cause,
            PlanRefusalCause::CapacityBusy,
            "the production plan erased the typed reader refusal: {}",
            refused.detail
        );
        assert_eq!(
            refused.compaction, None,
            "reader contention requested compaction"
        );
    }
    // One free reader is enough for all nested history projections. A plan
    // that checks out a second reader will fail despite available capacity.
    held.pop()
        .ok_or("held reader missing")?
        .finish()
        .map_err(|error| error.to_string())?;
    plan.plan(step, 1, grapt_runtime::HistoryGeneration::new(1))
        .map_err(|refusal| {
            format!(
                "the original coding plan did not recover: {}",
                refusal.detail
            )
        })?;
    for lease in held {
        lease.finish().map_err(|error| error.to_string())?;
    }
    Ok(())
}

#[test]
fn local_capacity_product_plan_keeps_invalid_durable_input_terminal() -> Result<(), String> {
    let fixture = Fixture::new("planning-invalid-input")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "different input")?;
    let refused = plan
        .plan(
            StepNumber::new(1).ok_or("step")?,
            1,
            grapt_runtime::HistoryGeneration::new(1),
        )
        .err()
        .ok_or("a mismatched durable input produced a request")?;
    assert_eq!(refused.cause, PlanRefusalCause::Terminal);
    assert_eq!(refused.compaction, None);
    Ok(())
}
