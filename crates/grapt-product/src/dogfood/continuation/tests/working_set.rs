//! Working-set retention carries exact recent outputs through a real cut/reopen.
use super::sustained_defects::step_owner;
use super::*;
use grapt_types::CompactionId;

pub(super) fn output_step(fixture: &Fixture, step: u32, text: &str) -> Result<(), String> {
    let mut owner = step_owner(fixture, step)?;
    let call = ToolCallId::new(format!("working-{step}")).ok_or("call")?;
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
        .map_err(|error| error.to_string())?;
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(text.as_bytes()),
            CaptureBound::new(64 * 1024, 4096).ok_or("output capture bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| error.to_string())?;
    let output = ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        .ok_or("output artifact")?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Completed {
                output: Some(output),
            },
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())
}

fn output<'a>(items: &'a [CodexInputItem], name: &str) -> Option<&'a str> {
    items.iter().find_map(|item| match item {
        CodexInputItem::ToolOutput { call, output, .. } if call.as_str() == name => {
            Some(output.as_str())
        }
        _ => None,
    })
}

#[test]
fn multiple_recent_outputs_and_current_authority_survive_compaction_and_reopen(
) -> Result<(), String> {
    let mut fixture = Fixture::new("working-set-reopen")?;
    // Exceed the tail budget so older work really belongs in the summary.
    fixture
        .record_exchange_output(
            true,
            false,
            false,
            READ_TOOL,
            "read",
            &b"fixture output ".repeat(2000),
        )?
        .ok_or("older fixture output missing")?;
    output_step(&fixture, 2, "parser accepts exactly tenant_key")?;
    let current: MessageId = "msg_working_current"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &current,
        "now fix the serializer",
    )?;
    output_step(&fixture, 3, "serializer emits tenant_key and value")?;
    let continuation = fixture.continuation(current.clone())?;
    let id = CompactionId::new("working-set-reopen").ok_or("compaction")?;
    let selected = continuation.prepare_summary("now fix the serializer", &id, 128_000)?;
    assert_eq!(
        output(&selected.retained, "working-2"),
        Some("parser accepts exactly tenant_key")
    );
    assert_eq!(
        output(&selected.retained, "working-3"),
        Some("serializer emits tenant_key and value")
    );
    assert!(
        selected.text.contains("fixture output"),
        "older work was not summarized"
    );
    assert!(output(&selected.retained, "call_d2a_read").is_none());
    super::summary_tests::commit_selection(&fixture, &selected, id)?;
    let before = continuation.request_input("now fix the serializer", false)?;
    assert!(before.iter().any(|item| matches!(item,
        CodexInputItem::Message(message) if message.role == MessageRole::User
            && message.content == "now fix the serializer")));
    drop(continuation);
    fixture.close();
    let reopened = fixture.reopen(current)?;
    let after = reopened.request_input("now fix the serializer", false)?;
    assert_eq!(after, before);
    assert_eq!(
        output(&after, "working-2"),
        Some("parser accepts exactly tenant_key")
    );
    assert_eq!(
        output(&after, "working-3"),
        Some("serializer emits tenant_key and value")
    );
    Ok(())
}

#[test]
fn an_older_large_output_is_summarized_without_splitting_the_recent_exchange() -> Result<(), String>
{
    let fixture = Fixture::new("working-set-budget")?;
    fixture.record_exchange()?;
    output_step(&fixture, 2, &"older output ".repeat(1000))?;
    output_step(&fixture, 3, "newest exact fact")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let id = CompactionId::new("working-set-budget").ok_or("compaction")?;
    let selected = continuation.prepare_summary("use the tool", &id, 128_000)?;
    assert!(output(&selected.retained, "working-2").is_none());
    assert_eq!(
        output(&selected.retained, "working-3"),
        Some("newest exact fact")
    );
    assert!(selected.retained.iter().any(|item| matches!(item,
        CodexInputItem::ToolCall { call, .. } if call.as_str() == "working-3")));
    assert!(selected.retained_bytes <= 8_000);
    Ok(())
}

#[test]
fn consecutive_working_sets_advance_the_committed_cut() -> Result<(), String> {
    let fixture = Fixture::new("working-set-advance")?;
    fixture.record_exchange()?;
    output_step(&fixture, 2, "first recent fact")?;
    output_step(&fixture, 3, "second recent fact")?;
    let continuation = fixture.continuation(fixture.input.clone())?;
    let first_id = CompactionId::new("working-set-first").ok_or("compaction")?;
    let first = continuation.prepare_summary("use the tool", &first_id, 128_000)?;
    let first_cut = first.selection.from;
    super::summary_tests::commit_selection(&fixture, &first, first_id)?;
    output_step(&fixture, 4, "third recent fact")?;
    let second_id = CompactionId::new("working-set-second").ok_or("compaction")?;
    let second = continuation.prepare_summary("use the tool", &second_id, 128_000)?;
    assert!(
        second.selection.from > first_cut,
        "retaining recent work must still advance the cut"
    );
    assert!(output(&second.retained, "working-2").is_none());
    assert_eq!(
        output(&second.retained, "working-3"),
        Some("second recent fact")
    );
    assert_eq!(
        output(&second.retained, "working-4"),
        Some("third recent fact")
    );
    Ok(())
}

#[test]
fn a_short_previous_user_input_does_not_hide_the_only_removable_exchange() -> Result<(), String> {
    let fixture = Fixture::new("working-set-real-prefix")?;
    fixture
        .record_exchange_output(true, false, false, READ_TOOL, "read", &b"s".repeat(7524))?
        .ok_or("older output missing")?;
    output_step(&fixture, 2, "seeded")?;
    let current: MessageId = "msg_working_next"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(fixture.inbox()?, &fixture.session, &current, "continue")?;
    let continuation = fixture.continuation(current)?;
    let id = CompactionId::new("working-set-real-prefix").ok_or("compaction")?;
    let selected = continuation.prepare_summary("continue", &id, 128_000)?;
    assert!(selected.removed_bytes >= 7524);
    assert!(output(&selected.retained, "call_d2a_read").is_none());
    assert_eq!(output(&selected.retained, "working-2"), Some("seeded"));
    Ok(())
}
