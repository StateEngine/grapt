//! A rejected native call retains its receipt through both history projections.
use super::*;

fn rejected(tool: &str, execution: ToolExecution, cause: &str) -> Result<Fixture, String> {
    let fixture = Fixture::new("msys-rejection")?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "run tool",
    )?;
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
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_msys_rejected").ok_or("attempt")?,
        })
        .map_err(|error| error.reason)?;
    let call = ToolCallId::new("call_msys_rejected").ok_or("call")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: tool.into(),
                input: r#"{"cmd":"bash -c true","msys_unconfined":true}"#.into(),
                execution,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.reason)?;
    if execution == ToolExecution::Local && matches!(tool, "exec_command" | "shell") {
        fixture.ask_permission_for(
            "evt_msys_rejected",
            "per_msys_rejected",
            &call,
            grapt_permission::SANDBOX_MSYS_ACTION,
        )?;
    }
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(cause.as_bytes()),
            CaptureBound::new(16_384, 16_384).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::SecretBearing,
        )
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::AuthorityUnavailable,
                detail: Some(
                    ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
                        .ok_or("detail")?,
                ),
            },
        })
        .map_err(|error| error.reason)?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.reason)?;
    drop(owner);
    Ok(fixture)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn rejected_msys_receipt_survives_public_and_recent_history_without_foreign_details(
) -> Result<(), String> {
    let rejection =
        "the permission request was rejected; do not repeat the call unless the decision changes";
    // The runner stores this bounded, redacted native failure envelope when
    // permission is rejected before the MSYS child starts.
    let receipt = serde_json::json!({
        "enforcement": {"kind":"degraded", "backend":"mxc", "reason":"MSYS requires explicit consent"},
        "ran":false,
        "error":format!("shell failed (execution)\nCause: {rejection}\nStderr tail: not observed\nNext: Check the reported effect before retrying; use exec_command for long-running work."),
    })
    .to_string();
    let oversized = format!("{receipt}{}", " ".repeat(2048));
    for (tool, execution, detail, visible) in [
        ("exec_command", ToolExecution::Local, receipt.as_str(), true),
        ("shell", ToolExecution::Local, receipt.as_str(), true),
        (
            "exec_command",
            ToolExecution::ProviderHosted,
            receipt.as_str(),
            false,
        ),
        (
            "mcp.exec_command",
            ToolExecution::Local,
            receipt.as_str(),
            false,
        ),
        (
            "exec_command",
            ToolExecution::Local,
            "PRIVATE failure artifact",
            false,
        ),
        (
            "exec_command",
            ToolExecution::Local,
            oversized.as_str(),
            false,
        ),
    ] {
        let fixture = rejected(tool, execution, detail)?;
        let projection = tool_projection(
            fixture.inbox()?,
            fixture.artifacts()?,
            &fixture.session,
            &ToolCallId::new("call_msys_rejected").ok_or("call")?,
        )?;
        let raw =
            crate::dogfood::history_json::tool_state_json(&projection, fixture.artifacts()?, true)?;
        serde_json::from_str::<grapt_api::u3a::SessionMessageToolStateErrorDto>(&raw)
            .map_err(|error| error.to_string())?;
        let public: serde_json::Value =
            serde_json::from_str(&raw).map_err(|error| error.to_string())?;
        if visible {
            let error = public["error"]["message"]
                .as_str()
                .ok_or("public receipt missing")?;
            let actual: serde_json::Value = serde_json::from_str(error)
                .map_err(|error| format!("public rejection receipt missing: {error}"))?;
            assert_eq!(actual["ran"], false);
            assert_eq!(actual["enforcement"]["kind"], "degraded");
            assert_eq!(actual["enforcement"]["backend"], "mxc");
            assert!(actual["error"]
                .as_str()
                .is_some_and(|text| text.contains(rejection)));
            assert_eq!(public["content"][0]["text"], error);
        } else {
            assert_eq!(
                public["error"]["message"],
                "Tool failed: AuthorityUnavailable"
            );
            assert!(public.get("content").is_none());
        }
        let inbox = lock(fixture.inbox()?);
        let window = BoundedWindow {
            from: None,
            until: None,
            max_rows: MAX_CONTINUATION_ROWS,
        };
        let calls = stored_calls_for(
            inbox.store(),
            &fixture.session,
            fixture.artifacts()?,
            window,
            None,
            None,
        )?;
        let expected = if visible {
            format!("Tool failed: AuthorityUnavailable\n{receipt}")
        } else {
            "Tool failed: AuthorityUnavailable".to_owned()
        };
        let mut budget = super::super::recent::TOOL_ERROR_LABEL.len() + expected.len() + 1;
        let outputs = stored_outputs_for(
            inbox.store(),
            &fixture.session,
            fixture.artifacts()?,
            window,
            None,
            Some((&mut budget, &calls)),
        )?;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].output.as_str(), expected);
        assert_eq!(budget, 0, "recent context must charge the complete receipt");
    }
    Ok(())
}
