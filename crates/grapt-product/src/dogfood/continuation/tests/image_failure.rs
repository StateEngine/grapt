//! Durable writer-to-public projection, including hostile/legacy identities.
use super::recent::raw_conn;
use super::*;

fn failed(
    label: &str,
    tool: &str,
    execution: ToolExecution,
    cause: &str,
) -> Result<Fixture, String> {
    let fixture = Fixture::new(label)?;
    promote_test_input(
        fixture.inbox()?,
        &fixture.session,
        &fixture.input,
        "view image",
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
            attempt: PhysicalAttemptId::new("pat_image_failure").ok_or("attempt")?,
        })
        .map_err(|error| error.reason)?;
    let call = ToolCallId::new("call_image_failure").ok_or("call")?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: call.clone(),
                tool: tool.into(),
                input: r#"{"path":"image.png"}"#.into(),
                execution,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.reason)?;
    let captured = fixture
        .artifacts()?
        .capture(
            Cursor::new(cause.as_bytes()),
            CaptureBound::new(16384, 16384).ok_or("bound")?,
            ArtifactKind::CapturedOutput,
            Sensitivity::Ordinary,
        )
        .map_err(|error| error.to_string())?;
    let detail = ToolTerminalArtifact::new(captured.reference().clone(), captured.disposition())
        .ok_or("detail")?;
    owner
        .commit(DurableFact::ToolTerminal {
            call,
            outcome: grapt_types::ToolTerminalOutcome::Failed {
                reason: grapt_types::ToolFailureReason::EffectFailed,
                detail: Some(detail),
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
        &ToolCallId::new("call_image_failure").ok_or("call")?,
    )?;
    let raw =
        crate::dogfood::history_json::tool_state_json(&projection, fixture.artifacts()?, true)?;
    serde_json::from_str(&raw).map_err(|error| error.to_string())
}

#[test]
fn native_image_causes_are_safe_without_permission_records() -> Result<(), String> {
    let cause = format!(
        "image decoder refused input: {}",
        "PRIVATE_PIXELS秘密".repeat(512)
    );
    let fixture = failed(
        "native-image-safe",
        "view_image",
        ToolExecution::Local,
        &cause,
    )?;
    let value = public(&fixture)?;
    let text = value["content"][0]["text"]
        .as_str()
        .ok_or("public native cause missing")?;
    assert!(text.contains("image decoder refused"), "{value}");
    assert!(text.len() <= 2048);
    assert!(!value.to_string().contains("PRIVATE_PIXELS"));
    assert!(!value.to_string().contains("秘密"));
    assert_eq!(value["metadata"]["publicBoundBytes"], 2048);
    Ok(())
}

#[test]
fn legacy_foreign_and_hosted_image_lookalikes_do_not_disclose_failure_details() -> Result<(), String>
{
    for (name, execution, legacy) in [
        ("mcp.foreign.view_image", ToolExecution::Local, false),
        ("view_image", ToolExecution::ProviderHosted, false),
        ("view_image", ToolExecution::Local, true),
    ] {
        let fixture = failed(
            "image-foreign-detail",
            name,
            execution,
            "image decoder refused input: PRIVATE_RECEIPT secode-image-v1 BASE64_PIXELS",
        )?;
        if legacy {
            let conn = raw_conn(&fixture)?;
            conn.execute("UPDATE event SET type='session.tool.input.started.1', data=json_remove(data,'$.execution') WHERE type='session.tool.input.started.2'", []).map_err(|error| error.to_string())?;
        }
        let value = public(&fixture)?;
        assert_eq!(value["metadata"]["publicProjection"], "excluded", "{value}");
        assert!(value.get("content").is_none(), "{value}");
        for secret in ["PRIVATE_RECEIPT", "secode-image-v1", "BASE64_PIXELS"] {
            assert!(!value.to_string().contains(secret));
        }
    }
    Ok(())
}

#[test]
fn missing_corrupt_and_contradictory_image_provenance_cannot_become_native() -> Result<(), String> {
    for mutation in ["missing", "executor", "unknown", "permission"] {
        let fixture = failed(
            "image-invalid-provenance",
            "view_image",
            ToolExecution::Local,
            "PRIVATE_IMAGE_CAUSE",
        )?;
        if mutation == "permission" {
            fixture.ask_permission_for(
                "evt_image_wrong_permission",
                "per_image_wrong_permission",
                &ToolCallId::new("call_image_failure").ok_or("call")?,
                "view_image",
            )?;
            assert_eq!(
                public(&fixture)?["metadata"]["publicProjection"],
                "excluded"
            );
            continue;
        }
        let conn = raw_conn(&fixture)?;
        match mutation {
            "missing" => {
                conn.execute(
                    "DELETE FROM event WHERE type='session.tool.input.started.2'",
                    [],
                )
                .map_err(|error| error.to_string())?;
            }
            "executor" => {
                conn.execute("UPDATE event SET data=json_remove(data,'$.execution') WHERE type='session.tool.input.started.2'", []).map_err(|error| error.to_string())?;
            }
            _ => {
                conn.execute("UPDATE event SET data=json_set(data,'$.execution','unknown') WHERE type='session.tool.input.started.2'", []).map_err(|error| error.to_string())?;
            }
        }
        drop(conn);
        if mutation == "missing" {
            let value = public(&fixture)?;
            assert_eq!(value["metadata"]["publicProjection"], "excluded");
            assert!(!value.to_string().contains("PRIVATE_IMAGE_CAUSE"));
        } else {
            assert!(public(&fixture).is_err(), "corrupt executor became local");
        }
    }
    Ok(())
}

#[test]
fn legacy_native_image_with_a_recorded_read_still_has_safe_detail() -> Result<(), String> {
    let fixture = failed(
        "image-legacy-read",
        "view_image",
        ToolExecution::Local,
        "image container is truncated or corrupt",
    )?;
    fixture.ask_permission_for(
        "evt_image_legacy_read",
        "per_image_legacy_read",
        &ToolCallId::new("call_image_failure").ok_or("call")?,
        "read",
    )?;
    let conn = raw_conn(&fixture)?;
    conn.execute("UPDATE event SET type='session.tool.input.started.1', data=json_remove(data,'$.execution') WHERE type='session.tool.input.started.2'", []).map_err(|error| error.to_string())?;
    drop(conn);
    assert!(public(&fixture)?
        .to_string()
        .contains("truncated or corrupt"));
    Ok(())
}
