//! SC-REL-08: a structural compaction bound that lands inside an assistant
//! exchange. The 8-hour Windows soak failed this way at its fifth compaction:
//! the context-item bound fell on a read call's terminal, the summary window
//! kept the call and lost its output, and the Step failed with
//! "tool call_read_113 has no durable terminal output".

use super::*;
use grapt_types::CompactionId;

/// The continuation row bound, as the step and call counts these use.
const ROWS: u32 = MAX_CONTINUATION_ROWS as u32;

/// One settled Step of `texts` text blocks and then `calls` tiny read calls,
/// all called before any returns, as one response's parallel calls are.
fn exchange(fixture: &Fixture, step: u32, texts: u32, calls: u32) -> Result<(), String> {
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(u64::from(step) * 100_000))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|e| e.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("split-{step}")).ok_or("attempt")?,
        })
        .map_err(|e| e.to_string())?;
    let calls = (0..calls)
        .map(|index| ToolCallId::new(format!("call_split_{step}_{index}")).ok_or("call"))
        .collect::<Result<Vec<_>, _>>()?;
    let mut fragments = Vec::new();
    for index in 0..texts {
        let block = grapt_provider::BlockOrdinal::new(index);
        fragments.extend([
            AttemptFragment::TextOpened {
                block,
                state: AdapterState::none(),
            },
            AttemptFragment::TextDelta {
                block,
                text: format!("reading {step}"),
                state: AdapterState::none(),
            },
            AttemptFragment::TextClosed {
                block,
                state: AdapterState::none(),
            },
        ]);
    }
    fragments.extend(calls.iter().map(|call| AttemptFragment::ToolCalled {
        call: call.clone(),
        tool: READ_TOOL.to_owned(),
        input: "{}".to_owned(),
        execution: ToolExecution::Local,
        state: AdapterState::none(),
    }));
    for fragment in fragments {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|e| e.to_string())?;
    }
    for call in calls {
        owner
            .commit(DurableFact::ToolTerminal {
                call,
                outcome: ToolTerminalOutcome::Completed { output: None },
            })
            .map_err(|e| e.to_string())?;
    }
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|e| e.to_string())
}

/// The event type at `seq` and, for a tool terminal, its call.
fn event_at(fixture: &Fixture, seq: u64) -> Result<(String, Option<String>), String> {
    let seq = i64::try_from(seq).map_err(|e| e.to_string())?;
    lock(fixture.inbox()?)
        .store()
        .connection()
        .query_row(
            "SELECT type, json_extract(data, '$.call') FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1 AND seq = ?2",
            (fixture.session.as_str(), seq),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| e.to_string())
}

/// Steps 2..=ROWS+1 of one text block and one call each: four context items
/// apiece (the text, its response's first item, the call, its terminal).
fn spoken_calls(fixture: &Fixture) -> Result<(), String> {
    fixture.record_exchange()?;
    for step in 2..=ROWS + 1 {
        exchange(fixture, step, 1, 1)?;
    }
    Ok(())
}

#[test]
fn a_structural_bound_inside_an_exchange_summarizes_below_it() -> Result<(), String> {
    let fixture = Fixture::new("split-exchange")?;
    // One promoted input more puts the first context item past the item bound
    // (four per row) on a call's terminal, as in the soak, and makes the calls
    // the current input's.
    let input: MessageId = "msg_split_current"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    fixture.record_exchange()?;
    promote_test_input(fixture.inbox()?, &fixture.session, &input, "read on")?;
    for step in 2..=ROWS + 1 {
        exchange(&fixture, step, 1, 1)?;
    }
    let continuation = fixture.continuation(input)?;
    let bound = continuation
        .structural_admission()?
        .ok_or("the history did not reach a structural bound")?;
    assert_eq!(
        bound.limit, MAX_CONTEXT_ITEMS as u64,
        "the item bound must be the one reached"
    );
    let (kind, call) = event_at(&fixture, bound.seq.value())?;
    assert_eq!(kind, "session.tool.terminal.1");
    let call = call.ok_or("the terminal names no call")?;

    let id = CompactionId::new("split-exchange").ok_or("compaction")?;
    let selected = continuation.prepare_summary("read on", &id, 1_000_000)?;
    // The cut is below the split exchange, so its call and output both stay
    // in the exact retained history.
    assert!(selected.selection.from < bound.seq.value());
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let request = format!("{:?}", continuation.request_input("read on", false)?);
    assert!(
        request.matches(call.as_str()).count() >= 2,
        "{call} lost its call or its output: {request}"
    );
    Ok(())
}

#[test]
fn a_structural_bound_between_exchanges_is_unchanged() -> Result<(), String> {
    let fixture = Fixture::new("split-between")?;
    spoken_calls(&fixture)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let bound = continuation
        .structural_boundary()?
        .ok_or("the history did not reach a structural bound")?;
    // The bound is a later exchange's first item; every earlier call's
    // terminal precedes it.
    assert_eq!(
        event_at(&fixture, bound.value())?.0,
        "session.content.appended.1"
    );
    assert_eq!(continuation.summary_boundary()?, Some(bound));
    Ok(())
}

#[test]
fn one_split_exchange_with_nothing_before_it_refuses_without_an_empty_summary() -> Result<(), String>
{
    let fixture = Fixture::new("split-alone")?;
    // One call past the bound in one response: the call bound falls among
    // them, after a bound's worth of calls whose terminals all follow it.
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "use the tool",
    )?;
    exchange(&fixture, 1, 0, ROWS + 1)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let bound = continuation
        .structural_admission()?
        .ok_or("the history did not reach a structural bound")?;
    assert_eq!(bound.limit, MAX_CONTINUATION_ROWS as u64);
    let opening = continuation
        .summary_boundary()?
        .ok_or("the bound vanished")?;
    assert!(opening < bound.seq);
    let id = CompactionId::new("split-alone").ok_or("compaction")?;
    let refused = continuation
        .prepare_summary("use the tool", &id, 1_000_000)
        .err()
        .ok_or("a window holding only the current input produced a summary")?;
    // The established no-progress refusal: nothing is selected, so nothing
    // empty is summarized and admission does not compact the same window
    // again.
    assert!(
        matches!(
            &refused,
            super::super::summarization::PreparationError::NoProgress(_)
        ) && refused
            .to_string()
            .contains("no complete history prefix fits the summary budget"),
        "{refused}"
    );
    assert!(matches!(
        refused.into_failure(),
        grapt_runtime::CompactionFailure::NoProgress { .. }
    ));
    Ok(())
}
