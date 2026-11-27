use super::*;

/// Checked on actual ProductPlan assemblies, not historical inspection reads.
pub(in crate::dogfood) fn assert_coding_final_turn(input: &[CodexInputItem]) {
    assert!(
        matches!(input.last(), Some(CodexInputItem::ToolOutput { .. }))
            || matches!(input.last(), Some(CodexInputItem::Message(message))
            if message.role != MessageRole::Assistant),
        "coding request must end in user, tool or instruction content, never assistant prefill"
    );
}

type Owner = DurableAttemptOwner<'static, FilesystemArtifactStore, NoLiveSubscribers>;

fn owner(fixture: &Fixture, step: u32) -> Result<Owner, String> {
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(u64::from(step) * 10_000))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("response-{step}")).ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    Ok(owner)
}

fn text(owner: &mut Owner, close: bool) -> Result<(), String> {
    let block = grapt_provider::BlockOrdinal::new(0);
    for held in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: "response text".into(),
            state: AdapterState::none(),
        },
    ] {
        fragment(owner, held)?;
    }
    if close {
        fragment(
            owner,
            AttemptFragment::TextClosed {
                block,
                state: AdapterState::none(),
            },
        )?;
    }
    Ok(())
}

fn fragment(owner: &mut Owner, fragment: AttemptFragment) -> Result<(), String> {
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(fragment),
        })
        .map_err(|error| error.to_string())
}

fn call(owner: &mut Owner, name: &str) -> Result<ToolCallId, String> {
    let call = ToolCallId::new(name).ok_or("call")?;
    fragment(
        owner,
        AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: READ_TOOL.into(),
            input: "{}".into(),
            execution: ToolExecution::Local,
            state: AdapterState::none(),
        },
    )?;
    Ok(call)
}

fn result(owner: &mut Owner, call: ToolCallId) -> Result<(), String> {
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Completed { output: None },
        })
        .map_err(|error| error.to_string())
}

fn shape(input: &[CodexInputItem]) -> Vec<String> {
    input
        .iter()
        .map(|item| match item {
            CodexInputItem::AssistantResponseStart => "response".into(),
            CodexInputItem::Message(message) => format!("{:?}:{}", message.role, message.content),
            CodexInputItem::ToolCall { call, .. } => format!("call:{call}"),
            CodexInputItem::ToolOutput { call, .. } => format!("result:{call}"),
            _ => panic!("unexpected response fixture input"),
        })
        .collect()
}

#[test]
fn durable_parallel_results_follow_all_response_content_after_reopen() -> Result<(), String> {
    // Closing text before, between or after calls changes its content ordinal.
    // In the last case it commits between asynchronous results: the C04 bug.
    for text_at in [0, 1, 2] {
        let mut fixture = Fixture::new("response-order")?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &fixture.input,
            "inspect",
        )?;
        if text_at == 2 {
            // Session sequences permit gaps. Skip the unrelated C04 prelude so
            // the real writer reproduces its exact content/result seq10..15.
            recent::raw_conn(&fixture)?.execute(
                "UPDATE event_sequence SET seq = 6 WHERE aggregate_family = 'session' AND aggregate_id = ?1",
                [fixture.session.as_str()],
            ).map_err(|error| error.to_string())?;
        }
        let mut first = owner(&fixture, 1)?;
        if text_at == 0 {
            text(&mut first, true)?;
        }
        if text_at == 2 {
            text(&mut first, false)?;
        }
        let a = call(&mut first, "call-a")?;
        if text_at == 1 {
            text(&mut first, true)?;
        }
        let b = call(&mut first, "call-b")?;
        result(&mut first, a)?;
        if text_at == 2 {
            fragment(
                &mut first,
                AttemptFragment::TextClosed {
                    block: grapt_provider::BlockOrdinal::new(0),
                    state: AdapterState::none(),
                },
            )?;
        }
        result(&mut first, b)?;
        first
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
        drop(first);
        let continuation = fixture.continuation(fixture.input.clone())?;
        let context = continuation.request_context("inspect", true, None, true)?;
        let raw = shape(
            &context
                .rows
                .iter()
                .map(|(_, item)| item.clone())
                .collect::<Vec<_>>(),
        );
        if text_at == 2 {
            assert_eq!(
                context.rows.iter().map(|(seq, _)| *seq).collect::<Vec<_>>(),
                [1, 10, 12, 13, 14, 15],
                "exact C04 durable commit layout"
            );
            assert!(
                raw.iter().position(|s| s == "result:call-a")
                    < raw.iter().position(|s| s == "Assistant:response text")
            );
            assert!(
                raw.iter().position(|s| s == "Assistant:response text")
                    < raw.iter().position(|s| s == "result:call-b")
            );
        }
        let before = context.into_input();
        assert_coding_final_turn(&before);
        let mut expected = vec!["User:inspect", "response", "call:call-a", "call:call-b"];
        expected.insert(2 + text_at, "Assistant:response text");
        expected.extend(["result:call-a", "result:call-b"]);
        assert_eq!(shape(&before), expected);
        drop(continuation);
        fixture.close();
        let after = fixture
            .reopen(fixture.input.clone())?
            .request_input("inspect", true)?;
        assert_eq!(
            before, after,
            "restart must preserve every request-local item byte"
        );
    }
    Ok(())
}

