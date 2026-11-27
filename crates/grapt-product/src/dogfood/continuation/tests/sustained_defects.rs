//! SUSTAINED-WORK-REPAIR-01 J01: the audited defects, pinned as accepted base
//! `20909c94` actually behaves.
//!
//! These are reproduction receipts, not the repaired contract. Each assertion
//! states the defective outcome through the ordinary continuation owner; the
//! repair that owns it replaces the test with its positive witness and an exact
//! resurrection mutant.

use super::*;
mod usage_threshold;
use grapt_runtime::AttemptPlan;
use grapt_types::{CompactionId, DeterministicAuthorities, ExecutionId, TokenCount, UsageField};

pub(super) fn product_plan(
    fixture: &Fixture,
    input: MessageId,
    prompt: &str,
) -> Result<crate::dogfood::production::ProductPlan, String> {
    let targets = crate::dogfood::production::tests::projection_targets(
        grapt_provider::ProtocolFamily::ChatGptCodex,
    )?;
    let first = targets.issue()?;
    Ok(crate::dogfood::production::ProductPlan::new(
        grapt_provider::ProtocolFamily::ChatGptCodex,
        prompt.to_owned(),
        grapt_provider::PromptCacheKey::for_session(&fixture.session)
            .map_err(|error| error.to_string())?,
        Some(first),
        targets,
        Arc::new(crate::dogfood::mcp::PublishedTools::default()),
        fixture.continuation(input)?,
        StepNumber::new(1).ok_or("step")?,
    ))
}

pub(super) fn assert_structural_refusal(
    plan: &mut crate::dogfood::production::ProductPlan,
) -> Result<(), String> {
    let refusal = plan
        .plan(
            StepNumber::new(1).ok_or("step")?,
            1,
            grapt_runtime::HistoryGeneration::new(1),
        )
        .err()
        .ok_or("ordinary product plan ignored the structural bound")?;
    assert_eq!(
        refusal.compaction,
        Some(grapt_types::CompactionTrigger::StructuralBound),
        "ordinary product plan lost the structural compaction trigger"
    );
    Ok(())
}

pub(super) fn assert_plan_recovers(
    plan: &mut crate::dogfood::production::ProductPlan,
) -> Result<(), String> {
    plan.plan(
        StepNumber::new(1).ok_or("step")?,
        1,
        grapt_runtime::HistoryGeneration::new(2),
    )
    .map(|_| ())
    .map_err(|refusal| format!("ordinary product plan did not recover: {}", refusal.detail))
}

pub(super) fn step_owner(
    fixture: &Fixture,
    step: u32,
) -> Result<DurableAttemptOwner<'static, FilesystemArtifactStore, NoLiveSubscribers>, String> {
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
            attempt: PhysicalAttemptId::new(format!("swr-{step}")).ok_or("attempt")?,
        })
        .map_err(|e| e.to_string())?;
    Ok(owner)
}

