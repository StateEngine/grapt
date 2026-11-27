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

/// Five 900 KiB external/MCP outputs in one exchange total 4.5 MiB raw,
/// exceeding the 4 MiB request ceiling, yet their projected inline previews
/// total <= 160 KiB. The pre-projection structural SQL guard must charge the
/// bounded preview size for previewed external tools so the exchange succeeds.
#[test]
fn five_900kib_mcp_outputs_in_one_exchange_succeed_under_preview_budget() -> Result<(), String> {
    const OUTPUT_BYTES: usize = 900 * 1024;
    let fixture = Fixture::new("fix1-five-mcp-900k")?;
    fixture.record_exchange()?;
    let payload = "m".repeat(OUTPUT_BYTES);
    let mut owner = step_owner(&fixture, 2)?;
    for index in 0..5 {
        let call = ToolCallId::new(format!("mcp-900k-{index}")).ok_or("call")?;
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ToolCalled {
                    call: call.clone(),
                    tool: "mcp_demo_search".to_owned(),
                    input: "{}".to_owned(),
                    execution: ToolExecution::Local,
                    state: AdapterState::none(),
                }),
            })
            .map_err(|error| error.to_string())?;
        let captured = owner
            .capture_hosted_output(&payload)
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

    let continuation = fixture.continuation(fixture.input.clone())?;
    let items = continuation
        .request_input("use the tool", true)
        .map_err(|error| format!("five 900 KiB MCP outputs were refused: {error}"))?;
    let mcp_outputs: Vec<&CodexToolText> = items
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ToolOutput { call, output, .. }
                if call.as_str().starts_with("mcp-900k-") =>
            {
                Some(output)
            }
            _ => None,
        })
        .collect();
    assert_eq!(mcp_outputs.len(), 5);
    for output in mcp_outputs {
        assert!(output.as_str().len() <= 32 * 1024);
    }
    Ok(())
}

/// Appending a 1 KiB hook note onto an accepted 1,048,064-byte MCP result
fn capture_secret_mcp_artifact(
    artifacts: &FilesystemArtifactStore,
    bytes: &[u8],
) -> Result<ToolTerminalArtifact, String> {
    let bound = CaptureBound::new(grapt_runtime::TOOL_ARTIFACT_MAX_BYTES, 64 * 1024)
        .ok_or("capture bound")?;
    let captured = artifacts
        .capture(
            Cursor::new(bytes),
            bound,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|error| error.to_string())?;
    assert!(captured.disposition().is_complete());
    let read_budget =
        grapt_artifact::ReadBudget::new(grapt_runtime::TOOL_ARTIFACT_MAX_BYTES).ok_or("budget")?;
    let fetched = artifacts
        .fetch(captured.reference(), 0, read_budget)
        .map_err(|error| error.to_string())?;
    assert_eq!(fetched.bytes(), bytes);
    ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        .ok_or_else(|| "terminal artifact".to_owned())
}

fn commit_mcp_call(
    owner: &mut impl grapt_runtime::AttemptOwner,
    call: &ToolCallId,
    outcome: ToolTerminalOutcome,
) -> Result<(), String> {
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: "mcp_demo_search".to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::ToolTerminal {
            call: call.clone(),
            outcome,
        })
        .map_err(|error| error.to_string())
}

fn read_all_public_history_pages(
    fixture: &Fixture,
    call: &ToolCallId,
) -> Result<(String, usize), String> {
    let inbox = fixture.inbox()?;
    let artifacts = fixture.artifacts()?;
    let mut offset = Some(0u64);
    let mut collected = String::new();
    let mut pages = 0usize;
    let mut expected_total = None;
    while let Some(current) = offset {
        let raw = history(
            inbox,
            artifacts,
            &fixture.session,
            Some(call),
            None,
            Some(current),
        )?;
        let page: serde_json::Value =
            serde_json::from_slice(&raw).map_err(|error| error.to_string())?;
        let total = page["totalBytes"]
            .as_u64()
            .ok_or("history page missing totalBytes")?;
        if let Some(prev) = expected_total {
            assert_eq!(prev, total);
        } else {
            expected_total = Some(total);
        }
        let content = page["content"]
            .as_str()
            .ok_or("history page missing content")?;
        collected.push_str(content);
        pages += 1;
        offset = page["nextOffset"].as_u64();
    }
    assert_eq!(
        Some(collected.len() as u64),
        expected_total,
        "paged public history length must equal totalBytes"
    );
    Ok((collected, pages))
}