#[test]
fn consecutive_durable_responses_and_user_boundaries_stay_distinct() -> Result<(), String> {
    let mut fixture = Fixture::new("distinct-responses")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    for step in [1, 2] {
        let mut owner = owner(&fixture, step)?;
        text(&mut owner, true)?;
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::Stop,
            })
            .map_err(|error| error.to_string())?;
    }
    let next: MessageId = "msg_next_response"
        .parse()
        .map_err(|e: grapt_types::IdError| e.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "next input")?;
    let before = fixture
        .continuation(next.clone())?
        .request_input("next input", false)?;
    assert_coding_final_turn(&before);
    assert_eq!(
        shape(&before),
        [
            "User:inspect",
            "response",
            "Assistant:response text",
            "response",
            "Assistant:response text",
            "User:next input"
        ]
    );
    fixture.close();
    assert_eq!(
        before,
        fixture.reopen(next)?.request_input("next input", false)?
    );
    Ok(())
}

#[test]
fn interrupted_response_keeps_new_user_last_after_restart() -> Result<(), String> {
    let mut fixture = Fixture::new("interrupted-final-turn")?;
    fixture.record_exchange_failure(false, false, true)?;
    let next: MessageId = "msg_after_interruption"
        .parse()
        .map_err(|e: grapt_types::IdError| e.to_string())?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &next,
        "inspect current state",
    )?;
    let before = fixture
        .continuation(next.clone())?
        .request_input("inspect current state", false)?;
    assert_coding_final_turn(&before);
    fixture.close();
    let after = fixture
        .reopen(next)?
        .request_input("inspect current state", false)?;
    assert_eq!(before, after);
    Ok(())
}

#[test]
fn response_boundaries_participate_in_structural_compaction_admission() -> Result<(), String> {
    let fixture = Fixture::new("response-marker-bound")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    // Three durable rows per step and one user fit the item bound; one
    // response marker per step takes it one past. Every other family fits:
    // one user, a bound's worth of calls/results, bounded event count.
    for step in 1..=MAX_CONTINUATION_ROWS as u32 {
        let mut current = owner(&fixture, step)?;
        text(&mut current, true)?;
        let called = call(&mut current, &format!("bound-{step}"))?;
        result(&mut current, called)?;
        current
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
        if step == 128 {
            // SC-REL-03 / SC-CACHE-03: this short transcript used to
            // compact despite being well below its token window.
            let continuation = fixture.continuation(fixture.input.clone())?;
            assert!(continuation.structural_admission()?.is_none());
            assert_eq!(continuation.request_input("inspect", true)?.len(), 513);
        }
    }
    let continuation = fixture.continuation(fixture.input.clone())?;
    let refusal = continuation
        .request_input("inspect", true)
        .err()
        .ok_or("request items past the bound escaped the context bound")?;
    assert!(refusal.contains("response groups exceed"), "{refusal}");
    let admission = continuation
        .structural_admission()?
        .ok_or("response marker overflow missed structural compaction")?;
    let items = MAX_CONTEXT_ITEMS as u64;
    assert_eq!((admission.observed, admission.limit), (items + 1, items));
    Ok(())
}

#[test]
fn instruction_update_committed_between_call_and_result_follows_the_complete_response_group(
) -> Result<(), String> {
    let fixture = Fixture::new("epoch-response-order")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "inspect",
    )?;
    let source = grapt_types::InstructionKey::new("project/agents").ok_or("instruction key")?;
    let refresh = |id: &str, value: &str| -> Result<(), String> {
        let mut inbox = lock(fixture.inbox()?);
        grapt_instruction::InstructionService::refresh(
            inbox.store_mut(),
            grapt_instruction::RefreshInput {
                event: id.parse().map_err(|error| format!("{error}"))?,
                session: fixture.session.clone(),
                created_ms: FixedClock.now().as_millis(),
                required: vec![source.clone()],
                observations: vec![grapt_instruction::SourceObservation::Available {
                    key: source.clone(),
                    value: grapt_instruction::InstructionValue::Text(value.to_owned()),
                }],
            },
        )
        .map_err(|error| error.to_string())?;
        Ok(())
    };
    refresh("evt_epoch_before", "first rule")?;
    let mut active = owner(&fixture, 1)?;
    let invoked = call(&mut active, "epoch-call")?;
    refresh("evt_epoch_between", "updated rule")?;
    result(&mut active, invoked.clone())?;
    active
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;
    drop(active);
    let history = {
        let inbox = lock(fixture.inbox()?);
        grapt_instruction::InstructionService::request_history(inbox.store(), &fixture.session)
            .map_err(|error| error.to_string())?
            .ok_or("instruction history missing")?
    };
    let continuation = fixture.continuation(fixture.input.clone())?;
    let (items, _, _) =
        continuation.request_with_instructions("inspect", true, &history.updates)?;
    let call_index = items
        .iter()
        .position(|item| matches!(item, CodexInputItem::ToolCall { call, .. } if *call == invoked))
        .ok_or("call missing")?;
    assert!(
        matches!(items.get(call_index + 1), Some(CodexInputItem::ToolOutput { call, .. }) if *call == invoked)
    );
    assert!(
        matches!(items.get(call_index + 2), Some(CodexInputItem::Message(message)) if message.role == MessageRole::System && message.content.contains("updated rule"))
    );
    assert_coding_final_turn(&items);
    Ok(())
}

#[path = "context_capacity.rs"]
mod context_capacity;