/// One ordinary settled Step of `calls` tiny successful tool exchanges.
pub(super) fn settled_step(
    fixture: &Fixture,
    step: u32,
    calls: usize,
    label: &str,
) -> Result<(), String> {
    let mut owner = step_owner(fixture, step)?;
    for index in 0..calls {
        let call = ToolCallId::new(format!("{label}-{step}-{index}")).ok_or("call")?;
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
            .map_err(|e| e.to_string())?;
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

pub(super) fn head(fixture: &Fixture) -> Result<u64, String> {
    let head: i64 = lock(fixture.inbox()?)
        .store()
        .connection()
        .query_row(
            "SELECT MAX(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
            [fixture.session.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    u64::try_from(head).map_err(|e| e.to_string())
}

fn last_assistant_opened(fixture: &Fixture) -> Result<u64, String> {
    let cut: i64 = lock(fixture.inbox()?)
        .store()
        .connection()
        .query_row(
            "SELECT MAX(seq) FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND type IN ('session.assistant-message.opened.1',
                            'session.assistant-message.opened.2')",
            [fixture.session.as_str()],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    u64::try_from(cut).map_err(|e| e.to_string())
}

pub(super) fn stage_cut(fixture: &Fixture, label: &str, from: u64) -> Result<(), String> {
    let source_head = head(fixture)?;
    if from > source_head {
        return Err("fixture cut exceeds its source head".to_owned());
    }
    let id = CompactionId::new(label).ok_or("compaction")?;
    let selected = super::super::summarization::SummaryPreparation {
        text: String::new(),
        recent: grapt_api::u3a::Utf16Text::empty(),
        prefix: Vec::new(),
        retained: Vec::new(),
        removed_bytes: 100,
        retained_bytes: 0,
        quote: None,
        previous_generation: None,
        source_epoch: grapt_types::ContextEpoch::FIRST,
        selection: super::super::retained::Selection {
            session: fixture.session.clone(),
            input: fixture.input.clone(),
            compaction: id.clone(),
            from,
            source_head,
            admission: None,
            server: None,
            summary_plan: None,
        },
    };
    super::summary_tests::commit_selection(fixture, &selected, id)
}

/// A committed whole-history cut at the current head, through the descriptor owner.
fn stage_cut_at_head(fixture: &Fixture, label: &str) -> Result<(), String> {
    let head = head(fixture)?;
    stage_cut(fixture, label, head)
}

/// The fixture rebound to a created Session: execution observations belong only
/// to a Session the store knows.
fn seeded(label: &str) -> Result<Fixture, String> {
    let mut fixture = Fixture::new(label)?;
    let ids = CountingIds(AtomicU64::new(0x7000_0000));
    let clock = FixedClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_swr_seeded").ok_or("project")?,
        "/work/swr-seeded",
        "swr-seeded",
    )
    .map_err(|e| e.to_string())?;
    let created = {
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::create_session(inbox.store_mut(), &authorities, &seed, &mut [])
            .map_err(|e| e.to_string())?
            .session
    };
    fixture.session = created;
    Ok(fixture)
}

/// Valid, ordinary session rows the transcript fold does not own: each pair is
/// one started and one succeeded execution observation.
fn execution_rows(fixture: &Fixture, pairs: usize, base: u64) -> Result<(), String> {
    let ids = CountingIds(AtomicU64::new(base));
    let clock = FixedClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut inbox = lock(fixture.inbox()?);
    if pairs == 0 {
        return Ok(());
    }
    // Exercise the observation owner once, then plant the same valid lifecycle
    // shape in one governed fixture batch. Revalidating the whole execution
    // prefix for all 25,000 pairs made setup quadratic; compaction is under test.
    let first = ExecutionId::new(format!("exe_swr_{base:x}_0")).ok_or("execution")?;
    grapt_session::execution::begin(inbox.store_mut(), &authorities, &fixture.session, &first)?;
    grapt_session::execution::settle(
        inbox.store_mut(),
        &authorities,
        &fixture.session,
        &first,
        grapt_session::execution::Outcome::Succeeded,
    )?;
    let aggregate = AggregateId::Session(fixture.session.clone());
    let writer = inbox
        .store()
        .fencing_token(&aggregate)
        .map_err(|e| e.to_string())?;
    let tx = inbox
        .store_mut()
        .governed_transaction()
        .map_err(|e| e.to_string())?;
    for index in 1..pairs {
        let execution = ExecutionId::new(format!("exe_swr_{base:x}_{index}")).ok_or("execution")?;
        let data = serde_json::json!({"sessionID": fixture.session.as_str(), "executionID": execution.as_str()});
        for kind in ["session.execution.started", "session.execution.succeeded"] {
            let event = format!("evt_{}", authorities.mint().as_str())
                .parse::<EventId>()
                .map_err(|e| e.to_string())?;
            let appended = EventStore::append_within(
                &tx,
                grapt_store::AppendInput {
                    id: &event,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: kind,
                    version: grapt_types::EventVersion::new(1).ok_or("version")?,
                    created_ms: authorities.now().as_millis(),
                    data: &data,
                },
                &mut [],
            )
            .map_err(|e| e.to_string())?;
            if !matches!(appended, grapt_store::Appended::Committed { .. }) {
                return Err("fixture event was not committed".into());
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?.discard();
    let last = ExecutionId::new(format!("exe_swr_{base:x}_{}", pairs - 1)).ok_or("execution")?;
    let read = grapt_session::execution::read(inbox.store().connection(), &fixture.session, &last)?;
    assert_eq!(read.len(), 2);
    assert_eq!(
        read[1].outcome,
        Some(grapt_session::execution::Outcome::Succeeded)
    );
    Ok(())
}

pub(super) fn append_malformed_session_event(
    fixture: &Fixture,
    event: &str,
    kind: &'static str,
    version: u32,
) -> Result<(), String> {
    let event = event
        .parse::<EventId>()
        .map_err(|error| error.to_string())?;
    let version = grapt_types::EventVersion::new(version).ok_or("event version")?;
    let aggregate = AggregateId::Session(fixture.session.clone());
    let mut inbox = lock(fixture.inbox()?);
    let writer = inbox
        .store()
        .fencing_token(&aggregate)
        .map_err(|error| error.to_string())?;
    let tx = inbox
        .store_mut()
        .governed_transaction()
        .map_err(|error| error.to_string())?;
    let appended = EventStore::append_within(
        &tx,
        grapt_store::AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: kind,
            version,
            created_ms: FixedClock.now().as_millis(),
            data: &serde_json::json!({}),
        },
        &mut [],
    )
    .map_err(|error| error.to_string())?;
    if !matches!(appended, grapt_store::Appended::Committed { .. }) {
        return Err("malformed event fixture repeated its identity".to_owned());
    }
    tx.commit().map_err(|error| error.to_string())?.discard();
    Ok(())
}

fn usage_step(fixture: &Fixture, step: u32, input: u64) -> Result<PhysicalAttemptId, String> {
    usage_step_output(fixture, step, input, 0)
}

fn usage_step_output(
    fixture: &Fixture,
    step: u32,
    input: u64,
    output: u64,
) -> Result<PhysicalAttemptId, String> {
    let attempt = PhysicalAttemptId::new(format!("swr-{step}")).ok_or("attempt")?;
    let mut owner = step_owner(fixture, step)?;
    let usage = grapt_provider::TokenUsage::new(
        UsageField::reported(TokenCount::new(input).map_err(|error| error.to_string())?),
        UsageField::ABSENT,
        UsageField::reported(TokenCount::new(output).map_err(|error| error.to_string())?),
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
        UsageField::ABSENT,
    );
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                usage: Some(usage),
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())?;
    Ok(attempt)
}

/// Steps of 32 settled calls that fill the continuation page bound exactly.
const STEPS_OF_32: u32 = (MAX_CONTINUATION_ROWS / 32) as u32;

#[test]
fn swr_c1_tiny_settled_calls_compact_before_the_page_bound() -> Result<(), String> {
    let fixture = Fixture::new("swr-c1-calls")?;
    fixture.record_exchange()?;
    // The recorded exchange's call plus these: one past the page bound.
    for step in 2..=STEPS_OF_32 + 1 {
        settled_step(&fixture, step, 32, "tiny")?;
    }
    let continuation = fixture.continuation(fixture.input.clone())?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)?;
    let error = continuation
        .request_input("use the tool", true)
        .err()
        .ok_or("tiny settled calls past the page bound produced a request")?;
    assert!(
        error.contains("tool continuation history is unreadable"),
        "{error}"
    );
    assert!(error.contains("durable tool-call rows"), "{error}");
    let id = CompactionId::new("swr-c1-calls").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 1_000_000)?;
    assert!(selected.selection.from < selected.selection.source_head);
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    assert_plan_recovers(&mut plan)?;
    assert!(continuation.request_input("use the tool", false).is_ok());
    Ok(())
}

#[test]
fn swr_c1_promoted_inputs_compact_before_the_page_bound() -> Result<(), String> {
    let fixture = Fixture::new("swr-c1-users")?;
    for index in 0..=MAX_CONTINUATION_ROWS {
        let input = format!("msg_swr_user_{index:03}")
            .parse::<MessageId>()
            .map_err(|e| e.to_string())?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &input,
            &format!("prompt {index}"),
        )?;
    }
    let last = format!("msg_swr_user_{MAX_CONTINUATION_ROWS:03}")
        .parse::<MessageId>()
        .map_err(|e| e.to_string())?;
    let prompt = format!("prompt {MAX_CONTINUATION_ROWS}");
    settled_step(&fixture, 1, 1, "user-window")?;
    let error = fixture
        .continuation(last.clone())?
        .request_input(&prompt, false)
        .err()
        .ok_or("promoted inputs past the page bound produced a request")?;
    assert!(
        error.contains("conversation user history is unreadable"),
        "{error}"
    );
    let continuation = fixture.continuation(last)?;
    let mut plan = product_plan(&fixture, continuation.input.clone(), &prompt)?;
    assert_structural_refusal(&mut plan)?;
    let id = CompactionId::new("swr-c1-users").ok_or("compaction")?;
    let selected = continuation.prepare_summary(&prompt, &id, 1_000_000)?;
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    assert_plan_recovers(&mut plan)?;
    let rebuilt = continuation.request_input(&prompt, false)?;
    assert!(rebuilt.iter().any(|item| matches!(item,
        CodexInputItem::Message(message)
            if message.role == MessageRole::User && message.content == prompt
    )));
    Ok(())
}

#[test]
fn swr_c2_a_tight_cut_suffix_advances_through_another_compaction() -> Result<(), String> {
    let fixture = Fixture::new("swr-c2")?;
    fixture.record_exchange()?;
    let retained_cut = last_assistant_opened(&fixture)?;
    stage_cut(&fixture, "swr-c2-cut", retained_cut)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    assert!(continuation.request_input("use the tool", false).is_ok());
    let no_progress = CompactionId::new("swr-c2-no-progress").ok_or("compaction")?;
    let error = continuation
        .prepare_summary("use the tool", &no_progress, 1_000_000)
        .err()
        .ok_or("compactor republished the retained cut without progress")?;
    assert!(
        matches!(
            &error,
            super::super::summarization::PreparationError::NoProgress(_)
        ) && error
            .to_string()
            .contains("no complete history prefix fits the summary budget"),
        "{error}"
    );
    for step in 2..=STEPS_OF_32 + 1 {
        settled_step(&fixture, step, 32, "suffix")?;
    }
    settled_step(&fixture, STEPS_OF_32 + 2, 1, "suffix")?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)?;
    let error = continuation
        .request_input("use the tool", false)
        .err()
        .ok_or("retained suffix calls past the page bound produced a request")?;
    assert!(
        error.contains("tool continuation history is unreadable"),
        "{error}"
    );
    let id = CompactionId::new("swr-c2-next").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 1_000_000)?;
    assert!(selected.selection.from > 0);
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    assert_plan_recovers(&mut plan)?;
    assert!(continuation.request_input("use the tool", false).is_ok());
    Ok(())
}

