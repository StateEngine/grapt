//! Required compatible reasoning is read from its own committed content.
use super::*;
use grapt_event::session_message::AssistantSelection;
use grapt_provider::{BlockOrdinal, CompatibleReasoningReplay, ProtocolFamily};
fn target() -> Result<grapt_types::ResolvedModelTarget, String> {
    crate::dogfood::production::tests::projection_target()
}
fn record(fixture: &Fixture, step: u32, foreign: bool, reasons: &[&str]) -> Result<(), String> {
    let target = target()?;
    let selection = AssistantSelection::new(
        AgentId::new("review-agent").ok_or("agent")?,
        target.model_ref().provider().clone(),
        if foreign {
            grapt_types::ModelId::new("other-model").ok_or("model")?
        } else {
            target.model_ref().model().clone()
        },
        None,
    );
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
    .map_err(|error| error.to_string())?
    .with_selection(selection)
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("review-reasoning-{step}")).ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    for (index, reason) in reasons.iter().enumerate() {
        let block = BlockOrdinal::new(u32::try_from(index).map_err(|error| error.to_string())?);
        for fragment in [
            AttemptFragment::ReasoningOpened {
                block,
                state: AdapterState::none(),
            },
            AttemptFragment::ReasoningDelta {
                block,
                text: (*reason).into(),
                state: AdapterState::none(),
            },
            AttemptFragment::ReasoningClosed {
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
    }
    let block = BlockOrdinal::new(u32::try_from(reasons.len()).map_err(|error| error.to_string())?);
    for fragment in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: format!("visible-{step}"),
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
        .map_err(|error| error.to_string())
}
fn enable(continuation: &DurableContinuation) -> Result<(), String> {
    continuation.select_target_with_effort(
        &target()?,
        ProtocolFamily::OpenAiCompatible,
        Default::default(),
    );
    continuation.select_compatible_reasoning(CompatibleReasoningReplay::ReasoningContent);
    Ok(())
}
fn reasons(input: &[CodexInputItem]) -> Vec<&str> {
    input
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::CompatibleReasoning(text) => Some(text.as_str()),
            _ => None,
        })
        .collect()
}
fn begin(fixture: &Fixture) -> Result<(), String> {
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "use the tool",
    )
}
#[test]
fn review_reasoning_restart_keeps_exact_blocks_and_omits_another_models_reasoning(
) -> Result<(), String> {
    let mut fixture = Fixture::new("compatible-replay-origin")?;
    begin(&fixture)?;
    record(&fixture, 1, false, &["first π", "second 中"])?;
    record(&fixture, 2, false, &["separate turn"])?;
    record(&fixture, 3, true, &["foreign reasoning must stay private"])?;
    let before = {
        let continuation = fixture.continuation(fixture.input.clone())?;
        enable(&continuation)?;
        let input = continuation.request_input("use the tool", false)?;
        assert_eq!(reasons(&input), ["first π", "second 中", "separate turn"]);
        input
    };
    fixture.close();
    let reopened = fixture.reopen(fixture.input.clone())?;
    enable(&reopened)?;
    assert_eq!(reopened.request_input("use the tool", false)?, before);
    Ok(())
}
#[test]
fn review_reasoning_required_missing_or_corrupt_artifact_refuses_without_fabrication(
) -> Result<(), String> {
    for missing in [true, false] {
        let mut fixture = Fixture::new("compatible-replay-integrity")?;
        begin(&fixture)?;
        record(&fixture, 1, false, &["private reasoning"])?;
        let reference = {
            let inbox = lock(fixture.inbox()?);
            let history = SessionHistoryStore::over(inbox.store())
                .fold(&fixture.session)
                .map_err(|error| error.to_string())?;
            history
                .messages()
                .map_err(|error| error.to_string())?
                .iter()
                .filter_map(|message| message.content.as_ref())
                .flat_map(|content| content.items())
                .find(|held| matches!(held.item.kind(), grapt_types::ContentKind::Reasoning))
                .ok_or("reasoning artifact")?
                .item
                .payload()
                .clone()
        };
        let identity = reference.id().as_str();
        let object = fixture
            .artifact_root
            .join("objects")
            .join(identity.get(..2).ok_or("shard")?)
            .join(identity);
        if missing {
            std::fs::remove_file(object).map_err(|error| error.to_string())?;
        } else {
            std::fs::write(object, b"corrupt reasoning").map_err(|error| error.to_string())?;
        }
        fixture.close();
        let reopened = fixture.reopen(fixture.input.clone())?;
        enable(&reopened)?;
        let error = reopened
            .request_input("use the tool", false)
            .err()
            .ok_or("required reasoning was fabricated or discarded")?;
        assert!(error.contains("compatible reasoning"), "{error}");
        reopened.select_compatible_reasoning(CompatibleReasoningReplay::None);
        assert!(reasons(&reopened.request_input("use the tool", false)?).is_empty());
    }
    Ok(())
}
#[test]
fn review_reasoning_required_oversized_or_truncated_capture_refuses() -> Result<(), String> {
    for size in [
        MAX_CODEX_TOOL_TEXT_BYTES + 1,
        usize::try_from(MAX_BLOCK_CAPTURE_BYTES).map_err(|error| error.to_string())? + 1,
    ] {
        let fixture = Fixture::new("compatible-replay-bound")?;
        begin(&fixture)?;
        record(&fixture, 1, false, &[&"r".repeat(size)])?;
        let continuation = fixture.continuation(fixture.input.clone())?;
        enable(&continuation)?;
        let error = continuation
            .request_input("use the tool", false)
            .err()
            .ok_or("required reasoning silently shortened")?;
        assert!(
            error.contains("compatible reasoning")
                && (error.contains("bound") || error.contains("truncated")),
            "{error}"
        );
    }
    Ok(())
}
