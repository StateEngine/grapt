use super::*;
use grapt_event::session_message::AssistantSelection;
use grapt_provider::BlockOrdinal;

fn signed_exchange(fixture: &Fixture, replay: &str) -> Result<(), String> {
    signed_exchange_at(fixture, replay, 1)
}

fn signed_exchange_at(fixture: &Fixture, replay: &str, step: u32) -> Result<(), String> {
    if step == 1 {
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &fixture.input,
            "use the tool",
        )?;
    }
    let selection = AssistantSelection::new(
        AgentId::new("pc-agent").ok_or("agent")?,
        grapt_types::ProviderId::new("pc-anthropic").ok_or("provider")?,
        grapt_types::ModelId::new("pc-model").ok_or("model")?,
        None,
    );
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(u64::from(step - 1) * 100))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?
    .with_selection(selection)
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("pat_pc_signed_{step}")).ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let block = BlockOrdinal::new(0);
    let call = ToolCallId::new(format!("call_pc_signed_{step}")).ok_or("call")?;
    for fragment in [
        AttemptFragment::ReasoningOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::ReasoningDelta {
            block,
            text: "inspect the file".into(),
            state: AdapterState::none(),
        },
        AttemptFragment::ReasoningClosed {
            block,
            state: AdapterState::retain(replay).map_err(|error| error.to_string())?,
        },
        AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: READ_TOOL.into(),
            input: r#"{"path":"fixture.txt"}"#.into(),
            execution: ToolExecution::Local,
            state: AdapterState::none(),
        },
    ] {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|error| error.to_string())?;
    }
    fixture.ask_permission(
        &format!("evt_pc_signed_{step}"),
        &format!("per_pc_signed_{step}"),
        &call,
    )?;
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

