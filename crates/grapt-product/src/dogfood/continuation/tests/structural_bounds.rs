//! SC-COMPACT-01 fix 1: the structural bounds at their production values.
//! Tokens trigger compaction; the row bounds and the declared-bytes backstop
//! only catch what tokens cannot see. Every boundary here is the production
//! constant, never a testkit value.

use grapt_runtime::AttemptPlan;

use super::sustained_defects::{
    append_malformed_session_event, assert_structural_refusal, product_plan, settled_step,
    step_owner,
};
use super::*;

/// Settle tiny calls in Steps of at most 32 until the window holds `total`
/// calls, counting the recorded exchange's one call.
fn settle_to(fixture: &Fixture, total: usize, label: &str) -> Result<(), String> {
    let mut held = 1;
    let mut step = 2;
    while held < total {
        let calls = (total - held).min(32);
        settled_step(fixture, step, calls, label)?;
        held += calls;
        step += 1;
    }
    Ok(())
}

/// A plan over the fixture's current input with the model window `tokens`.
fn plan_with_window(
    fixture: &Fixture,
    tokens: u64,
) -> Result<crate::dogfood::production::ProductPlan, String> {
    let targets = crate::dogfood::production::tests::projection_targets_with_window(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        tokens,
    )?;
    let first = targets.issue()?;
    Ok(crate::dogfood::production::ProductPlan::new(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        "use the tool".to_owned(),
        grapt_provider::PromptCacheKey::for_session(&fixture.session)
            .map_err(|error| error.to_string())?,
        Some(first),
        targets,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        fixture.continuation(fixture.input.clone())?,
        StepNumber::new(1).ok_or("step")?,
    ))
}

/// W1: 200 small calls, far under the token threshold, are no reason to
/// compact (the old 128-call bound compacted at the 129th); the same window
/// under a model window it fills compacts on the token threshold.
#[test]
fn w1_two_hundred_small_calls_compact_only_on_tokens() -> Result<(), String> {
    let fixture = Fixture::new("fix1-w1")?;
    fixture.record_exchange()?;
    settle_to(&fixture, 200, "w1")?;
    let step = StepNumber::new(1).ok_or("step")?;
    let mut plan = plan_with_window(&fixture, 128_000)?;
    let planned = plan
        .plan(step, 1, grapt_runtime::HistoryGeneration::new(1))
        .map_err(|refusal| format!("200 small calls refused: {}", refusal.detail))?;
    assert_eq!(plan.context_admission(&planned), Ok(None));
    let mut tight = plan_with_window(&fixture, 1_000)?;
    let planned = tight
        .plan(step, 1, grapt_runtime::HistoryGeneration::new(1))
        .map_err(|refusal| format!("the tight window refused to plan: {}", refusal.detail))?;
    assert_eq!(
        tight.context_admission(&planned),
        Ok(Some(grapt_types::CompactionTrigger::PolicyThreshold)),
        "the token threshold no longer compacts"
    );
    Ok(())
}

/// Exactly the tool-call page bound plans and builds; one call more is the
/// structural refusal.
#[test]
fn the_tool_call_page_bound_plans_and_one_more_is_structural() -> Result<(), String> {
    let fixture = Fixture::new("fix1-calls-at-bound")?;
    fixture.record_exchange()?;
    settle_to(&fixture, MAX_CONTINUATION_ROWS, "at")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    continuation
        .request_input("use the tool", true)
        .map_err(|error| format!("{MAX_CONTINUATION_ROWS} calls refused: {error}"))?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )
    .map_err(|refusal| format!("the bound itself compacted: {}", refusal.detail))?;

    settled_step(&fixture, 1_000, 1, "over")?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)
}

/// Exactly the promoted page bound of inputs plans and builds; the
/// first-over case is `swr_c1_promoted_inputs_compact_before_the_page_bound`.
#[test]
fn the_promoted_page_bound_plans() -> Result<(), String> {
    let fixture = Fixture::new("fix1-users-at-bound")?;
    let mut last = None;
    for index in 0..MAX_CONTINUATION_ROWS {
        let input = format!("msg_fix1_user_{index:04}")
            .parse::<MessageId>()
            .map_err(|error| error.to_string())?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &input,
            &format!("prompt {index}"),
        )?;
        last = Some(input);
    }
    let last = last.ok_or("no input")?;
    let prompt = format!("prompt {}", MAX_CONTINUATION_ROWS - 1);
    settled_step(&fixture, 1, 1, "users-at")?;
    fixture
        .continuation(last.clone())?
        .request_input(&prompt, false)
        .map_err(|error| format!("{MAX_CONTINUATION_ROWS} inputs refused: {error}"))?;
    let mut plan = product_plan(&fixture, last, &prompt)?;
    plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )
    .map(|_| ())
    .map_err(|refusal| format!("the promoted bound itself compacted: {}", refusal.detail))
}

