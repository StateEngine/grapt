//! Durable writer-to-public projection, including hostile/legacy identities.
use super::*;

fn failed(
    label: &str,
    tool: &str,
    execution: ToolExecution,
    cause: Option<&str>,
) -> Result<Fixture, String> {
    let fixture = Fixture::new(label)?;
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
            attempt: PhysicalAttemptId::new("pat_codemode_failure").ok_or("attempt")?,
        })
        .map_err(|error| error.reason)?;
    let call = ToolCallId::new("call_codemode_failure").ok_or("call")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: tool.into(),
                input: r#"{"code":"await tools.echo({})"}"#.into(),
                execution,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.reason)?;
    let detail = if let Some(cause) = cause {
        let captured = fixture
            .artifacts()?
            .capture(
                Cursor::new(cause.as_bytes()),
                CaptureBound::new(16384, 16384).ok_or("bound")?,
                ArtifactKind::CapturedOutput,
                Sensitivity::Ordinary,
            )
            .map_err(|error| error.to_string())?;
        Some(
            ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
                .ok_or("detail")?,
        )
    } else {
        None
    };
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: grapt_types::ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::EffectFailed,
                detail,
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

fn public(fixture: &Fixture) -> Result<serde_json::Value, String> {
    let projection = tool_projection(
        fixture.inbox()?,
        fixture.artifacts()?,
        &fixture.session,
        &ToolCallId::new("call_codemode_failure").ok_or("call")?,
    )?;
    let raw =
        crate::dogfood::history_json::tool_state_json(&projection, fixture.artifacts()?, true)?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

#[test]
fn typed_codemode_causes_survive_public_history_and_model_reopen_without_secrets(
) -> Result<(), String> {
    let secret = "PRIVATE_SCHEMA_TOKEN秘密".repeat(200);
    let cause = crate::dogfood::tools::codemode_failure::capture(
        grapt_codemode::GuestFailure::GuestError,
        Some(grapt_codemode::HostFailure::InvalidArguments),
        Some(&secret),
    );
    let mut fixture = failed(
        "codemode-safe",
        "execute",
        ToolExecution::Local,
        Some(&cause),
    )?;
    let value = public(&fixture)?;
    let message = value["error"]["message"]
        .as_str()
        .ok_or("safe public cause missing")?;
    assert!(message.contains("[invalid_arguments]"));
    assert!(!value.to_string().contains("PRIVATE_SCHEMA_TOKEN"));
    let expected = fixture
        .continuation(fixture.input.clone())?
        .request_input("run tool", true)?;
    let errors = expected
        .iter()
        .filter_map(|item| match item {
            CodexInputItem::ToolOutput { output, .. } => Some(output.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(errors.len(), 1);
    assert!(errors[0].contains("[invalid_arguments]"));
    assert!(!errors[0].contains("PRIVATE_SCHEMA_TOKEN"));
    fixture.close();
    assert_eq!(
        fixture
            .reopen(fixture.input.clone())?
            .request_input("run tool", true)?,
        expected
    );
    Ok(())
}

#[test]
fn foreign_codemode_lookalikes_keep_the_generic_public_failure() -> Result<(), String> {
    let cause = crate::dogfood::tools::codemode_failure::capture(
        grapt_codemode::GuestFailure::GuestError,
        Some(grapt_codemode::HostFailure::AuthorityUnavailable),
        Some("PRIVATE"),
    );
    for (label, tool, execution) in [
        ("codemode-hosted", "execute", ToolExecution::ProviderHosted),
        ("codemode-foreign", "foreign_execute", ToolExecution::Local),
    ] {
        let fixture = failed(label, tool, execution, Some(&cause))?;
        let value = public(&fixture)?;
        assert_eq!(value["error"]["message"], "Tool failed: EffectFailed");
        assert!(!value.to_string().contains("permission_refused"));
        assert!(!value.to_string().contains("PRIVATE"));
    }
    Ok(())
}

#[test]
fn codemode_failure_budget_charges_exact_framing_with_and_without_artifacts() -> Result<(), String>
{
    let capture = crate::dogfood::tools::codemode_failure::capture(
        grapt_codemode::GuestFailure::GuestError,
        Some(grapt_codemode::HostFailure::InvalidArguments),
        Some("PRIVATE"),
    );
    for (label, detail) in [
        ("codemode-budget-detail", Some(capture.as_str())),
        ("codemode-budget-absent", None),
    ] {
        let fixture = failed(label, "execute", ToolExecution::Local, detail)?;
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
        let cause = crate::dogfood::tools::codemode_failure::public(
            grapt_types::ToolFailureReason::EffectFailed,
            detail.unwrap_or(""),
        )
        .ok_or("cause missing")?;
        let expected = format!("Tool failed: EffectFailed\n{cause}");
        let exact = super::super::recent::TOOL_ERROR_LABEL.len() + expected.len() + 1;
        let mut fitting = exact;
        let outputs = stored_outputs_for(
            inbox.store(),
            &fixture.session,
            fixture.artifacts()?,
            window,
            None,
            Some((&mut fitting, &calls)),
        )?;
        assert_eq!(outputs.len(), 1);
        assert_eq!(outputs[0].output.as_str(), expected);
        assert_eq!(fitting, 0, "materialized framing was not fully charged");
        let mut short = exact - 1;
        assert!(matches!(
            stored_outputs_for(
                inbox.store(),
                &fixture.session,
                fixture.artifacts()?,
                window,
                None,
                Some((&mut short, &calls))
            ),
            Err(super::super::recent::WindowError::Capacity(_))
        ));
    }
    Ok(())
}

#[test]
fn a_corrupt_codemode_cause_keeps_the_generic_failure() -> Result<(), String> {
    let cause = crate::dogfood::tools::codemode_failure::capture(
        grapt_codemode::GuestFailure::GuestError,
        Some(grapt_codemode::HostFailure::InvalidArguments),
        Some("PRIVATE"),
    );
    let fixture = failed(
        "codemode-corrupt",
        "execute",
        ToolExecution::Local,
        Some(&cause),
    )?;
    super::corrupt_outputs::corrupt_terminal_artifacts(&fixture)?;
    let value = public(&fixture)?;
    assert!(
        !value.to_string().contains("[invalid_arguments]"),
        "{value}"
    );
    fixture
        .continuation(fixture.input.clone())?
        .request_input("run tool", true)?;
    Ok(())
}