fn replay() -> String {
    serde_json::json!({"kind":"anthropic-thinking-v1","thinking":"inspect the file","signature":"PC_SIGNATURE_NEVER_PROSE","binding":vec![0u8;32]}).to_string()
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_text_replaces_only_its_exact_adjacent_rendering_after_restart() -> Result<(), String> {
    const RENDERED: &str = "answer [fixture](https://example.com)";
    for (label, visible) in [("exact", RENDERED), ("different", "other visible text")] {
        let mut fixture = Fixture::new(&format!("native-text-{label}"))?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &fixture.input,
            "use the tool",
        )?;
        let selection = AssistantSelection::new(
            AgentId::new("native-agent").ok_or("agent")?,
            grapt_types::ProviderId::new("pc-anthropic").ok_or("provider")?,
            grapt_types::ModelId::new("pc-model").ok_or("model")?,
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
                attempt: PhysicalAttemptId::new("pat_native_text").ok_or("attempt")?,
            })
            .map_err(|error| error.to_string())?;
        let replay = serde_json::json!({"kind":"anthropic-text-v1","binding":vec![0u8;32],
            "rendered":RENDERED,"item":{"type":"text","text":"answer","citations":[{
                "type":"web_search_result_location","url":"https://example.com","title":"fixture",
                "cited_text":"source","encrypted_index":"PRIVATE_INDEX"}]}})
        .to_string();
        for (ordinal, text, state) in [
            (0, RENDERED, AdapterState::none()),
            (
                1,
                visible,
                AdapterState::retain(replay).map_err(|error| error.to_string())?,
            ),
        ] {
            let block = BlockOrdinal::new(ordinal);
            for fragment in [
                AttemptFragment::TextOpened {
                    block,
                    state: AdapterState::none(),
                },
                AttemptFragment::TextDelta {
                    block,
                    text: text.into(),
                    state: AdapterState::none(),
                },
                AttemptFragment::TextClosed { block, state },
            ] {
                owner
                    .commit(DurableFact::Fragment {
                        fragment: Box::new(fragment),
                    })
                    .map_err(|error| error.to_string())?;
            }
        }
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::Stop,
            })
            .map_err(|error| error.to_string())?;
        drop(owner);
        fixture.close();
        let context = fixture
            .reopen(fixture.input.clone())?
            .request_input("use the tool", false)?;
        let text: Vec<_> = context
            .iter()
            .filter_map(|item| match item {
                CodexInputItem::Message(message) if message.role == MessageRole::Assistant => {
                    Some(message.content.as_str())
                }
                _ => None,
            })
            .collect();
        assert_eq!(
            text.iter().filter(|text| **text == RENDERED).count(),
            1,
            "the independent earlier text must survive while the native rendering is suppressed"
        );
        assert!(text.contains(&visible));
        if visible != RENDERED {
            assert!(
                text.contains(&"other visible text"),
                "nonmatching text must not be suppressed"
            );
        }
        let native = context
            .iter()
            .find_map(|item| match item {
                CodexInputItem::ProviderReplay(replay) if replay.is_anthropic_text() => {
                    Some(replay)
                }
                _ => None,
            })
            .ok_or("native text replay missing")?;
        assert_eq!(native.anthropic_rendered_text(), Some(RENDERED));
        assert!(!format!("{context:?}").contains("PRIVATE_INDEX"));
    }
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn fork_omits_inherited_thinking_but_keeps_its_own_across_restart() -> Result<(), String> {
    let mut fixture = Fixture::new("fork-signed")?;
    let ids = CountingIds(AtomicU64::new(1_000_000));
    let authorities = grapt_types::DeterministicAuthorities::new(&FixedClock, &ids);
    let parent = {
        let mut inbox = lock(fixture.inbox()?);
        let seed = grapt_session::SessionSeed::new(
            ProjectId::new("prj_signed_fork").ok_or("project")?,
            fixture.artifact_root.to_string_lossy(),
            "signed fork",
        )
        .map_err(|error| error.to_string())?;
        let mut projector = grapt_session::SessionListProjector;
        grapt_session::create_session(
            inbox.store_mut(),
            &authorities,
            &seed,
            &mut [&mut projector],
        )
        .map_err(|error| error.to_string())?
        .session
    };
    fixture.session = parent.clone();
    signed_exchange(&fixture, &replay())?;
    let parent_input = fixture
        .continuation(fixture.input.clone())?
        .request_input("use the tool", true)?;
    assert!(parent_input
        .iter()
        .any(|item| matches!(item, CodexInputItem::ProviderReplay(_))));
    let child = {
        let mut inbox = lock(fixture.inbox()?);
        let mut projector = grapt_inbox::InboxProjector;
        grapt_session::fork_session(
            inbox.store_mut(),
            &authorities,
            &parent,
            &mut [&mut projector],
        )
        .map_err(|error| error.to_string())?
        .session
    };
    fixture.session = child;
    fixture.input = "msg_child_signed"
        .parse::<MessageId>()
        .map_err(|error| error.to_string())?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "child task",
    )?;
    let inherited = fixture
        .continuation(fixture.input.clone())?
        .request_input("child task", false)?;
    assert!(!inherited
        .iter()
        .any(|item| matches!(item, CodexInputItem::ProviderReplay(_))));
    assert!(inherited
        .iter()
        .any(|item| matches!(item, CodexInputItem::ToolCall { .. })));
    assert!(inherited
        .iter()
        .any(|item| matches!(item, CodexInputItem::ToolOutput { .. })));
    {
        let mut inbox = lock(fixture.inbox()?);
        grapt_session::execution::begin(
            inbox.store_mut(),
            &authorities,
            &fixture.session,
            &grapt_types::ExecutionId::new("exe_child_signed").ok_or("execution")?,
        )?;
    }
    let selection = AssistantSelection::new(
        AgentId::new("pc-agent").ok_or("agent")?,
        grapt_types::ProviderId::new("pc-anthropic").ok_or("provider")?,
        grapt_types::ModelId::new("pc-model").ok_or("model")?,
        None,
    );
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(2_000_000))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(2).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?
    .with_selection(selection)
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_child_signed").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    for fragment in [
        AttemptFragment::ReasoningOpened {
            block: BlockOrdinal::new(0),
            state: AdapterState::none(),
        },
        AttemptFragment::ReasoningClosed {
            block: BlockOrdinal::new(0),
            state: AdapterState::retain(replay()).map_err(|error| error.to_string())?,
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
    let input = fixture
        .continuation(fixture.input.clone())?
        .request_input("child task", false)?;
    assert_eq!(
        input
            .iter()
            .filter(|item| matches!(item, CodexInputItem::ProviderReplay(_)))
            .count(),
        1
    );
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    assert_eq!(reopened.request_input("child task", false)?, input);
    Ok(())
}

