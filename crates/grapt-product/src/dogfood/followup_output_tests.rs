//! Exact execution boundaries exclude both prior answers and later Steps.
use super::*;
use grapt_session::execution::{self, Outcome};
use grapt_types::ExecutionId;

fn observe(fixture: &Fixture, execution: &ExecutionId, outcome: Option<Outcome>) -> TestResult {
    let authorities = DeterministicAuthorities::new(&*fixture.app.clock, &*fixture.app.ids);
    let mut held = fixture.app.lock_inbox();
    match outcome {
        None => {
            execution::begin(held.store_mut(), &authorities, &fixture.child, execution)?;
        }
        Some(outcome) => {
            execution::settle(
                held.store_mut(),
                &authorities,
                &fixture.child,
                execution,
                outcome,
            )?;
        }
    }
    Ok(())
}

fn answer(fixture: &Fixture, marker: &str) -> TestResult {
    use grapt_provider::{AdapterState, AttemptFragment, BlockOrdinal, FinishReason};
    use grapt_runtime::{AttemptOwner, DurableAttemptOwner, DurableFact, NoLiveSubscribers};
    let step = crate::dogfood::execution::next_step(&fixture.app.inbox, &fixture.child)?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.app.inbox.clone(),
        fixture.app.executor.artifacts().shared_clone(),
        fixture.app.clock.clone(),
        fixture.app.ids.clone(),
        NoLiveSubscribers,
        fixture.child.clone(),
        step,
        None,
    )?;
    let attempt =
        grapt_types::PhysicalAttemptId::new(format!("pat_{}", fixture.app.ids.mint().as_str()))
            .ok_or("attempt")?;
    owner.commit(DurableFact::AttemptStarted { attempt })?;
    let block = BlockOrdinal::new(0);
    for fragment in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: marker.to_owned(),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ] {
        owner.commit(DurableFact::Fragment {
            fragment: Box::new(fragment),
        })?;
    }
    owner.commit(DurableFact::StepEnded {
        finish: FinishReason::Stop,
    })?;
    Ok(())
}

#[tokio::test]
async fn followup_output_never_uses_an_old_or_later_answer() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Background)?;
    let old = ExecutionId::new("exe_old").ok_or("old execution")?;
    observe(&fixture, &old, None)?;
    answer(&fixture, "old-child-answer")?;
    observe(&fixture, &old, Some(Outcome::Succeeded))?;
    for (name, outcome) in [
        ("empty", Outcome::Succeeded),
        ("failed", Outcome::Failed),
        ("interrupted", Outcome::Interrupted),
    ] {
        let id = ExecutionId::new(format!("exe_{name}")).ok_or("execution")?;
        observe(&fixture, &id, None)?;
        observe(&fixture, &id, Some(outcome))?;
        assert!(
            child_output_for_execution(&fixture.app, &fixture.child, &id).is_err(),
            "{name} execution inherited the earlier answer"
        );
    }
    let next = ExecutionId::new("exe_next").ok_or("next execution")?;
    observe(&fixture, &next, None)?;
    answer(&fixture, "exact-followup-answer")?;
    observe(&fixture, &next, Some(Outcome::Succeeded))?;
    answer(&fixture, "later-answer-must-not-leak")?;
    let text = child_output_for_execution(&fixture.app, &fixture.child, &next)
        .map_err(|error| error.reason().to_owned())?;
    assert!(text.contains("exact-followup-answer"), "{text}");
    assert!(
        !text.contains("old-child-answer") && !text.contains("later-answer"),
        "{text}"
    );
    Ok(())
}

#[test]
fn initial_capacity_refusal_reaches_the_parent_even_without_an_assistant_step() -> TestResult {
    let fixture = Fixture::new(SubagentMode::Foreground)?;
    let execution = ExecutionId::new("exe_capacity_refused").ok_or("execution")?;
    let authorities = DeterministicAuthorities::new(&*fixture.app.clock, &*fixture.app.ids);
    let (start, end) = {
        let mut held = fixture.app.lock_inbox();
        let start = execution::begin(held.store_mut(), &authorities, &fixture.child, &execution)?;
        let end = execution::settle_failed(
            held.store_mut(),
            &authorities,
            &fixture.child,
            &execution,
            execution::Failure::ProviderCapacity,
        )?;
        (start.position.seq, end.position.seq)
    };
    let result = child_output(
        &fixture.app,
        &fixture.child,
        start.saturating_sub(1),
        SubagentMode::Foreground,
    )
    .err()
    .ok_or("an unopened child was reported successful")?;
    assert!(
        result.reason().contains("retryable") && result.reason().contains("followup_task"),
        "{result:?}"
    );
    let old = child_output(&fixture.app, &fixture.child, end, SubagentMode::Foreground)
        .err()
        .ok_or("an old execution was reported successful")?;
    assert!(
        !old.reason().contains("retryable"),
        "a prior capacity failure leaked across the admission floor"
    );
    Ok(())
}
