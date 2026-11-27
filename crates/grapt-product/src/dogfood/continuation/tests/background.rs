use super::*;
use crate::dogfood::tools::TerminalRegistry;

fn report(done: bool) -> Vec<u8> {
    serde_json::json!({
        "session_id":"op_restart_command", "status":if done {"exited"} else {"running"},
        "done":done, "output":"retained output", "output_cursor":15,
        "recovery":{"started_ms":123456,"host":"local/linux/fixture-host"},
    })
    .to_string()
    .into_bytes()
}

#[test]
fn detached_work_is_recovered_after_database_and_artifact_reopen_without_replay(
) -> Result<(), String> {
    let mut fixture = Fixture::new("background-restart")?;
    fixture.record_exchange_call(
        (true, false, false),
        "exec_command",
        "shell",
        &report(false),
        r#"{"cmd":"never replay this command"}"#,
    )?;
    let input = fixture.input.clone();
    fixture.close();
    let continuation = fixture.reopen(input)?;
    let live = TerminalRegistry::default();
    let note = super::super::background::notice(
        &continuation.inbox,
        &continuation.artifacts,
        &continuation.session,
        &live,
    )?
    .ok_or("recovery notice missing")?;
    for expected in [
        "previously started",
        "status unknown",
        "fixture-host",
        "123456",
        "last observed \"running\"",
        "history({\"call\":\"call_d2a_read\"})",
        "Never replay",
    ] {
        assert!(note.contains(expected), "missing {expected}: {note}");
    }
    assert!(
        !note.contains("never replay this command"),
        "raw command leaked into recovery"
    );
    let recovered = continuation.with_background_recovery(Some(note.clone()));
    let (items, _, _) = recovered
        .share()
        .request_with_instructions("use the tool", false, &[])?;
    assert!(
        matches!(items.first(), Some(CodexInputItem::Message(InputMessage { role: MessageRole::System, content })) if content == &note)
    );
    let foreign = "ses_recovery_foreign".parse().map_err(|_| "session")?;
    assert!(super::super::background::notice(
        &recovered.inbox,
        &recovered.artifacts,
        &foreign,
        &live
    )?
    .is_none());
    Ok(())
}

#[test]
fn final_observations_and_server_tool_impostors_do_not_become_background_work() -> Result<(), String>
{
    for (execution, tool, action, done) in [
        (ToolExecution::Local, "exec_command", "shell", true),
        (
            ToolExecution::ProviderHosted,
            "exec_command",
            "shell",
            false,
        ),
        (ToolExecution::Local, "read", "read", false),
        (ToolExecution::Local, "exec_command", "mcp.remote", false),
    ] {
        let fixture = Fixture::new("background-provenance")?;
        fixture.record_owned_exchange_call(
            (true, false, false),
            tool,
            action,
            &report(done),
            "{}",
            execution,
        )?;
        let actual = super::super::background::notice(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &TerminalRegistry::default(),
        );
        if action == "mcp.remote" {
            let error = actual
                .err()
                .ok_or("contradictory permission provenance was not refused")?;
            assert!(error.contains("tool identity conflicts with its permission action"));
        } else {
            assert!(actual?.is_none());
        }
    }
    Ok(())
}

