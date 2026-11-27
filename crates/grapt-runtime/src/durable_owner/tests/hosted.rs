use super::*;

#[test]
fn hosted_identity_and_captured_terminal_survive_a_fresh_history_read(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("hosted-persistence");
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
    let call = ToolCallId::new("hosted-call").ok_or("call")?;
    owner.commit(DurableFact::AttemptStarted {
        attempt: attempt("hosted-attempt"),
    })?;
    owner.commit(DurableFact::Fragment {
        fragment: Box::new(AttemptFragment::ToolCalled {
            call: call.clone(),
            tool: "web_search".into(),
            input: r#"{"query":"fixture"}"#.into(),
            execution: grapt_types::ToolExecution::ProviderHosted,
            state: AdapterState::none(),
        }),
    })?;
    let captured = owner.capture_hosted_output("bounded hosted result")?;
    owner.commit(DurableFact::ToolTerminal {
        call: call.clone(),
        outcome: ToolTerminalOutcome::Completed {
            output: Some(captured.clone()),
        },
    })?;
    drop(owner);
    let rows = rows(&store);
    let (_, _, identity) = rows
        .iter()
        .find(|(_, kind, _)| kind == "session.tool.input.started.2")
        .ok_or("hosted identity missing")?;
    let identity = grapt_event::session_tool_identity::decode(2, identity)?;
    assert_eq!(identity.call, call);
    assert_eq!(
        identity.execution,
        grapt_types::ToolExecution::ProviderHosted
    );
    assert!(!rows
        .iter()
        .any(|(_, kind, _)| kind == "session.tool.input.started.1"));
    let (_, _, terminal) = rows
        .iter()
        .find(|(_, kind, _)| kind == "session.tool.terminal.1")
        .ok_or("hosted terminal missing")?;
    let terminal = grapt_event::session_tool_terminal::decode(terminal)?;
    assert_eq!(
        terminal.outcome(),
        &ToolTerminalOutcome::Completed {
            output: Some(captured.clone())
        }
    );
    let artifacts = scratch.artifacts();
    let bytes = artifacts.fetch(
        captured.reference(),
        0,
        grapt_artifact::ReadBudget::new(65536).ok_or("read budget")?,
    )?;
    assert_eq!(bytes.bytes(), b"bounded hosted result");
    Ok(())
}

#[test]
fn hosted_output_capture_keeps_its_actual_truncation_disposition(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new("hosted-overflow");
    let store = store();
    let clock = FrozenClock;
    let ids = CountingIds::default();
    let mut owner = DurableAttemptOwner::elect(
        store,
        scratch.artifacts(),
        &clock,
        &ids,
        RecordingSink::default(),
        session(),
        step(1),
        None,
    )?;
    let output =
        owner.capture_hosted_output(&"x".repeat(crate::TOOL_ARTIFACT_MAX_BYTES as usize + 1))?;
    assert_eq!(output.reference().length(), crate::TOOL_ARTIFACT_MAX_BYTES);
    assert_eq!(
        output.disposition(),
        CaptureDisposition::TruncatedAtBoundary {
            captured: crate::TOOL_ARTIFACT_MAX_BYTES
        }
    );
    Ok(())
}