/// Appending a 1 KiB hook note onto an accepted 1,048,064-byte MCP result
/// (both success and refusal) must preserve EVERY accepted MCP byte intact in
/// the complete durable artifact, read cleanly across offset pages via the
/// real public `history` tool, AND replay cleanly with status prefixes
/// projected under 32 KiB.
#[test]
fn near_cap_mcp_output_with_hook_note_survives_history_and_replay() -> Result<(), String> {
    let fixture = Fixture::new("fix1-mcp-hook-near-cap")?;
    fixture.record_exchange()?;
    let raw_success = format!("MCP_OK_HEAD_{}_MCP_OK_TAIL", "m".repeat(1_048_040));
    let raw_refusal = format!("MCP_ERR_HEAD_{}_MCP_ERR_TAIL", "e".repeat(1_048_038));
    assert_eq!(
        (raw_success.len(), raw_refusal.len()),
        (1_048_064, 1_048_064)
    );
    let hook_note = format!("HOOK_MARKER_{}", "h".repeat(1012));

    let annotated_success = crate::dogfood::tools::annotate_success_output(
        raw_success.as_bytes().to_vec(),
        std::slice::from_ref(&hook_note),
        false,
    )?;
    let annotated_refusal = crate::dogfood::tools::annotate_failure_detail(
        raw_refusal.clone(),
        std::slice::from_ref(&hook_note),
    );
    assert!(annotated_success.starts_with(raw_success.as_bytes()));
    assert!(annotated_refusal.starts_with(&raw_refusal));
    assert!(annotated_success.len() > 1_048_576 && annotated_refusal.len() > 1_048_576);

    let artifacts = fixture.artifacts()?;
    let ok_artifact = capture_secret_mcp_artifact(artifacts, &annotated_success)?;
    let err_artifact = capture_secret_mcp_artifact(artifacts, annotated_refusal.as_bytes())?;

    let mut owner = step_owner(&fixture, 2)?;
    let ok_call = ToolCallId::new("mcp-near-cap-ok").ok_or("ok call")?;
    let err_call = ToolCallId::new("mcp-near-cap-err").ok_or("err call")?;
    commit_mcp_call(
        &mut owner,
        &ok_call,
        ToolTerminalOutcome::Completed {
            output: Some(ok_artifact),
        },
    )?;
    commit_mcp_call(
        &mut owner,
        &err_call,
        ToolTerminalOutcome::Failed {
            reason: grapt_types::ToolFailureReason::EffectFailed,
            detail: Some(err_artifact),
        },
    )?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;

    let (paged_ok, ok_pages) = read_all_public_history_pages(&fixture, &ok_call)?;
    assert!(
        ok_pages > 1,
        "expected multiple 64 KiB public history pages"
    );
    assert_eq!(paged_ok.as_bytes(), annotated_success.as_slice());

    let (paged_err, err_pages) = read_all_public_history_pages(&fixture, &err_call)?;
    assert!(
        err_pages > 1,
        "expected multiple 64 KiB public history pages"
    );
    assert!(paged_err.starts_with("Tool failed: EffectFailed\n"));
    assert!(paged_err.ends_with(&annotated_refusal));

    let items = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)
        .map_err(|error| format!("replay refused near-cap MCP outputs: {error}"))?;
    for (expected_call, expected_error) in [(&ok_call, false), (&err_call, true)] {
        let projected = items
            .iter()
            .find_map(|item| match item {
                CodexInputItem::ToolOutput {
                    call,
                    output,
                    is_error,
                } if call == expected_call => Some((output, *is_error)),
                _ => None,
            })
            .ok_or("replayed MCP output missing")?;
        assert_eq!(projected.1, expected_error);
        assert!(projected.0.as_str().len() <= 32 * 1024);
    }
    Ok(())
}

