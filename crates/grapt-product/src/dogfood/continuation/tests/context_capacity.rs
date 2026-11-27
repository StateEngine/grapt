//! SC-REL-07a: a request-context capacity refusal reaches compaction by type.
//!
//! Before 07a the plan compared one literal, so a capacity refusal worded any
//! other way ended the Step as `PlanRefused` and failed the same way on every
//! later plan. Each producer now names its bound and unit; the plan asks for
//! compaction from that type alone.

use super::super::sustained_defects::{assert_plan_recovers, product_plan};
use super::*;
use crate::dogfood::continuation::context_error::{Bound, Capacity, ContextError, Unit};
use grapt_runtime::AttemptPlan;
use grapt_types::CompactionId;

fn promote(fixture: &Fixture, input: &str, text: &str) -> Result<MessageId, String> {
    let input = input
        .parse::<MessageId>()
        .map_err(|error| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &input, text)?;
    Ok(input)
}

fn exchanges(fixture: &Fixture, steps: std::ops::RangeInclusive<u32>) -> Result<(), String> {
    for step in steps {
        let mut current = owner(fixture, step)?;
        text(&mut current, true)?;
        let called = call(&mut current, &format!("cap-{step}"))?;
        result(&mut current, called)?;
        current
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
    }
    Ok(())
}

fn plan_once(
    plan: &mut crate::dogfood::production::ProductPlan,
    generation: u64,
) -> Result<Result<(), grapt_runtime::PlanRefusal>, String> {
    Ok(plan
        .plan(
            StepNumber::new(1).ok_or("step")?,
            1,
            grapt_runtime::HistoryGeneration::new(generation),
        )
        .map(|_| ()))
}

fn compact(fixture: &Fixture, current: &MessageId, prompt: &str, id: &str) -> Result<(), String> {
    let id = CompactionId::new(id).ok_or("compaction")?;
    let selected = fixture
        .continuation(current.clone())?
        .prepare_summary(prompt, &id, 1_000_000)
        .map_err(|error| format!("prepare {}: {error}", id.as_str()))?;
    super::super::summary_tests::commit_selection(fixture, &selected, id)
}

/// One settled step that answers with `size` bytes of assistant text.
fn long_answer(fixture: &Fixture, step: u32, size: usize) -> Result<(), String> {
    let mut current = owner(fixture, step)?;
    let block = grapt_provider::BlockOrdinal::new(0);
    for held in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: "a".repeat(size),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ] {
        fragment(&mut current, held)?;
    }
    current
        .commit(DurableFact::StepEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())
}

/// A compacted history, then `steps` tool exchanges, `answers` text-only
/// answers and `users` extra inputs, then the current input.
fn item_suffix(
    label: &str,
    steps: u32,
    answers: u32,
    users: u32,
    current_text: &str,
) -> Result<(Fixture, MessageId), String> {
    let fixture = Fixture::new(label)?;
    for index in 0..3 {
        promote(&fixture, &format!("msg_cap_old_{index}"), "an old prompt")?;
    }
    let old = promote(&fixture, "msg_cap_old_3", "the last old prompt")?;
    exchanges(&fixture, 1..=4)?;
    compact(
        &fixture,
        &old,
        "the last old prompt",
        &format!("{label}-first"),
    )?;
    exchanges(&fixture, 5..=4 + steps)?;
    for answer in 0..answers {
        long_answer(&fixture, 5 + steps + answer, 1)?;
    }
    for user in 0..users {
        promote(&fixture, &format!("msg_cap_extra_{user}"), "and also")?;
    }
    let current = promote(&fixture, "msg_cap_current", current_text)?;
    Ok((fixture, current))
}

fn request_items(fixture: &Fixture, current: MessageId) -> Result<usize, String> {
    let context = fixture
        .continuation(current)?
        .request_context("current", false, None, true)
        .map_err(|error| format!("probe: {error}"))?;
    Ok(context.rows.len() + context.response_order.boundaries() + context.efforts.len())
}

/// Grow the retained suffix until its own items are exactly the item bound,
/// so the retained summary is the one item past it. A tool exchange is four
/// items (answer, call, result, response marker), a text answer two, an input one.
fn at_the_item_bound(label: &str) -> Result<(Fixture, MessageId), String> {
    let (probe, current) = item_suffix(&format!("{label}-p"), 0, 0, 0, "current")?;
    let base = request_items(&probe, current)?;
    let short = MAX_CONTEXT_ITEMS
        .checked_sub(base)
        .ok_or("the probe already exceeds the item bound")?;
    let steps = u32::try_from(short / 4).map_err(|error| error.to_string())?;
    let rest = short % 4;
    let (answers, users) = (u32::from(rest >= 2), u32::from(rest % 2 == 1));
    item_suffix(&format!("{label}-r"), steps, answers, users, "current")
}

