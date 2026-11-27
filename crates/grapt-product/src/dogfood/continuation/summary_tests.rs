use super::*;

#[test]
fn cr_mixed_v1_v2_descriptors_rebuild_after_restart() -> Result<(), String> {
    let mut fixture = Fixture::new("cr-mixed-version")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let first = grapt_types::CompactionId::new("cr-v1").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &first, 128_000)?;
    commit_selection_with_policy(&fixture, &selected, first, "scc-recent-step-v1")?;
    drop(continuation);
    fixture.close();
    let continuation = fixture.reopen(fixture.input.clone())?;
    fixture.inbox = Some(continuation.inbox.clone());
    fixture.artifacts = Some(continuation.artifacts.clone());
    assert!(continuation.request_input("use the tool", false).is_ok());
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(100))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("cr-second-epoch").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let block = grapt_provider::BlockOrdinal::new(0);
    for fragment in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: "Observed work. ".repeat(1000),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ] {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|error| error.to_string())?;
    }
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())?;
    drop(owner);
    let second = grapt_types::CompactionId::new("cr-v2").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &second, 128_000)?;
    assert_eq!(selected.source_epoch.value(), 1);
    commit_selection_with_policy(
        &fixture,
        &selected,
        second,
        super::super::retained::RETENTION_POLICY,
    )?;
    let before = continuation.request_with_identity("use the tool", false)?;
    assert_eq!(
        before.1.as_ref().ok_or("context identity")?.epoch.value(),
        2
    );
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    assert_eq!(
        reopened.request_with_identity("use the tool", false)?,
        before
    );
    let old_call = ToolCallId::new("call_d2a_read").ok_or("old call")?;
    let recovered: serde_json::Value = serde_json::from_slice(&super::history(
        &reopened.inbox,
        reopened.artifacts.as_ref(),
        &reopened.session,
        Some(&old_call),
        None,
        None,
    )?)
    .map_err(|error| error.to_string())?;
    assert_eq!(recovered["content"], "fixture output");
    assert!(before.0.iter().any(|item| matches!(item,
        CodexInputItem::Message(message)
            if message.role == MessageRole::Assistant
                && message.content.starts_with(SUMMARY_LABEL)
    )));
    Ok(())
}

#[test]
fn cr_completed_large_tool_result_can_compact_before_next_attempt() -> Result<(), String> {
    let fixture = Fixture::new("cr-final-cut")?;
    fixture.record_exchange_output(true, false, false, READ_TOOL, "read", &vec![b'x'; 550_033])?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let id = grapt_types::CompactionId::new("cr-final-cut").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 190_000)?;
    assert!(selected.removed_bytes >= 550_033);
    assert!(selected.text.contains("source_event_seq"));
    commit_selection(&fixture, &selected, id)?;
    let after = continuation.request_input("use the tool", true)?;
    assert!(after.iter().map(context_item_bytes).sum::<usize>() < 190_000);
    Ok(())
}