/// A 1,000,003-byte JSON-expanding or quote-heavy MCP result plus a 2-byte
/// hook note must not expand via JSON re-serialization past
/// `TOOL_ARTIFACT_MAX_BYTES`, and must read cleanly through public `history`.
#[test]
fn json_expanding_mcp_output_with_hook_note_fits_cap_and_reads_via_history() -> Result<(), String> {
    let fixture = Fixture::new("fix2-mcp-json-expand")?;
    fixture.record_exchange()?;
    let expanding_json = format!("[{}1]", "1e9,".repeat(250_000));
    let quote_heavy = format!("\"{}\"", "\\\"".repeat(500_000) + "x");
    assert_eq!(expanding_json.len(), 1_000_003);
    assert_eq!(quote_heavy.len(), 1_000_003);
    let note = "ok".to_owned();

    let annotated_expanding = crate::dogfood::tools::annotate_success_output(
        expanding_json.as_bytes().to_vec(),
        std::slice::from_ref(&note),
        false,
    )?;
    assert!(
        annotated_expanding.len() <= grapt_runtime::TOOL_ARTIFACT_MAX_BYTES as usize,
        "expanding JSON MCP result exceeded TOOL_ARTIFACT_MAX_BYTES: {}",
        annotated_expanding.len()
    );
    assert!(annotated_expanding.starts_with(expanding_json.as_bytes()));

    let annotated_quotes = crate::dogfood::tools::annotate_success_output(
        quote_heavy.as_bytes().to_vec(),
        std::slice::from_ref(&note),
        false,
    )?;
    assert!(
        annotated_quotes.len() <= grapt_runtime::TOOL_ARTIFACT_MAX_BYTES as usize,
        "quote-heavy MCP result exceeded TOOL_ARTIFACT_MAX_BYTES: {}",
        annotated_quotes.len()
    );

    let artifacts = fixture.artifacts()?;
    let exp_artifact = capture_secret_mcp_artifact(artifacts, &annotated_expanding)?;
    let quote_artifact = capture_secret_mcp_artifact(artifacts, &annotated_quotes)?;

    let mut owner = step_owner(&fixture, 2)?;
    let exp_call = ToolCallId::new("mcp-json-expand").ok_or("exp call")?;
    let quote_call = ToolCallId::new("mcp-quote-heavy").ok_or("quote call")?;
    commit_mcp_call(
        &mut owner,
        &exp_call,
        ToolTerminalOutcome::Completed {
            output: Some(exp_artifact),
        },
    )?;
    commit_mcp_call(
        &mut owner,
        &quote_call,
        ToolTerminalOutcome::Completed {
            output: Some(quote_artifact),
        },
    )?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;

    let (paged_exp, exp_pages) = read_all_public_history_pages(&fixture, &exp_call)?;
    assert!(exp_pages > 1);
    assert_eq!(paged_exp.as_bytes(), annotated_expanding.as_slice());

    let (paged_quote, quote_pages) = read_all_public_history_pages(&fixture, &quote_call)?;
    assert!(quote_pages > 1);
    assert_eq!(paged_quote.as_bytes(), annotated_quotes.as_slice());
    Ok(())
}

/// 120 × 900 KiB (~105 MiB raw) external/MCP outputs stay bounded in projected
/// request context (<= 32 KiB each, <= 4 MiB total) because each previewed
/// terminal is projected immediately at read time rather than aggregated raw.
#[test]
fn one_hundred_twenty_900kib_mcp_outputs_stay_bounded_in_projected_request() -> Result<(), String> {
    const OUTPUT_BYTES: usize = 900 * 1024;
    const COUNT: usize = 120;
    let fixture = Fixture::new("fix1-120-mcp-900k")?;
    fixture.record_exchange()?;
    let payload = "m".repeat(OUTPUT_BYTES);
    let shared_artifact = capture_secret_mcp_artifact(fixture.artifacts()?, payload.as_bytes())?;

    let mut remaining = COUNT;
    let mut step = 2u32;
    let mut index = 0usize;
    while remaining > 0 {
        let batch = remaining.min(30);
        let mut owner = step_owner(&fixture, step)?;
        for _ in 0..batch {
            let call = ToolCallId::new(format!("mcp-120-{index:03}")).ok_or("call")?;
            commit_mcp_call(
                &mut owner,
                &call,
                ToolTerminalOutcome::Completed {
                    output: Some(shared_artifact.clone()),
                },
            )?;
            index += 1;
        }
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
        remaining -= batch;
        step += 1;
    }

    let inbox = lock(fixture.inbox()?);
    let artifacts = fixture.artifacts()?;
    let calls = stored_calls(inbox.store(), &fixture.session, artifacts, None, None)?;
    let projected = stored_outputs_projected(
        inbox.store(),
        &fixture.session,
        artifacts,
        None,
        None,
        &calls,
    )?;
    drop(inbox);
    let mcp_projected: Vec<&StoredOutput> = projected
        .iter()
        .filter(|entry| entry.call.as_str().starts_with("mcp-120-"))
        .collect();
    assert_eq!(mcp_projected.len(), COUNT);
    let total_held: usize = mcp_projected
        .iter()
        .map(|entry| entry.output.as_str().len())
        .sum();
    assert!(total_held <= MAX_CONTEXT_BYTES);
    assert!(mcp_projected
        .iter()
        .all(|e| e.output.as_str().len() <= 32 * 1024));

    let items = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)
        .map_err(|error| format!("120 × 900 KiB MCP outputs refused: {error}"))?;
    let count = items
        .iter()
        .filter(|item| {
            matches!(
                item,
                CodexInputItem::ToolOutput { call, output, .. }
                    if call.as_str().starts_with("mcp-120-") && output.as_str().len() <= 32 * 1024
            )
        })
        .count();
    assert_eq!(count, COUNT);
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