/// Many large artifacts: every call's output declares 64 KiB, 64 MiB over
/// the window. The request refuses into a structural compaction having
/// fetched no more than the request byte bound plus one artifact.
#[test]
fn many_large_outputs_refuse_before_their_payloads_are_read() -> Result<(), String> {
    const OUTPUT_BYTES: usize = 64 * 1024;
    let fixture = Fixture::new("fix1-many-large")?;
    fixture.record_exchange()?;
    let output = "x".repeat(OUTPUT_BYTES);
    let mut held = 1;
    let mut step = 2;
    while held < MAX_CONTINUATION_ROWS {
        let calls = (MAX_CONTINUATION_ROWS - held).min(32);
        let mut owner = step_owner(&fixture, step)?;
        for index in 0..calls {
            let call = ToolCallId::new(format!("large-{step}-{index}")).ok_or("call")?;
            owner
                .commit(DurableFact::Fragment {
                    fragment: Box::new(AttemptFragment::ToolCalled {
                        call: call.clone(),
                        tool: READ_TOOL.to_owned(),
                        input: "{}".to_owned(),
                        execution: ToolExecution::Local,
                        state: AdapterState::none(),
                    }),
                })
                .map_err(|error| error.to_string())?;
            let captured = owner
                .capture_hosted_output(&output)
                .map_err(|error| error.to_string())?;
            owner
                .commit(DurableFact::ToolTerminal {
                    call,
                    outcome: ToolTerminalOutcome::Completed {
                        output: Some(captured),
                    },
                })
                .map_err(|error| error.to_string())?;
        }
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
        held += calls;
        step += 1;
    }
    let continuation = fixture.continuation(fixture.input.clone())?;
    FETCHED_BYTES.with(|fetched| fetched.set(0));
    let refused = continuation.request_input("use the tool", true);
    let fetched = FETCHED_BYTES.with(|fetched| fetched.get());
    assert!(
        refused.is_err(),
        "64 MiB of declared output produced a request"
    );
    assert!(
        fetched <= MAX_CONTEXT_BYTES + MAX_CODEX_TOOL_TEXT_BYTES,
        "the refused request fetched {fetched} bytes"
    );
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)
}

/// The tool probe counts exactly what the tool-call page reads, malformed
/// rows included. At 1024 settled calls every other probe is still in
/// bound, so one malformed tool row past them is the tool probe's alone: it
/// must ask for a structural compaction rather than leave the page read to
/// refuse.
#[test]
fn the_tool_probe_counts_a_malformed_row_past_the_page_bound() -> Result<(), String> {
    let fixture = Fixture::new("fix1-tool-probe")?;
    fixture.record_exchange()?;
    settle_to(&fixture, MAX_CONTINUATION_ROWS, "probe")?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )
    .map_err(|refusal| format!("the bound itself compacted: {}", refusal.detail))?;
    append_malformed_session_event(
        &fixture,
        "evt_fix1_malformed_tool",
        "session.content.appended",
        1,
    )?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)
}

/// History past the read bounds is summarized from its NEWEST window: the cut
/// sits near the head and the exact tail is small, where the oldest-prefix
/// choice cut near the start and left the whole rest retained. The oldest
/// rows leave the quote, as a prior-art compactor drops the oldest item of an
/// input that does not fit.
#[test]
fn history_past_the_read_bounds_summarizes_its_newest_window() -> Result<(), String> {
    let fixture = Fixture::new("fix1-newest-window")?;
    fixture.record_exchange()?;
    settle_to(&fixture, MAX_CONTINUATION_ROWS + 256, "newest")?;
    let id = grapt_types::CompactionId::new("cmp_newest_window").ok_or("compaction id")?;
    let mut continuation = fixture.continuation(fixture.input.clone())?;
    // A 40k-token window leaves the quote about 23 KB: far under the history.
    let oldest = continuation.prepare_summary("use the tool", &id, 40_000)?;
    let floor = continuation
        .summary_window_floor()?
        .ok_or("a history past the read bounds has a window floor")?;
    continuation.set_summary_floor(Some(floor));
    let newest = continuation.prepare_summary("use the tool", &id, 40_000)?;
    assert!(
        newest.selection.from > oldest.selection.from,
        "the newest window cut at {} not past the oldest-prefix cut {}",
        newest.selection.from,
        oldest.selection.from
    );
    assert!(newest.selection.from >= floor.value());
    // What stays exact after the cut is what lies between it and the head.
    let tail = |prepared: &super::super::summarization::SummaryPreparation| {
        prepared.selection.source_head - prepared.selection.from
    };
    assert!(
        tail(&newest) < tail(&oldest),
        "the exact tail spans {} rows against {}",
        tail(&newest),
        tail(&oldest)
    );
    assert!(
        newest.text.len() <= 40_000 - 16_384,
        "the quote exceeds its budget"
    );
    assert!(newest.removed_bytes > 0);
    Ok(())
}

/// After a compaction that did not fit, history still past the read bounds
/// runs the next input as a trimmed request from the newest window those
/// bounds serve, not as another compaction.
#[test]
fn a_trimmed_retry_past_the_read_bounds_plans_instead_of_compacting() -> Result<(), String> {
    let fixture = Fixture::new("fix1-trim-past-bounds")?;
    fixture.record_exchange()?;
    settle_to(&fixture, MAX_CONTINUATION_ROWS, "at")?;
    settled_step(&fixture, 1_000, 1, "over")?;
    let mut ordinary = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut ordinary)?;

    // The next input, after a compaction that did not fit ended the last.
    let next: MessageId = "msg_trim_past_bounds_next"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "carry on")?;
    let registry = super::super::compaction_recovery::Registry::default();
    registry.did_not_fit(&fixture.session);
    let targets = crate::dogfood::production::tests::projection_targets_with_window(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        1_000_000,
    )?;
    let first = targets.issue()?;
    let mut plan = crate::dogfood::production::ProductPlan::new(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        "carry on".to_owned(),
        grapt_provider::PromptCacheKey::for_session(&fixture.session)
            .map_err(|error| error.to_string())?,
        Some(first),
        targets,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        fixture
            .continuation(next)?
            .with_compaction_recovery(registry),
        StepNumber::new(1).ok_or("step")?,
    );
    plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(1),
    )
    .map_err(|refusal| format!("the trimmed retry compacted again: {}", refusal.detail))?;
    Ok(())
}