#[test]
fn hr_c01_large_output_selection_includes_later_instruction_and_recoverable_source(
) -> Result<(), String> {
    let mut fixture = Fixture::new("hr-large-output")?;
    let output = "x".repeat(550_033);
    let artifact = fixture
        .record_exchange_output(true, false, false, READ_TOOL, "read", output.as_bytes())?
        .ok_or("output artifact")?;
    let next: MessageId = "msg_hr_blue"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let prompt = "Supersede RED with BLUE. Invoice effect UNKNOWN; do not repeat it.";
    promote_test_input(fixture.inbox()?, &fixture.session, &next, prompt)?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(100))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("hr-large-overflow").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let continuation = fixture.continuation(next.clone())?;
    let compaction = grapt_types::CompactionId::new("hr-large-selection").ok_or("compaction")?;
    let selected = continuation.prepare_summary(prompt, &compaction, 190_000)?;
    let items = summary_history(&selected.text)?;
    let projected = items
        .iter()
        .find(|item| item["call"] == "call_d2a_read" && item.get("output").is_some())
        .ok_or("oversized result was excluded")?;
    assert_eq!(projected["projection"]["original_bytes"], 550_033);
    assert_eq!(projected["projection"]["truncated"], true);
    assert_eq!(
        projected["projection"]["source_session"],
        fixture.session.as_str()
    );
    assert!(projected["projection"]["source_event_seq"]
        .as_u64()
        .is_some());
    // The later instruction is shown to the summarizer as labelled context,
    // never as the history's last turn for it to answer.
    assert!(!items.iter().any(|item| item["content"] == prompt));
    assert!(selected.text.contains(&format!(
        "<latest-instruction>\n{prompt}\n</latest-instruction>"
    )));
    assert!(selected.removed_bytes >= output.len());
    assert!(
        selected.text.len() + super::super::summarization::SUMMARY_INSTRUCTION.len() <= 173_616
    );
    assert_eq!(
        read_text(
            fixture.artifacts()?,
            &artifact,
            CaptureDisposition::Complete,
            "original"
        )?
        .as_str(),
        output
    );
    commit_selection(&fixture, &selected, compaction)?;
    let after = continuation.request_input(prompt, false)?;
    assert!(after.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.role == MessageRole::User && message.content == prompt)));
    assert!(after.iter().map(context_item_bytes).sum::<usize>() < output.len());
    drop(continuation);
    drop(owner);
    fixture.close();
    assert_eq!(fixture.reopen(next)?.request_input(prompt, false)?, after);
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn scc_post_compaction_tool_history_survives_new_input_and_restart() -> Result<(), String> {
    for outcome in ["complete", "failed", "missing-failed", "missing-live"] {
        let mut fixture = Fixture::new(outcome)?;
        fixture.record_exchange()?;
        let mut owner = DurableAttemptOwner::elect_inbox_owned(
            fixture.inbox()?.clone(),
            open_artifacts(&fixture.artifact_root)?,
            Arc::new(FixedClock),
            Arc::new(CountingIds(AtomicU64::new(100))),
            NoLiveSubscribers,
            fixture.session.clone(),
            StepNumber::new(2).ok_or("step")?,
            None,
        )
        .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::AttemptStarted {
                attempt: PhysicalAttemptId::new("scc-after-cut").ok_or("attempt")?,
            })
            .map_err(|error| error.to_string())?;
        let continuation = fixture.continuation(fixture.input.clone())?;
        let compaction = grapt_types::CompactionId::new("scc-post-cut").ok_or("compaction")?;
        let selected = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
        assert!(selected.text.contains("fixture output"));
        commit_selection(&fixture, &selected, compaction)?;
        drop(continuation);
        let call = ToolCallId::new("call_after_compaction").ok_or("call")?;
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ToolCalled {
                    call: call.clone(),
                    tool: READ_TOOL.to_owned(),
                    input: r#"{"path":"fixture.txt"}"#.to_owned(),
                    execution: ToolExecution::Local,
                    state: AdapterState::none(),
                }),
            })
            .map_err(|error| error.to_string())?;
        fixture.ask_permission("evt_after_cut_permission", "per_after_cut", &call)?;
        if outcome == "complete" || outcome == "failed" {
            let terminal = if outcome == "complete" {
                let captured = fixture
                    .artifacts()?
                    .capture(
                        Cursor::new(b"after compaction output"),
                        CaptureBound::new(4096, 4096).ok_or("capture bound")?,
                        ArtifactKind::CapturedOutput,
                        Sensitivity::Ordinary,
                    )
                    .map_err(|error| error.to_string())?;
                ToolTerminalOutcome::Completed {
                    output: Some(
                        ToolTerminalArtifact::new(
                            captured.reference().clone(),
                            captured.disposition(),
                        )
                        .ok_or("terminal artifact")?,
                    ),
                }
            } else {
                ToolTerminalOutcome::Failed {
                    reason: grapt_types::ToolFailureReason::EffectFailed,
                    detail: None,
                }
            };
            owner
                .commit(DurableFact::ToolTerminal {
                    call: call.clone(),
                    outcome: terminal,
                })
                .map_err(|error| error.to_string())?;
            owner
                .commit(DurableFact::StepEnded {
                    finish: FinishReason::ToolCalls,
                })
                .map_err(|error| error.to_string())?;
        } else if outcome == "missing-failed" {
            owner
                .commit(DurableFact::StepFailed {
                    failure: grapt_runtime::StepFailure::new(
                        grapt_types::StepTerminalReason::Interrupted {
                            by: grapt_types::InterruptionSource::User,
                        },
                        "fixture lost outcome",
                    ),
                })
                .map_err(|error| error.to_string())?;
        }
        drop(owner);
        let next: MessageId = "msg_after_compaction"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &next,
            "inspect new work",
        )?;
        fixture.close();
        let continuation = fixture.reopen(next)?;
        let result = continuation.request_input("inspect new work", false);
        if outcome == "missing-live" {
            assert!(result.is_err_and(|error| error.contains("has no durable terminal output")));
            continue;
        }
        let input = result?;
        // The input selected by the descriptor keeps its exact position on
        // later inputs too; moving it into the facts would rewrite the prefix.
        let (facts, users): (Vec<_>, Vec<_>) = input
            .iter()
            .filter_map(|item| match item {
                CodexInputItem::Message(message) if message.role == MessageRole::User => {
                    Some(message.content.as_str())
                }
                _ => None,
            })
            .partition(|content| {
                content.starts_with(super::retained_facts::FACTS_LABEL)
                    || content.starts_with(super::retained_users::LEDGER_LABEL)
            });
        assert_eq!(users, ["use the tool", "inspect new work"]);
        assert_eq!(facts.len(), 1, "{facts:?}");
        assert!(
            matches!(input.first(), Some(CodexInputItem::Message(message)) if message.content == facts[0])
        );
        assert!(!facts[0].contains("- \"use the tool\"\n"), "{}", facts[0]);
        assert!(
            facts[0].contains("- read {\"path\":\"fixture.txt\"} -> "),
            "{}",
            facts[0]
        );
        if outcome == "missing-failed" {
            assert!(input.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.content.contains("effects are unknown"))));
            assert!(!input.iter().any(|item| matches!(
                item,
                CodexInputItem::ToolCall { .. } | CodexInputItem::ToolOutput { .. }
            )));
        } else {
            assert!(input.iter().any(
                |item| matches!(item, CodexInputItem::ToolCall { call: held, .. } if held == &call)
            ));
            let expected = if outcome == "complete" {
                "after compaction output"
            } else {
                "Tool failed: EffectFailed"
            };
            assert!(input.iter().any(|item| matches!(item, CodexInputItem::ToolOutput { call: held, output, is_error } if held == &call && output.as_str() == expected && *is_error == (outcome == "failed"))));
        }
        assert!(continuation.request_input("use the tool", false).is_err());
    }
    Ok(())
}