#[test]
fn swr_c3_a_retained_suffix_over_the_event_bound_compacts_a_bounded_window() -> Result<(), String> {
    let fixture = seeded("swr-c3")?;
    fixture.record_exchange()?;
    stage_cut_at_head(&fixture, "swr-c3-cut")?;
    // Two events per pair, one fifth before the settled step: past the bound.
    let pairs = grapt_session::MAX_HISTORY_ROWS / 2 + 1;
    execution_rows(&fixture, pairs / 5, 0x4000_0000)?;
    settled_step(&fixture, 2, 1, "event-window")?;
    execution_rows(&fixture, pairs - pairs / 5, 0x4100_0000)?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_structural_refusal(&mut plan)?;
    let error = continuation
        .request_input("use the tool", false)
        .err()
        .ok_or("a retained suffix past the event bound produced a request")?;
    assert!(
        error.contains("conversation assistant history is unreadable"),
        "{error}"
    );
    assert!(error.contains("durable rows"), "{error}");
    let id = CompactionId::new("swr-c3-next").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 1_000_000)?;
    assert!(selected.selection.from < selected.selection.source_head);
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    assert_plan_recovers(&mut plan)?;
    assert!(continuation.request_input("use the tool", false).is_ok());
    Ok(())
}

#[test]
fn swr_c4_prior_attempt_usage_ignores_unrelated_session_lifetime() -> Result<(), String> {
    let fixture = seeded("swr-c4")?;
    fixture.record_exchange()?;
    execution_rows(&fixture, 2_501, 0x5000_0000)?;
    let attempt = usage_step(&fixture, 2, 41)?;
    let foreign = usage_step(&fixture, 3, 99)?;
    stage_cut_at_head(&fixture, "swr-c4-cut")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    assert!(
        continuation.request_input("use the tool", false).is_ok(),
        "the small retained suffix must itself be readable"
    );
    let mut plan = product_plan(&fixture, fixture.input.clone(), "use the tool")?;
    assert_plan_recovers(&mut plan)?;
    assert_eq!(
        continuation
            .confirmed_usage(&attempt)
            .map(|usage| usage.input),
        Ok(Some(41))
    );
    assert_eq!(
        continuation
            .confirmed_usage(&foreign)
            .map(|usage| usage.input),
        Ok(Some(99))
    );
    append_malformed_session_event(
        &fixture,
        "evt_swr_malformed_usage",
        "session.usage.observed",
        2,
    )?;
    // The malformed row is a named gap in the read, not a refusal that loses
    // the attempt's own measured usage: absence of a count is not a failure.
    let confirmed = continuation
        .confirmed_usage(&attempt)
        .map_err(|error| format!("a malformed sibling row hid this attempt: {error:?}"))?;
    assert_eq!(confirmed.input, Some(41));
    Ok(())
}

