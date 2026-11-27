use super::*;
use grapt_event::session_tool_nested;
use grapt_types::{ToolDefinitionId, ToolFailureReason, ToolSchemaVersion};

#[test]
fn nested_input_is_durable_but_not_model_content() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("nested-private-input");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = DurableAttemptOwner::elect(
        Arc::clone(&store),
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    )?;
    let parent = ToolCallId::new("parent-execute").ok_or("parent")?;
    let child = ToolCallId::new("nested-child").ok_or("child")?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("pat_nested"),
    })?;
    let before = rows(&store).len();
    owner.commit(DurableFact::NestedToolCalled {
        parent,
        call: child.clone(),
        tool: ToolDefinitionId::new("private-tool").ok_or("tool")?,
        catalog: ToolSchemaVersion::new("catalog-1").ok_or("catalog")?,
        input: crate::BoundedToolInput::new(br#"{"secret":"nested-only"}"#.to_vec())?,
    })?;
    owner.commit(DurableFact::ToolTerminal {
        call: child,
        outcome: ToolTerminalOutcome::Failed {
            reason: ToolFailureReason::EffectUnknown,
            detail: None,
        },
    })?;
    let persisted = rows(&store);
    let added = &persisted[before..];
    assert_eq!(added.len(), 2);
    assert_eq!(added[0].1, "session.tool.nested.started.1");
    session_tool_nested::decode(&added[0].2)?;
    assert_eq!(added[1].1, "session.tool.terminal.1");
    assert!(!serde_json::to_string(&added)?.contains("nested-only"));
    let folded = history(&store);
    let messages = folded.messages()?;
    assert!(messages.iter().all(|message| message
        .content
        .as_ref()
        .is_none_or(|content| content.is_empty())));
    Ok(())
}
