//! Responses reasoning replay in the continuation: kept in place before its
//! call, bounded per request, and dropped once a summary covers it.
use super::*;
use grapt_event::session_message::AssistantSelection;
use grapt_provider::BlockOrdinal;

/// One Step whose reasoning items precede a failed `read` call.
fn reasoning_exchange(fixture: &Fixture, items: &[String]) -> Result<(), String> {
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "use the tool",
    )?;
    let selection = AssistantSelection::new(
        AgentId::new("rr-agent").ok_or("agent")?,
        grapt_types::ProviderId::new("rr-openai").ok_or("provider")?,
        grapt_types::ModelId::new("rr-model").ok_or("model")?,
        None,
    );
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?
    .with_selection(selection)
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_rr").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let call = ToolCallId::new("call_rr").ok_or("call")?;
    let mut fragments = Vec::new();
    for (index, item) in items.iter().enumerate() {
        let block = BlockOrdinal::new(u32::try_from(index).map_err(|error| error.to_string())?);
        fragments.extend([
            AttemptFragment::ReasoningOpened {
                block,
                state: AdapterState::none(),
            },
            AttemptFragment::ReasoningClosed {
                block,
                state: AdapterState::retain(item.as_str()).map_err(|error| error.to_string())?,
            },
        ]);
    }
    fragments.push(AttemptFragment::ToolCalled {
        call: call.clone(),
        tool: READ_TOOL.into(),
        input: r#"{"path":"fixture.txt"}"#.into(),
        execution: ToolExecution::Local,
        state: AdapterState::none(),
    });
    for fragment in fragments {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|error| error.to_string())?;
    }
    fixture.ask_permission("evt_rr", "per_rr", &call)?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::EffectFailed,
                detail: None,
            },
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

fn item(id: &str, encrypted: usize) -> String {
    serde_json::json!({
        "kind": "openai-reasoning-v1",
        "item": {"type": "reasoning", "id": id, "summary": [], "encrypted_content": "e".repeat(encrypted)},
        "binding": vec![0u8; 32],
    })
    .to_string()
}

fn reasoning(input: &[CodexInputItem]) -> Vec<&grapt_provider::ProviderReplay> {
    input
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ProviderReplay(replay) if replay.is_responses_reasoning() => {
                Some(replay)
            }
            _ => None,
        })
        .collect()
}

#[test]
fn rr_reasoning_replays_in_place_before_its_call() -> Result<(), String> {
    let fixture = Fixture::new("rr-in-place")?;
    reasoning_exchange(&fixture, &[item("rs_rr_0", 16)])?;
    let input = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    assert!(
        matches!(
            &input[..],
            [
                CodexInputItem::Message(_),
                CodexInputItem::AssistantResponseStart,
                CodexInputItem::ProviderReplay(replay),
                CodexInputItem::ToolCall { .. },
                CodexInputItem::ToolOutput { is_error: true, .. }
            ] if replay.is_responses_reasoning()
        ),
        "{input:?}"
    );
    Ok(())
}

#[test]
fn rr_reasoning_past_the_request_bound_is_not_replayed_and_the_oldest_stays() -> Result<(), String>
{
    let fixture = Fixture::new("rr-bound")?;
    // 60 KiB items: seventeen fit the 1 MiB bound, the eighteenth does not.
    let each = 60 * 1024;
    let fit = MAX_REASONING_REPLAY_BYTES / (each + "rs_rr_00".len());
    let items: Vec<String> = (0..=fit)
        .map(|index| item(&format!("rs_rr_{index:02}"), each))
        .collect();
    reasoning_exchange(&fixture, &items)?;
    let input = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    let kept = reasoning(&input);
    assert_eq!(kept.len(), fit, "the item past the bound is dropped");
    assert!(
        kept.iter().map(|replay| replay.byte_len()).sum::<usize>() <= MAX_REASONING_REPLAY_BYTES
    );
    assert!(
        matches!(input.last(), Some(CodexInputItem::ToolOutput { .. })),
        "the turn's call and result remain"
    );
    Ok(())
}

#[test]
fn rr_reasoning_a_summary_covers_is_not_replayed() -> Result<(), String> {
    let fixture = Fixture::new("rr-summary")?;
    reasoning_exchange(&fixture, &[item("rs_rr_0", 16)])?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    assert_eq!(
        reasoning(&continuation.request_input("use the tool", true)?).len(),
        1
    );
    let compaction = grapt_types::CompactionId::new("rr-summary").ok_or("compaction")?;
    let mut selected = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
    // Retain the whole exchange verbatim beside the summary, as a working
    // tail does, so only the reasoning rule decides what is dropped.
    selected.selection.from = 0;
    summary_tests::commit_selection(&fixture, &selected, compaction)?;
    let after = continuation.request_input("use the tool", true)?;
    assert!(
        after
            .iter()
            .any(|item| matches!(item, CodexInputItem::ToolCall { .. })),
        "the summary retains the exchange, so only the reasoning is dropped: {after:?}"
    );
    assert!(reasoning(&after).is_empty(), "{after:?}");
    Ok(())
}