#[test]
fn swr_d3_compaction_keeps_the_newest_complete_exchange_exact() -> Result<(), String> {
    let fixture = Fixture::new("swr-tail")?;
    fixture.record_exchange()?;
    settled_step(&fixture, 2, 1, "working-tail")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let id = CompactionId::new("swr-tail").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 128_000)?;
    assert!(
        selected.selection.from < selected.selection.source_head,
        "the working-tail cut must precede the newest settled exchange"
    );
    assert!(
        selected.text.contains("fixture output"),
        "the older exchange should move into the concise summary input"
    );
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let rebuilt = continuation.request_input("use the tool", false)?;
    assert!(rebuilt.iter().any(|item| matches!(item,
        CodexInputItem::ToolCall { call, .. } if call.as_str() == "working-tail-2-0"
    )));
    assert!(rebuilt.iter().any(|item| matches!(item,
        CodexInputItem::ToolOutput { call, output, .. }
            if call.as_str() == "working-tail-2-0" && output.as_str().is_empty()
    )));
    Ok(())
}

#[test]
fn swr_t1_legacy_failure_without_detail_does_not_invent_one() -> Result<(), String> {
    let fixture = Fixture::new("swr-early")?;
    fixture.record_exchange_state(true, true)?;
    let request = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", false)?;
    let text: Vec<String> = request
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::Message(message) => Some(message.content.clone()),
            CodexInputItem::ToolOutput { output, .. } => Some(output.as_str().to_owned()),
            _ => None,
        })
        .filter(|text| text.contains("Tool failed: EffectFailed"))
        .collect();
    assert_eq!(text.len(), 1, "{request:?}");
    assert!(
        !text[0].contains("Tool failed: EffectFailed\n"),
        "an early failure carried detail: {}",
        text[0]
    );
    Ok(())
}