#[test]
fn pc_signed_history_survives_tool_followup_restart_and_summary_without_signature_prose(
) -> Result<(), String> {
    let mut fixture = Fixture::new("pc-signed")?;
    signed_exchange(&fixture, &replay())?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let input = continuation.request_input("use the tool", true)?;
    assert!(matches!(
        &input[..],
        [
            CodexInputItem::Message(_),
            CodexInputItem::AssistantResponseStart,
            CodexInputItem::ProviderReplay(_),
            CodexInputItem::ToolCall { .. },
            CodexInputItem::ToolOutput { is_error: true, .. }
        ]
    ));
    assert!(!format!("{input:?}").contains("PC_SIGNATURE_NEVER_PROSE"));
    let compaction = grapt_types::CompactionId::new("pc-signed-summary").ok_or("compaction")?;
    let settled = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
    assert!(settled.text.contains("provider-replay"));
    assert!(settled.text.contains("arguments") && settled.text.contains("output"));
    assert!(!settled.text.contains("PC_SIGNATURE_NEVER_PROSE"));
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(fixture.inbox()?),
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
            attempt: PhysicalAttemptId::new("pat_pc_summary").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let selected = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
    assert!(!selected.text.contains("PC_SIGNATURE_NEVER_PROSE"));
    drop(owner);
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    assert_eq!(reopened.request_input("use the tool", true)?, input);
    Ok(())
}

#[test]
fn pc_corrupt_or_missing_signature_artifact_refuses_followup_after_restart() -> Result<(), String> {
    for payload in [
        "{",
        r#"{"kind":"anthropic-thinking-v1","thinking":"x","signature":""}"#,
    ] {
        let mut fixture = Fixture::new("pc-corrupt-replay")?;
        signed_exchange(&fixture, payload)?;
        fixture.close();
        let reopened = fixture.reopen(fixture.input.clone())?;
        assert!(reopened.request_input("use the tool", true).is_err());
    }
    Ok(())
}

#[test]
fn pc_missing_or_corrupt_opaque_object_refuses_restarted_request() -> Result<(), String> {
    for missing in [true, false] {
        let mut fixture = Fixture::new("pc-opaque-object")?;
        signed_exchange(&fixture, &replay())?;
        let reference = {
            let inbox = lock(fixture.inbox()?);
            let history = SessionHistoryStore::over(inbox.store())
                .fold(&fixture.session)
                .map_err(|error| error.to_string())?;
            let messages = history.messages().map_err(|error| error.to_string())?;
            messages
                .iter()
                .filter_map(|message| message.content.as_ref())
                .flat_map(|content| content.items())
                .find(|held| matches!(held.item.kind(), grapt_types::ContentKind::Opaque))
                .ok_or("opaque artifact missing")?
                .item
                .payload()
                .clone()
        };
        let identity = reference.id().as_str();
        let metadata = fixture
            .artifacts()?
            .stat(reference.id())
            .map_err(|error| error.to_string())?;
        assert_eq!(
            metadata.sensitivity(),
            Sensitivity::SecretBearing,
            "opaque replay must use private artifact addressing"
        );
        let object = fixture
            .artifact_root
            .join("objects")
            .join(identity.get(..2).ok_or("shard")?)
            .join(identity);
        if missing {
            std::fs::remove_file(object).map_err(|error| error.to_string())?;
        } else {
            std::fs::write(object, b"corrupt signed state").map_err(|error| error.to_string())?;
        }
        fixture.close();
        assert!(fixture
            .reopen(fixture.input.clone())?
            .request_input("use the tool", true)
            .is_err());
    }
    Ok(())
}