#[test]
fn scc_review_retained_older_tool_exchange_survives_budget_cut() -> Result<(), String> {
    let mut fixture = Fixture::new("review-older-tool-cut")?;
    fixture.record_exchange()?;
    let next: MessageId = "msg_review_next"
        .parse()
        .map_err(|e: grapt_types::IdError| e.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "continue work")?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(100))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|e| e.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("scc-review-overflow").ok_or("attempt")?,
        })
        .map_err(|e| e.to_string())?;
    drop(owner);
    let continuation = fixture.continuation(next.clone())?;
    let before = continuation.request_input("continue work", false)?;
    assert!(before.iter().any(|item| matches!(item, CodexInputItem::ToolOutput { output, .. } if output.as_str() == "fixture output")));
    let compaction = grapt_types::CompactionId::new("scc-review-cut").ok_or("compaction")?;
    // Room for the instruction and the request's framing, and 100 bytes of
    // history: the older exchange is retained, not summarized.
    let framing = super::super::summarization::summary_prompt(None, "continue work", "").len();
    let budget = 16_384
        + super::super::summarization::SUMMARY_INSTRUCTION.len() as u64
        + framing as u64
        + 100;
    let selected = continuation.prepare_summary("continue work", &compaction, budget)?;
    assert!(
        !selected.text.contains("fixture output"),
        "output must be retained, not summarized"
    );
    assert!(
        !selected.text.contains("arguments"),
        "call must be retained, not summarized"
    );
    commit_selection(&fixture, &selected, compaction)?;
    let after = continuation.request_input("continue work", false)?;
    assert!(after.iter().any(|item| matches!(item, CodexInputItem::ToolOutput { output, .. } if output.as_str() == "fixture output")),
        "committed retained suffix silently lost older tool output; summary={} rebuilt={after:?}", selected.text);
    assert!(after.iter().any(|item| matches!(item, CodexInputItem::ToolCall { call, .. } if call.as_str() == "call_d2a_read")));
    assert!(after.iter().any(|item| matches!(item, CodexInputItem::Message(message) if message.role == MessageRole::User && message.content == "use the tool")));
    assert!(continuation.request_input("changed intent", false).is_err());
    drop(continuation);
    fixture.close();
    assert_eq!(
        fixture
            .reopen(next)?
            .request_input("continue work", false)?,
        after
    );
    Ok(())
}

