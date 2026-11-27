use super::*;
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug, Default)]
struct Fixed(AtomicU64);
impl ClockAuthority for Fixed {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1234)
    }
}
impl IdAuthority for Fixed {
    fn mint(&self) -> grapt_types::IdToken {
        grapt_types::IdToken::new(format!(
            "summarytest{}",
            self.0.fetch_add(1, Ordering::Relaxed)
        ))
        .unwrap_or_else(|| panic!("non-empty identity"))
    }
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn rejected_summary_retains_bounded_content_and_reported_terminal_usage() -> Result<(), String> {
    use grapt_provider::{AdapterState, BlockOrdinal, FinishReason};
    use grapt_types::{TokenCount, UsageField};
    let (continuation, _scratch) = DurableContinuation::empty_for_test("summary-rejection")?;
    let fixed = Arc::new(Fixed::default());
    let mut owner = SummaryOwner {
        inbox: continuation.inbox.clone(),
        artifacts: continuation.artifacts.clone(),
        session: continuation.session,
        ids: fixed.clone(),
        clock: fixed,
        compaction: CompactionId::new("summary-rejection").ok_or("compaction")?,
        step: StepNumber::new(1).ok_or("step")?,
        text: String::new(),
        coordinates: None,
        compaction_states: Vec::new(),
        tokens: None,
        other_items: 0,
        rejection: None,
        rejected_fragments: 0,
    };
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: grapt_types::PhysicalAttemptId::new("summary-attempt").ok_or("attempt")?,
        })
        .map_err(|e| e.reason)?;
    for _ in 0..=super::compaction::MAX_COMPACTION_ITEMS {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ReasoningClosed {
                    block: BlockOrdinal::new(0),
                    state: AdapterState::retain("{\"kind\":\"openai-compaction-v1\"}")
                        .map_err(|e| format!("{e:?}"))?,
                }),
            })
            .map_err(|e| e.reason)?;
    }
    assert_eq!(owner.rejection, Some(Rejection::TooManyItems));
    assert_eq!(
        owner.compaction_states.len(),
        super::compaction::MAX_COMPACTION_ITEMS
    );
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::TextDelta {
                block: BlockOrdinal::new(0),
                text: "ignored".repeat(12_000),
                state: AdapterState::none(),
            }),
        })
        .map_err(|e| e.reason)?;
    assert!(owner.text.is_empty());
    let field = |count| {
        TokenCount::new(count)
            .map(UsageField::reported)
            .map_err(|e| e.to_string())
    };
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ProviderTerminal {
                finish: FinishReason::Stop,
                state: AdapterState::none(),
                usage: Some(TokenUsage::new(
                    field(10_000)?,
                    field(500)?,
                    field(100)?,
                    field(0)?,
                    field(9_500)?,
                    field(0)?,
                    field(10_100)?,
                )),
            }),
        })
        .map_err(|e| e.reason)?;
    owner
        .commit(DurableFact::AttemptEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|e| e.reason)?;
    let inbox = lock(&continuation.inbox);
    let raw: String = inbox
        .store()
        .connection()
        .query_row(
            "SELECT data FROM event WHERE type='session.compaction.call.usage.1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let fact = grapt_event::session_message::decode_observed(
        &serde_json::from_str(&raw).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    assert_eq!(fact.usage().phase(), UsageObservationPhase::Terminal);
    assert_eq!(fact.usage().cache_read_input_tokens().value(), Some(9_500));
    let ended: String = inbox
        .store()
        .connection()
        .query_row(
            "SELECT data FROM event WHERE type='session.compaction.call.ended.1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| e.to_string())?;
    let (_, outcome) =
        compaction_call::decode_ended(&serde_json::from_str(&ended).map_err(|e| e.to_string())?)?;
    assert_eq!(outcome, CallEnd::Failed);
    Ok(())
}