#[test]
fn pc_public_hydration_never_reads_or_displays_private_replay_artifacts() -> Result<(), String> {
    let mut fixture = Fixture::new("pc-public-replay")?;
    fixture.close();
    let store =
        EventStore::open(&fixture.database, "pc-public").map_err(|error| error.to_string())?;
    let target = crate::dogfood::production::tests::projection_target()?;
    let hub = crate::dogfood::event_hub()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &fixture._scratch.0,
        store,
        &target,
        hub,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )
    .map_err(|error| error.to_string())?;
    let artifacts = FilesystemArtifactStore::open(
        fixture._scratch.0.join("dogfood-artifacts"),
        crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
        Box::new(FixedClock),
    )
    .map_err(|error| error.to_string())?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(&app.inbox),
        artifacts,
        Arc::new(FixedClock),
        Arc::new(CountingIds::default()),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_pc_private").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let block = BlockOrdinal::new(0);
    for fragment in [
        AttemptFragment::ReasoningOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::ReasoningClosed {
            block,
            state: AdapterState::retain(replay()).map_err(|error| error.to_string())?,
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
    let message = {
        let inbox = lock(&app.inbox);
        let history = SessionHistoryStore::over(inbox.store())
            .fold(&fixture.session)
            .map_err(|error| error.to_string())?;
        history
            .messages()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or("message")?
    };
    let before = app.assistant_json(&fixture.session, &message)?;
    assert!(!before.contains("PC_SIGNATURE_NEVER_PROSE"));
    let content = message.content.as_ref().ok_or("content")?;
    let reference = content.items().first().ok_or("opaque item")?.item.payload();
    let identity = reference.id().as_str();
    std::fs::remove_file(
        fixture
            ._scratch
            .0
            .join("dogfood-artifacts")
            .join("objects")
            .join(identity.get(..2).ok_or("shard")?)
            .join(identity),
    )
    .map_err(|error| error.to_string())?;
    assert_eq!(
        app.assistant_json(&fixture.session, &message)?,
        before,
        "public hydration must not materialize private replay bytes"
    );
    Ok(())
}

#[test]
fn a_rewritten_summary_drops_old_thinking_and_keeps_the_tool_exchange() -> Result<(), String> {
    for (label, replay) in [
        ("signed", replay()),
        (
            "redacted",
            serde_json::json!({"kind":"anthropic-redacted-thinking-v1",
                "data":"PRIVATE_REDACTED_THINKING", "binding":vec![0u8;32]})
            .to_string(),
        ),
    ] {
        let mut fixture = Fixture::new(&format!("rewritten-thinking-{label}"))?;
        signed_exchange(&fixture, &replay)?;
        let continuation = fixture.continuation(fixture.input.clone())?;
        assert!(continuation
            .request_input("use the tool", true)?
            .iter()
            .any(|item| matches!(item, CodexInputItem::ProviderReplay(_))));
        let compaction =
            grapt_types::CompactionId::new(format!("rewritten-{label}")).ok_or("compaction")?;
        let selected = continuation.prepare_summary("use the tool", &compaction, 128_000)?;
        let descriptor = super::recent::anchor_descriptor(
            &fixture,
            compaction.as_str(),
            1,
            selected.selection.source_head,
        )?;
        super::recent::commit_descriptor(&fixture, &format!("evt_rewritten_{label}"), &descriptor)?;
        let rebuilt = continuation.request_input("use the tool", true)?;
        assert!(
            !rebuilt
                .iter()
                .any(|item| matches!(item, CodexInputItem::ProviderReplay(_))),
            "a rewritten prefix retained old Anthropic thinking"
        );
        assert!(rebuilt
            .iter()
            .any(|item| matches!(item, CodexInputItem::ToolCall { .. })));
        assert!(rebuilt
            .iter()
            .any(|item| matches!(item, CodexInputItem::ToolOutput { .. })));
        assert_eq!(continuation.request_input("use the tool", true)?, rebuilt);
        signed_exchange_at(&fixture, &replay, 2)?;
        let followup = continuation.request_input("use the tool", true)?;
        assert_eq!(
            followup
                .iter()
                .filter(|item| matches!(item, CodexInputItem::ProviderReplay(_)))
                .count(),
            1,
            "the new tool-use exchange lost its unchanged thinking"
        );
        assert_eq!(continuation.request_input("use the tool", true)?, followup);
        drop(continuation);
        fixture.close();
        assert_eq!(
            fixture
                .reopen(fixture.input.clone())?
                .request_input("use the tool", true)?,
            followup
        );
    }
    Ok(())
}
