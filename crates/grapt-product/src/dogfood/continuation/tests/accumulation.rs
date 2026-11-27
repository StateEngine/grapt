use super::*;

fn owner(
    fixture: &Fixture,
    step: u32,
) -> Result<DurableAttemptOwner<'static, FilesystemArtifactStore, NoLiveSubscribers>, String> {
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
    .map_err(|e| e.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("accumulation-{step}")).ok_or("attempt")?,
        })
        .map_err(|e| e.to_string())?;
    Ok(owner)
}

fn call(
    owner: &mut DurableAttemptOwner<'_, FilesystemArtifactStore, NoLiveSubscribers>,
    name: &str,
    children: usize,
    output: &ToolTerminalArtifact,
) -> Result<ToolCallId, String> {
    let parent = ToolCallId::new(name).ok_or("parent")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: parent.clone(),
                tool: if children == 0 { READ_TOOL } else { "execute" }.to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|e| e.to_string())?;
    for index in 0..children {
        let child = ToolCallId::new(format!("{name}-child-{index}")).ok_or("child")?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: child.clone(),
                tool: grapt_types::ToolDefinitionId::new("image").ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|e| e.to_string())?,
            })
            .map_err(|e| e.to_string())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call: child,
                outcome: ToolTerminalOutcome::Completed {
                    output: if index == 0 {
                        Some(output.clone())
                    } else {
                        None
                    },
                },
            })
            .map_err(|e| e.to_string())?;
    }
    owner
        .commit(DurableFact::ToolTerminal {
            call: parent.clone(),
            outcome: ToolTerminalOutcome::Completed { output: None },
        })
        .map_err(|e| e.to_string())?;
    Ok(parent)
}

fn file_output(fixture: &Fixture) -> Result<ToolTerminalArtifact, String> {
    let binary = fixture
        .artifacts()?
        .capture(
            Cursor::new([0xff, 0xfe]),
            CaptureBound::new(1024, 1024).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|e| e.to_string())?;
    let reference = binary.reference();
    let envelope = serde_json::json!({"artifacts":[{"id":reference.id().as_str(),
        "digest":reference.digest().as_str(),"length":reference.length(),
        "mimeType":"image/png","sensitivity":"secret-bearing"}]});
    let output = fixture
        .artifacts()?
        .capture(
            Cursor::new(envelope.to_string().into_bytes()),
            CaptureBound::new(4096, 4096).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|e| e.to_string())?;
    ToolTerminalArtifact::new(output.reference().clone(), output.disposition())
        .ok_or("output".into())
}

pub(super) fn accumulated(
    compact: bool,
    nested: bool,
) -> Result<(Fixture, ToolCallId, ToolCallId), String> {
    let fixture = Fixture::new("accumulation")?;
    fixture.record_exchange()?;
    let output = file_output(&fixture)?;
    let mut before = owner(&fixture, 2)?;
    let old = call(
        &mut before,
        "old-call",
        if nested { 64 } else { 0 },
        &output,
    )?;
    for index in 0..if nested { 1 } else { 119 } {
        call(
            &mut before,
            &format!("prior-{index}"),
            if nested { 64 } else { 0 },
            &output,
        )?;
    }
    before
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|e| e.to_string())?;
    drop(before);
    let mut after = owner(&fixture, 3)?;
    if compact {
        let continuation = fixture.continuation(fixture.input.clone())?;
        let id = grapt_types::CompactionId::new("accumulation-cut").ok_or("compaction")?;
        let selected = continuation.prepare_summary("use the tool", &id, 128_000)?;
        assert!(selected.removed_bytes > 0);
        super::summary_tests::commit_selection(&fixture, &selected, id)?;
    }
    let current = call(
        &mut after,
        "current-call",
        if nested { 2 } else { 0 },
        &output,
    )?;
    if !nested {
        for index in 0..12 {
            call(&mut after, &format!("later-{index}"), 0, &output)?;
        }
    }
    after
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|e| e.to_string())?;
    drop(after);
    Ok((fixture, old, current))
}