/// The item probe sees response groups too, so this overflow is the request
/// assembly's backstop; it is still a typed item capacity, never bytes.
#[test]
fn sc_rel_07a_a_response_group_overflow_is_an_item_capacity() -> Result<(), String> {
    let fixture = Fixture::new("cap-response-groups")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    // One user, then three rows and one response marker per step.
    let steps = u32::try_from(MAX_CONTEXT_ITEMS / 4).map_err(|error| error.to_string())?;
    exchanges(&fixture, 1..=steps)?;
    let error = fixture
        .continuation(fixture.input.clone())?
        .request_context("inspect", true, None, true)
        .err()
        .ok_or("an over-bound response history produced a request")?;
    assert_eq!(
        error.capacity_bound(),
        Some(Capacity {
            bound: Bound::ResponseGroups,
            unit: Unit::Items,
            observed: 1 + 4 * u64::from(steps),
            limit: MAX_CONTEXT_ITEMS as u64,
        })
    );
    assert_eq!(
        error.to_string(),
        "response groups exceed the request item bound"
    );
    Ok(())
}

/// The structural item probe counts the retained suffix but not the summary
/// or facts it is sent with, so a suffix exactly at the item bound passed every
/// probe and was refused as `PlanRefused` on every later plan.
#[test]
fn sc_rel_07a_a_summary_and_retained_overflow_compacts_instead_of_refusing_the_plan(
) -> Result<(), String> {
    let (fixture, current) = at_the_item_bound("cap-sr")?;
    let prompt = "current";
    let continuation = fixture.continuation(current.clone())?;
    assert!(
        continuation.structural_admission()?.is_none(),
        "a probe saw it"
    );
    let error = continuation
        .request_context(prompt, false, None, true)
        .err()
        .ok_or("the summary did not push the suffix over the bound")?;
    let Some(capacity) = error.capacity_bound() else {
        panic!("a summary overflow was not a capacity: {error}")
    };
    assert_eq!(
        (capacity.bound, capacity.unit, capacity.limit),
        (
            Bound::SummaryAndRetained,
            Unit::Items,
            MAX_CONTEXT_ITEMS as u64
        )
    );
    assert!(capacity.observed > capacity.limit);

    let diagnosis = crate::dogfood::continuation::admission::DiagnosisSlot::default();
    let mut plan = product_plan(&fixture, current.clone(), prompt)?
        .with_admission_diagnosis(diagnosis.clone());
    let Err(refused) = plan_once(&mut plan, 1)? else {
        panic!("an over-bound request context produced a plan")
    };
    assert_eq!(
        refused.detail,
        "summary and retained history exceed the request bound"
    );
    assert_eq!(
        refused.compaction,
        Some(grapt_types::CompactionTrigger::StructuralBound),
        "a capacity refusal must ask for compaction"
    );
    // The diagnosis is the producer's own count, in items, never the
    // synthetic byte figure an item overflow used to inherit.
    assert_eq!(
        diagnosis.matching(grapt_types::CompactionTrigger::StructuralBound),
        Some(
            crate::dogfood::continuation::admission::AdmissionDiagnosis {
                trigger: grapt_types::CompactionTrigger::StructuralBound,
                estimate_basis:
                    crate::dogfood::continuation::admission::EstimateBasis::StructuralWindow,
                estimate: capacity.observed,
                budget: MAX_CONTEXT_ITEMS as u64,
            }
        )
    );
    compact(&fixture, &current, prompt, "cap-sr-second")?;
    assert_plan_recovers(&mut plan)
}

/// An overflow compaction cannot shrink (the current input alone exceeds the
/// bound, and every summary request carries it) stays the same typed capacity:
/// the summary cannot be prepared, nothing is committed, and the next plan
/// refuses the same way. The Step therefore ends in a bounded compaction
/// failure (see the runtime's rebuild bound), never a `PlanRefused` loop.
#[test]
fn sc_rel_07a_an_overflow_compaction_cannot_shrink_stays_a_capacity_refusal() -> Result<(), String>
{
    let prompt = "c".repeat(MAX_CONTEXT_BYTES);
    let (fixture, current) = item_suffix("cap-np", 2, 1, 0, &prompt)?;
    let mut plan = product_plan(&fixture, current.clone(), &prompt)?;
    for generation in 1..=2 {
        let Err(refused) = plan_once(&mut plan, generation)? else {
            panic!("an over-bound request context produced a plan")
        };
        assert_eq!(
            refused.compaction,
            Some(grapt_types::CompactionTrigger::StructuralBound),
            "the unshrinkable overflow became a plan refusal: {}",
            refused.detail
        );
        let id = CompactionId::new(format!("cap-np-{generation}")).ok_or("compaction")?;
        assert!(
            fixture
                .continuation(current.clone())?
                .prepare_summary(&prompt, &id, 1_000_000)
                .is_err(),
            "a summary was prepared for history that cannot shrink"
        );
    }
    Ok(())
}

/// A failure that is not capacity never asks for compaction, whatever its text.
#[test]
fn sc_rel_07a_a_non_capacity_failure_never_compacts() -> Result<(), String> {
    let fixture = Fixture::new("cap-failure")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let error = fixture
        .continuation(fixture.input.clone())?
        .request_context("a different prompt", false, None, true)
        .err()
        .ok_or("a mismatched prompt produced a request")?;
    assert_eq!(
        error,
        ContextError::Failed("current prompt differs from its durable input".to_owned())
    );
    assert_eq!(error.compaction_trigger(), None);
    let mut plan = product_plan(&fixture, fixture.input.clone(), "a different prompt")?;
    let Err(refused) = plan_once(&mut plan, 1)? else {
        panic!("a mismatched prompt produced a plan")
    };
    assert_eq!(refused.compaction, None, "{}", refused.detail);
    Ok(())
}