#[test]
fn swr_t2_legacy_non_utf8_tool_output_has_an_explicit_projection_marker() -> Result<(), String> {
    let fixture = Fixture::new("swr-utf8")?;
    fixture.record_exchange_output(true, false, false, READ_TOOL, "read", &[b'a', 0xC3])?;
    let request = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    let output = request
        .iter()
        .find_map(|item| match item {
            CodexInputItem::ToolOutput { output, .. } => Some(output.as_str()),
            _ => None,
        })
        .ok_or("tool output projection is absent")?;
    assert!(output.starts_with("a�"), "{output}");
    assert!(
        output.ends_with("[tool output projection: invalid UTF-8 bytes were replaced explicitly]")
    );
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn swr_d1_history_lists_with_a_cursor_and_refuses_foreign_calls() -> Result<(), String> {
    let fixture = Fixture::new("swr-history-list")?;
    fixture.record_exchange()?;
    settled_step(&fixture, 2, 32, "history-list")?;
    let ids = CountingIds(AtomicU64::new(0x7200_0000));
    let clock = FixedClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let other_session = {
        let seed = grapt_session::SessionSeed::new(
            grapt_types::ProjectId::new("prj_swr_history_other").ok_or("project")?,
            "/work/swr-history-other",
            "swr-history-other",
        )
        .map_err(|error| error.to_string())?;
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::create_session(inbox.store_mut(), &authorities, &seed, &mut [])
            .map_err(|error| format!("other session creation: {error}"))?
            .session
    };
    let foreign = ToolCallId::new("call_from_another_session").ok_or("call")?;
    let mut other = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(0x7300_0000))),
        NoLiveSubscribers,
        other_session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| format!("other owner election: {error}"))?;
    other
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("swr-history-other").ok_or("attempt")?,
        })
        .map_err(|error| format!("other attempt start: {error}"))?;
    other
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: foreign.clone(),
                tool: READ_TOOL.to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| format!("other tool call: {error}"))?;
    other
        .commit(DurableFact::ToolTerminal {
            call: foreign.clone(),
            outcome: ToolTerminalOutcome::Completed { output: None },
        })
        .map_err(|error| format!("other tool terminal: {error}"))?;
    other
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| format!("other step terminal: {error}"))?;
    drop(other);
    let first: serde_json::Value = serde_json::from_slice(&history(
        fixture.inbox()?,
        fixture.artifacts()?.as_ref(),
        &fixture.session,
        None,
        None,
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(first["calls"].as_array().map(Vec::len), Some(32));
    let cursor = first["nextCursor"].as_u64().ok_or("next cursor")?;
    let second: serde_json::Value = serde_json::from_slice(&history(
        fixture.inbox()?,
        fixture.artifacts()?.as_ref(),
        &fixture.session,
        None,
        Some(cursor),
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(second["calls"].as_array().map(Vec::len), Some(1));
    assert!(second["nextCursor"].is_null());
    let foreign_visible: serde_json::Value = serde_json::from_slice(&history(
        fixture.inbox()?,
        fixture.artifacts()?.as_ref(),
        &other_session,
        Some(&foreign),
        None,
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(foreign_visible["call"], foreign.as_str());
    let continuation = fixture.continuation(fixture.input.clone())?;
    let refusal = history(
        &continuation.inbox,
        continuation.artifacts.as_ref(),
        &continuation.session,
        Some(&foreign),
        None,
        Some(0),
    )
    .err()
    .ok_or("foreign call was accepted")?;
    // A call this session never recorded is told how to find a real one.
    assert_eq!(
        refusal,
        format!("no settled call {foreign} in this session; list calls with {{}} to find one")
    );
    Ok(())
}

#[test]
fn swr_d4_history_shows_a_corrupt_output_as_unreadable_after_restart() -> Result<(), String> {
    let mut fixture = Fixture::new("swr-history-corrupt")?;
    let reference = fixture.record_exchange()?;
    let identity = reference.id().as_str();
    let prefix = identity.get(..2).ok_or("artifact identity had no shard")?;
    let object = fixture
        .artifact_root
        .join("objects")
        .join(prefix)
        .join(identity);
    fixture.close();
    std::fs::write(object, b"corrupt output").map_err(|error| error.to_string())?;
    let continuation = fixture.reopen(fixture.input.clone())?;
    let corrupt = ToolCallId::new("call_d2a_read").ok_or("call")?;
    let listed: serde_json::Value = serde_json::from_slice(&history(
        &continuation.inbox,
        continuation.artifacts.as_ref(),
        &continuation.session,
        None,
        None,
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(listed["calls"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        listed["calls"][0]["outputBytes"].as_u64(),
        Some(reference.length())
    );
    let fetched = history(
        &continuation.inbox,
        continuation.artifacts.as_ref(),
        &continuation.session,
        Some(&corrupt),
        None,
        None,
    )?;
    assert!(String::from_utf8_lossy(&fetched).contains("unavailable or corrupt"));
    Ok(())
}

#[test]
fn swr_d1_history_list_skips_a_malformed_terminal() -> Result<(), String> {
    let fixture = Fixture::new("swr-history-list-corrupt")?;
    fixture.record_exchange()?;
    append_malformed_session_event(
        &fixture,
        "evt_swr_malformed_terminal",
        "session.tool.terminal",
        1,
    )?;
    let changed = rusqlite::Connection::open(&fixture.database)
        .map_err(|error| error.to_string())?
        .execute(
            "UPDATE event SET data = ?2 WHERE id = ?1",
            ("evt_swr_malformed_terminal", "{"),
        )
        .map_err(|error| error.to_string())?;
    if changed != 1 {
        return Err("malformed terminal JSON fixture was not found".to_owned());
    }
    let listed: serde_json::Value = serde_json::from_slice(&history(
        fixture.inbox()?,
        fixture.artifacts()?.as_ref(),
        &fixture.session,
        None,
        None,
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(listed["calls"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[test]
fn swr_d2_history_pages_exact_model_visible_output_on_utf8_boundaries() -> Result<(), String> {
    let mut fixture = Fixture::new("swr-history-output")?;
    let output = format!("{}é-tail", "a".repeat(65_535));
    fixture.record_exchange_output(true, false, false, READ_TOOL, "read", output.as_bytes())?;
    let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
    let first: serde_json::Value = serde_json::from_slice(&history(
        fixture.inbox()?,
        fixture.artifacts()?.as_ref(),
        &fixture.session,
        Some(&call),
        None,
        Some(0),
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(first["content"].as_str().map(str::len), Some(65_535));
    assert_eq!(first["nextOffset"].as_u64(), Some(65_535));
    assert_eq!(
        first["totalBytes"].as_u64(),
        u64::try_from(output.len()).ok()
    );
    fixture.close();
    let continuation = fixture.reopen(fixture.input.clone())?;
    let second: serde_json::Value = serde_json::from_slice(&history(
        &continuation.inbox,
        continuation.artifacts.as_ref(),
        &continuation.session,
        Some(&call),
        None,
        Some(65_535),
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(second["content"].as_str(), Some("é-tail"));
    assert!(second["nextOffset"].is_null());
    assert!(history(
        &continuation.inbox,
        continuation.artifacts.as_ref(),
        &continuation.session,
        Some(&call),
        None,
        Some(65_536),
    )
    .err()
    .is_some_and(|error| error.contains("UTF-8 boundary")));
    Ok(())
}