#[test]
fn completed_effects_do_not_limit_session_lifetime_or_target_history() -> Result<(), String> {
    for nested in [false, true] {
        let (mut fixture, old, current) = accumulated(true, nested)?;
        let input = fixture.input.clone();
        for restarted in [false, true] {
            let continuation = if restarted {
                fixture.close();
                fixture.reopen(input.clone())?
            } else {
                fixture.continuation(input.clone())?
            };
            let request = continuation.request_input("use the tool", false)?;
            assert!(request.iter().any(
                |item| matches!(item, CodexInputItem::ToolCall {call,..} if call == &current)
            ));
            // Recent completed calls can remain exact; the first exchange
            // still leaves active context without losing its durable history.
            assert!(!request
                .iter()
                .any(|item| matches!(item, CodexInputItem::ToolCall {call,..}
                    if call.as_str() == "call_d2a_read")));
            let count: i64 = lock(&continuation.inbox)
                .store()
                .connection()
                .query_row(
                    "SELECT count(*) FROM event WHERE type = 'session.tool.terminal.1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| e.to_string())?;
            assert!(count > 128, "must cross the reported lifetime boundary");
            for (target, expected) in [(&old, 64), (&current, 2)] {
                let projection = tool_projection(
                    &continuation.inbox,
                    &continuation.artifacts,
                    &fixture.session,
                    target,
                )?;
                assert!(projection.settled);
                if nested {
                    assert_eq!(
                        projection.progress.ok_or("progress missing")?.calls().len(),
                        expected
                    );
                    assert_eq!(projection.files.len(), 1);
                    assert!(
                        files::content(&projection.files[0], &continuation.artifacts)?
                            .contains("image/png")
                    );
                    let child =
                        ToolCallId::new(format!("{}-child-0", target.as_str())).ok_or("child")?;
                    assert_eq!(
                        progress::parent(
                            lock(&continuation.inbox).store(),
                            &fixture.session,
                            &child
                        )?,
                        Some(target.clone())
                    );
                } else {
                    assert!(projection.progress.is_none());
                }
            }
        }
    }
    Ok(())
}

#[test]
fn pre_compaction_unresolved_effects_still_quarantine_after_restart() -> Result<(), String> {
    for unknown in [false, true] {
        let (mut fixture, _, _) = accumulated(false, true)?;
        let mut lost = owner(&fixture, 4)?;
        let child = ToolCallId::new("lost-child").ok_or("child")?;
        lost.commit(DurableFact::NestedToolCalled {
            parent: ToolCallId::new("lost-parent").ok_or("parent")?,
            call: child.clone(),
            tool: grapt_types::ToolDefinitionId::new("effect").ok_or("tool")?,
            catalog: grapt_types::ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
            input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                .map_err(|e| e.to_string())?,
        })
        .map_err(|e| e.to_string())?;
        if unknown {
            lost.commit(DurableFact::ToolTerminal {
                call: child,
                outcome: ToolTerminalOutcome::Failed {
                    reason: grapt_types::ToolFailureReason::EffectUnknown,
                    detail: None,
                },
            })
            .map_err(|e| e.to_string())?;
        }
        drop(lost);
        let retained = owner(&fixture, 5)?;
        drop(retained);
        let head: i64 = lock(fixture.inbox()?).store().connection().query_row(
            "SELECT MAX(seq) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
            [fixture.session.as_str()], |row|row.get(0)).map_err(|e|e.to_string())?;
        let head = u64::try_from(head).map_err(|e| e.to_string())?;
        let id = grapt_types::CompactionId::new("unknown-cut").ok_or("compaction")?;
        // Stage a persisted cut past an unresolved effect through the normal
        // descriptor owner. Read-time safety cannot trust a summary to settle it.
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
                from: head,
                source_head: head,
                admission: None,
                server: None,
                summary_plan: None,
            },
        };
        super::summary_tests::commit_selection(&fixture, &selected, id)?;
        let input = fixture.input.clone();
        fixture.close();
        let continuation = fixture.reopen(input)?;
        assert!(super::super::retained::RetainedContext::load(
            &lock(&continuation.inbox),
            &continuation.artifacts,
            &continuation.session,
            &continuation.input
        )?
        .is_some());
        let request = continuation.request_input("use the tool", false);
        if unknown {
            // A recorded unknown outcome admits a warned prompt; only a start
            // with no terminal remains quarantine past the cut.
            request?;
            continue;
        }
        let error = request.err().ok_or("unresolved effect was ignored")?;
        assert_eq!(error, super::super::queries::ORPHANED_REFUSAL);
    }
    Ok(())
}

/// The tool calls a continuation hands the model, in request order.
fn requested_calls(items: &[CodexInputItem]) -> Vec<ToolCallId> {
    items
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ToolCall { call, .. } => Some(call.clone()),
            _ => None,
        })
        .collect()
}

