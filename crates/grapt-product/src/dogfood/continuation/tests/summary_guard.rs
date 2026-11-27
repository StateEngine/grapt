//! A summary request reads as the oracle's (buildPrompt): its task, a
//! previous summary as an anchor, the template, the history as quoted data,
//! then the task again.

use super::*;
use crate::dogfood::continuation::summarization::{
    warm_summary_prompt, warm_task_item, HISTORY_CLOSES, SUMMARY_TEMPLATE,
};

/// Commit one assistant text Step to the fixture's Session.
fn text_step(fixture: &Fixture, step: u32, text: String) -> Result<(), String> {
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        fixture.inbox()?.clone(),
        open_artifacts(&fixture.artifact_root)?,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(100 * u64::from(step)))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("framing-{step}")).ok_or("attempt")?,
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
            text,
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

#[test]
fn a_summary_request_frames_the_history_as_data_and_ends_with_its_task() -> Result<(), String> {
    let fixture = Fixture::new("summary-framing")?;
    fixture.record_exchange()?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let first = grapt_types::CompactionId::new("framing-first").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &first, 128_000)?;
    assert!(
        !selected.text.contains("<previous-summary>"),
        "a first summary has no predecessor: {}",
        selected.text
    );
    assert!(selected.text.starts_with("Create a new anchored summary"));
    super::summary_tests::commit_selection(&fixture, &selected, first)?;

    // A second epoch of history after the first summary; the newest Step
    // stays verbatim as the exact tail, the one before it is summarized.
    // Keep this framing witness above the exact working-set budget.
    text_step(&fixture, 2, "Observed work. ".repeat(3000))?;
    text_step(&fixture, 3, "Tail answer.".to_owned())?;
    // A later instruction, so the earlier turns are history to summarize.
    let next: MessageId = "msg_framing_next"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &next, "now finish")?;
    let continuation = fixture.continuation(next)?;

    let second = grapt_types::CompactionId::new("framing-second").ok_or("compaction")?;
    let selected = continuation.prepare_summary("now finish", &second, 128_000)?;
    let text = &selected.text;
    // The predecessor is an anchor to update, ahead of the history.
    assert!(
        text.starts_with("Update the anchored summary below"),
        "{text}"
    );
    assert!(text.contains("<previous-summary>\nUse tool.\n</previous-summary>"));
    assert!(text.contains(SUMMARY_TEMPLATE));
    // The history is quoted data: neither the predecessor nor the current
    // instruction is a turn in it.
    let items = super::summary_tests::summary_history(text)?;
    assert!(items.iter().any(|item| item["content"]
        .as_str()
        .is_some_and(|content| content.contains("Observed work."))));
    assert!(!items.iter().any(|item| item["content"] == "now finish"
        || item["content"]
            .as_str()
            .is_some_and(|content| content.contains("Use tool."))));
    assert!(text.contains("<latest-instruction>\nnow finish\n</latest-instruction>"));
    // The last thing the summarizer reads is its own task.
    assert!(text.ends_with(HISTORY_CLOSES), "{text}");
    Ok(())
}

/// The cache-warm task must read as the summarizer's instruction, not as a
/// user turn. J06A r4/r5, 4/4: the old clause, "the last user message above,
/// ... record what it means for the work", resolved to the prompt itself,
/// which was the request's last user message, and was recorded as a
/// stop/handoff directive in every summary it produced.
/// A mutant restoring that clause turns this test red.
#[test]
fn the_warm_summary_task_reads_as_meta_instruction() {
    let prompt = warm_summary_prompt();
    for required in [
        "an instruction to you, the summarizer",
        "not part of the conversation",
        "not a user instruction",
        "must not be recorded",
        "the user message immediately before this one",
        "must never describe preparing",
    ] {
        assert!(
            prompt.contains(required),
            "the warm prompt lost its required clause: {required:?}\n{prompt}"
        );
    }
    assert!(prompt.contains(SUMMARY_TEMPLATE), "{prompt}");
    assert!(
        !prompt.contains("the last user message above"),
        "the self-referential clause returned:\n{prompt}"
    );
    assert!(
        !prompt.contains("record what it means for the work"),
        "the directive to record its own clause returned:\n{prompt}"
    );
}

#[test]
fn the_warm_request_appends_the_task_as_one_user_item() {
    let CodexInputItem::Message(item) = warm_task_item() else {
        panic!("the warm task is not a message item");
    };
    assert_eq!(item.role, MessageRole::User);
    assert_eq!(item.content, warm_summary_prompt());
}