#[test]
fn scc_summary_selection_keeps_tool_pairs_and_requires_removable_history() -> Result<(), String> {
    let empty = Fixture::new("scc-no-removable-history")?;
    promote_test_input(
        empty.inbox()?,
        &empty.session,
        &empty.input,
        "only current input",
    )?;
    let no_history = empty.continuation(empty.input.clone())?;
    let empty_id = grapt_types::CompactionId::new("scc-empty").ok_or("compaction id")?;
    assert!(no_history
        .prepare_summary("only current input", &empty_id, 128_000)
        .is_err());
    let fixture = Fixture::new("scc-prefix")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let compaction = grapt_types::CompactionId::new("scc-prefix").ok_or("compaction id")?;
    // V2 may cut after the settled exchange before another attempt opens;
    // both members of the pair must be included together.
    let settled = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
    assert!(settled.text.contains("fixture output"));
    assert!(settled.text.contains("fixture.txt"));
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(100))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("scc-overflow").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let selected = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
    let items = summary_history(&selected.text)?;
    assert!(items
        .iter()
        .any(|item| item["arguments"] == r#"{"path":"fixture.txt"}"#));
    assert!(items.iter().any(|item| item["output"] == "fixture output"));
    assert!(selected.removed_bytes > 0);
    assert!(selected.selection.from <= selected.selection.source_head);
    assert!(continuation
        .prepare_summary("use the tool", &compaction, 1024)
        .is_err());
    assert!(continuation
        .prepare_summary("changed intent", &compaction, 128_000)
        .is_err());
    Ok(())
}

/// The quoted history items of a summary request.
pub(super) fn summary_history(text: &str) -> Result<Vec<serde_json::Value>, String> {
    use super::super::summarization::{HISTORY_CLOSES, HISTORY_OPENS};
    let (_, history) = text
        .split_once(&format!("{HISTORY_OPENS}\n\n"))
        .ok_or("the summary request quotes no history")?;
    let (history, _) = history
        .rsplit_once(&format!("\n\n{HISTORY_CLOSES}"))
        .ok_or("the summary request does not restate its task after the history")?;
    serde_json::from_str(history).map_err(|error| error.to_string())
}

pub(super) fn commit_selection(
    fixture: &Fixture,
    selected: &super::super::summarization::SummaryPreparation,
    compaction: grapt_types::CompactionId,
) -> Result<(), String> {
    commit_selection_with_policy(
        fixture,
        selected,
        compaction,
        super::super::retained::RETENTION_POLICY,
    )
}

pub(super) fn commit_selection_with_policy(
    fixture: &Fixture,
    selected: &super::super::summarization::SummaryPreparation,
    compaction: grapt_types::CompactionId,
    policy: &str,
) -> Result<(), String> {
    use grapt_types::{
        CompactionDescriptor, CompactionDescriptorParts, CompactionSummary, CompactionTrigger,
        ContextWindowId, NextRequestRef, RetainedHistory, RetentionPolicyVersion,
    };

    let capture = |text: String| -> Result<grapt_types::ArtifactRef, String> {
        let captured = fixture
            .artifacts()?
            .capture(
                Cursor::new(text.into_bytes()),
                CaptureBound::new(65_536, 4096).ok_or("bound")?,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|e| e.to_string())?;
        Ok(captured.reference().clone())
    };
    let summary_text = if selected.removed_bytes > "Use tool.".len() {
        "Use tool."
    } else {
        "x"
    };
    assert!(
        summary_text.len() < selected.removed_bytes,
        "summary must pass the product's progress gate"
    );
    let summary = capture(summary_text.to_owned())?;
    let selection = capture(selected.selection.encode().to_string())?;
    let event = format!("evt_review{}", selected.source_epoch.value());
    let descriptor = CompactionDescriptor::new(CompactionDescriptorParts {
        id: compaction,
        source_session: fixture.session.clone(),
        source_epoch: selected.source_epoch,
        target_window: ContextWindowId::new("review-window").ok_or("window")?,
        target_epoch: selected.source_epoch.next().ok_or("epoch overflow")?,
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new(policy).ok_or("policy")?,
        summary: CompactionSummary::ClientReadable { artifact: summary },
        retained: RetainedHistory::DeterministicSelection {
            descriptor: selection.clone(),
        },
        baseline: selection.clone(),
        next_request: NextRequestRef::new(StepNumber::new(2).ok_or("step")?, selection),
    })
    .map_err(|e| format!("{e:?}"))?;
    {
        let mut inbox = lock(fixture.inbox()?);
        let aggregate = AggregateId::Session(fixture.session.clone());
        let held = inbox.store().held_writers();
        let tx = inbox
            .store_mut()
            .governed_transaction()
            .map_err(|e| e.to_string())?;
        grapt_store::compaction::record(
            &tx,
            &event.parse::<EventId>().map_err(|e| e.to_string())?,
            held.token(&aggregate).map_err(|e| e.to_string())?,
            1_788_220_800_010,
            grapt_store::compaction::CompactionAttempt::Settled(&descriptor),
            &mut [],
        )
        .map_err(|e| e.to_string())?;
        tx.commit().map_err(|e| e.to_string())?.discard();
    }
    Ok(())
}