#[test]
fn unrelated_reads_keep_recovery_and_a_later_final_poll_removes_it() -> Result<(), String> {
    let fixture = Fixture::new("background-latest")?;
    fixture.record_exchange_call(
        (true, false, false),
        "exec_command",
        "shell",
        &report(false),
        "{}",
    )?;
    // Source reads do not count toward the native terminal recovery window.
    super::sustained_defects::settled_step(&fixture, 2, 70, "later-read")?;
    let registry = TerminalRegistry::default();
    assert!(super::super::background::notice(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        &registry
    )?
    .is_some());
    let mut owner = super::sustained_defects::step_owner(&fixture, 3)?;
    let call = ToolCallId::new("final-poll").ok_or("call")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: "write_stdin".into(),
                input: r#"{"session_id":"op_restart_command"}"#.into(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(report(true)),
            CaptureBound::new(MAX_BLOCK_CAPTURE_BYTES, 64 * 1024).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| error.to_string())?;
    let output = ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        .ok_or("output")?;
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
        .map_err(|error| error.to_string())?;
    assert!(
        super::super::background::notice(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &registry
        )?
        .is_none(),
        "an older running observation resurrected a final command"
    );
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn background_nested_terminal_results_survive_a_silent_execute_parent_and_remain_owner_only(
) -> Result<(), String> {
    for tool in ["exec_command", "mcp.remote_exec"] {
        let mut fixture = Fixture::new("background-nested")?;
        promote_test_input(
            fixture.inbox()?,
            &fixture.session,
            &fixture.input,
            "use the tool",
        )?;
        let mut owner = super::sustained_defects::step_owner(&fixture, 1)?;
        let parent = ToolCallId::new("execute-parent").ok_or("parent")?;
        let call = ToolCallId::new("terminal-child").ok_or("call")?;
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(AttemptFragment::ToolCalled {
                    call: parent.clone(),
                    tool: "execute".into(),
                    input: "{}".into(),
                    execution: ToolExecution::Local,
                    state: AdapterState::none(),
                }),
            })
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::NestedToolCalled {
                parent: parent.clone(),
                call: call.clone(),
                tool: grapt_types::ToolDefinitionId::new(tool).ok_or("tool")?,
                catalog: grapt_types::ToolSchemaVersion::new("catalog-recovery")
                    .ok_or("catalog")?,
                input: grapt_runtime::BoundedToolInput::new(b"{}".to_vec())
                    .map_err(|error| error.to_string())?,
            })
            .map_err(|error| error.to_string())?;
        let captured = fixture
            .artifacts()?
            .capture(
                Cursor::new(report(false)),
                CaptureBound::new(MAX_BLOCK_CAPTURE_BYTES, 64 * 1024).ok_or("bound")?,
                ArtifactKind::CapturedOutput,
                Sensitivity::SecretBearing,
            )
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call: call.clone(),
                outcome: ToolTerminalOutcome::Completed {
                    output: Some(
                        ToolTerminalArtifact::new(
                            captured.reference().clone(),
                            captured.disposition(),
                        )
                        .ok_or("output")?,
                    ),
                },
            })
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::ToolTerminal {
                call: parent,
                outcome: ToolTerminalOutcome::Completed { output: None },
            })
            .map_err(|error| error.to_string())?;
        owner
            .commit(DurableFact::StepEnded {
                finish: FinishReason::ToolCalls,
            })
            .map_err(|error| error.to_string())?;
        drop(owner);
        let input = fixture.input.clone();
        fixture.close();
        let reopened = fixture.reopen(input)?;
        let note = super::super::background::notice(
            &reopened.inbox,
            &reopened.artifacts,
            &reopened.session,
            &TerminalRegistry::default(),
        )?;
        let result = history(
            &reopened.inbox,
            &reopened.artifacts,
            &reopened.session,
            Some(&call),
            None,
            None,
        );
        if tool == "exec_command" {
            assert!(
                note.is_some_and(|note| note.contains("history({\"call\":\"terminal-child\"})"))
            );
            let result: serde_json::Value =
                serde_json::from_slice(&result?).map_err(|error| error.to_string())?;
            assert_eq!(
                result["content"],
                String::from_utf8(report(false)).map_err(|error| error.to_string())?
            );
            let foreign = "ses_background_foreign".parse().map_err(|_| "session")?;
            assert!(history(
                &reopened.inbox,
                &reopened.artifacts,
                &foreign,
                Some(&call),
                None,
                None
            )
            .is_err());
        } else {
            assert!(note.is_none());
            assert!(
                result.is_err(),
                "unrelated nested tool became a terminal recovery reader"
            );
        }
    }
    Ok(())
}
