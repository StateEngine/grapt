//! Review HOLD on 9abec016, finding 2: a requested /compact runs only before
//! the first attempt of the input that opens a chain, never mid-chain. Pinned
//! on the ordinary planner the real drain uses.

use super::*;
use grapt_runtime::AttemptPlan;
use grapt_types::CompactionTrigger;

fn plan_for(
    fixture: &Fixture,
    input: MessageId,
) -> Result<crate::dogfood::production::ProductPlan, String> {
    let targets = crate::dogfood::production::tests::projection_targets(
        grapt_provider::ProtocolFamily::ChatGptCodex,
    )?;
    let first = targets.issue()?;
    Ok(crate::dogfood::production::ProductPlan::new(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        "do the work".to_owned(),
        grapt_provider::PromptCacheKey::for_session(&fixture.session)
            .map_err(|error| error.to_string())?,
        Some(first),
        targets,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        fixture.continuation(input)?,
        StepNumber::new(1).ok_or("step")?,
    ))
}

fn requested(
    plan: &mut crate::dogfood::production::ProductPlan,
    step: u32,
    ordinal: u32,
) -> Result<bool, String> {
    Ok(matches!(
        plan.plan(
            StepNumber::new(step).ok_or("step")?,
            ordinal,
            grapt_runtime::HistoryGeneration::new(1),
        ),
        Err(refusal) if refusal.compaction == Some(CompactionTrigger::UserRequested)
    ))
}

#[test]
fn a_requested_compaction_is_planned_only_before_the_chains_first_attempt() -> Result<(), String> {
    let fixture = Fixture::new("requested-compaction-gate")?;
    let input: MessageId = "msg_requested_gate".parse().map_err(|_| "input id")?;
    promote_test_input(fixture.inbox()?, &fixture.session, &input, "do the work")?;
    let event: grapt_types::EventId = "evt_requested_gate".parse().map_err(|_| "event id")?;
    let request: MessageId = "msg_requested_gate_compact"
        .parse()
        .map_err(|_| "request id")?;
    lock(fixture.inbox()?)
        .admit_compaction(grapt_inbox::CompactionAdmitRequest {
            event_id: &event,
            session: &fixture.session,
            input: &request,
            created_ms: 1_789_560_000_000,
            instructions: None,
        })
        .map_err(|error| error.to_string())?;

    // A later Step of a running chain, or a later attempt of its first Step:
    // the request waits for the next input.
    assert!(!requested(&mut plan_for(&fixture, input.clone())?, 2, 1)?);
    assert!(!requested(&mut plan_for(&fixture, input.clone())?, 1, 2)?);
    // The first attempt of the chain's first Step runs it.
    assert!(requested(&mut plan_for(&fixture, input)?, 1, 1)?);
    Ok(())
}