/// A parent whose top-level call made nested calls is forked through the
/// lifecycle owner, as the product's fork route does. The fork's next turn
/// goes through the real continuation consumer, before and after the store
/// reopens: it carries the parent's top-level call and result, never a nested
/// call as one the model made, and runs nothing again.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_fork_of_a_nested_tool_session_continues_with_only_top_level_calls() -> Result<(), String> {
    let mut fixture = Fixture::new("nested-fork")?;
    let ids = CountingIds(AtomicU64::new(0x7100_0000));
    let clock = FixedClock;
    let authorities = grapt_types::DeterministicAuthorities::new(&clock, &ids);
    let seed = grapt_session::SessionSeed::new(
        grapt_types::ProjectId::new("prj_nested_fork").ok_or("project")?,
        "/work/nested-fork",
        "nested-fork",
    )
    .map_err(|e| e.to_string())?;
    fixture.session = {
        let mut inbox = lock(fixture.inbox()?);
        let mut listed = grapt_session::SessionListProjector;
        grapt_session::create_session(inbox.store_mut(), &authorities, &seed, &mut [&mut listed])
            .map_err(|e| e.to_string())?
            .session
    };
    fixture.record_exchange()?;
    let output = file_output(&fixture)?;
    let mut step = owner(&fixture, 2)?;
    let top = call(&mut step, "fork-top", 2, &output)?;
    step.commit(DurableFact::StepEnded {
        finish: FinishReason::ToolCalls,
    })
    .map_err(|e| e.to_string())?;
    drop(step);
    let nested: Vec<ToolCallId> = (0..2)
        .map(|index| ToolCallId::new(format!("fork-top-child-{index}")).ok_or("child"))
        .collect::<Result<_, _>>()?;

    let parent_calls = requested_calls(
        &fixture
            .continuation(fixture.input.clone())?
            .request_input("use the tool", false)?,
    );
    assert!(parent_calls.contains(&top));

    let child = {
        let mut inbox = lock(fixture.inbox()?);
        let mut projector = grapt_inbox::InboxProjector;
        grapt_session::fork_session(
            inbox.store_mut(),
            &authorities,
            &fixture.session,
            &mut [&mut projector],
        )
        .map_err(|e| e.to_string())?
        .session
    };
    let next = "msg_nested_fork_next"
        .parse::<MessageId>()
        .map_err(|e| e.to_string())?;
    promote_test_input(fixture.inbox()?, &child, &next, "continue in the fork")?;
    let terminals = |inbox: &Arc<Mutex<Inbox>>| -> Result<i64, String> {
        lock(inbox)
            .store()
            .connection()
            .query_row(
                "SELECT count(*) FROM event WHERE aggregate_id = ?1
                 AND type = 'session.tool.terminal.1'",
                [child.as_str()],
                |row| row.get(0),
            )
            .map_err(|e| e.to_string())
    };
    let copied = terminals(fixture.inbox()?)?;

    for reopened in [false, true] {
        let continuation = if reopened {
            fixture.close();
            fixture.session = child.clone();
            fixture.reopen(next.clone())?
        } else {
            DurableContinuation::new(
                Arc::clone(fixture.inbox()?),
                Arc::clone(fixture.artifacts()?),
                child.clone(),
                next.clone(),
            )
        };
        // Compacting the fork reads its settled tool calls through the
        // model-terminal and recent-window consumers, which tell nested from
        // top-level only by the classification row in the fork's own aggregate.
        let cut = grapt_types::CompactionId::new("nested-fork-cut").ok_or("compaction")?;
        continuation
            .prepare_summary("continue in the fork", &cut, 128_000)
            .map_err(|error| {
                format!("compacting the fork refused (reopened: {reopened}): {error}")
            })?;
        let request = continuation.request_input("continue in the fork", false)?;
        let calls = requested_calls(&request);
        assert_eq!(
            calls, parent_calls,
            "the fork's next turn hands the model other calls than the parent's (reopened: {reopened})"
        );
        for call in &nested {
            assert!(
                !calls.contains(call),
                "the fork hands the model nested call {} as its own (reopened: {reopened})",
                call.as_str()
            );
        }
        assert_eq!(
            terminals(&continuation.inbox)?,
            copied,
            "continuing the fork ran parent work again"
        );
    }
    Ok(())
}
