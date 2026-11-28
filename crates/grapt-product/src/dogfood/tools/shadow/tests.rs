use super::*;
use std::sync::mpsc::{sync_channel, Receiver};
type TestResult = Result<(), Box<dyn std::error::Error>>;

fn capture() -> Result<(Arc<Ledger>, Receiver<String>), Box<dyn std::error::Error>> {
    let (send, receive) = sync_channel(64);
    let recorder = Recorder::spawn(SCHEMA, QUEUE, move |line| {
        let _ = send.send(line.to_owned());
    })
    .ok_or("diagnostic writer did not start")?;
    Ok((
        Arc::new(Ledger {
            recorder: Some(recorder),
            instance: Some("evt_shadow_fixture".parse()?),
            next: AtomicU64::new(0),
        }),
        receive,
    ))
}

fn row(receive: &Receiver<String>) -> Result<serde_json::Value, Box<dyn std::error::Error>> {
    Ok(serde_json::from_str(
        &receive.recv_timeout(Duration::from_secs(2))?,
    )?)
}

#[test]
fn concrete_dispatches_keep_distinct_ordinals_and_closed_redacted_phases() -> TestResult {
    let (ledger, receive) = capture()?;
    let session = "ses_shadow_fixture".parse()?;
    let secret = "PRIVATE_SHADOW_PAYLOAD_82309";
    let direct = ledger
        .dispatch(&session, None, Origin::Direct, secret)
        .ok_or("direct observation missing")?;
    direct.prepared(&EffectTarget::Read(PathBuf::from(secret)));
    direct.authorized();
    direct.started();
    direct.finish(&ToolTerminalOutcome::Completed { output: None });
    direct.finish(&ToolTerminalOutcome::ResultMissing);
    drop(direct);
    let nested = ledger
        .dispatch(&session, None, Origin::Nested, secret)
        .ok_or("nested observation missing")?;
    nested.finish(&ToolTerminalOutcome::Failed {
        reason: ToolFailureReason::AuthorityUnavailable,
        detail: None,
    });
    drop(nested);
    let rows = (0..7)
        .map(|_| row(&receive))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(rows[0]["origin"], "direct");
    assert_eq!(rows[5]["origin"], "nested");
    assert_ne!(rows[0]["dispatch"], rows[5]["dispatch"]);
    let phases: Vec<_> = rows.iter().map(|row| row["phase"].as_str()).collect();
    assert_eq!(
        phases,
        vec![
            Some("dispatch"),
            Some("prepared"),
            Some("permission_granted"),
            Some("effect_started"),
            Some("completed"),
            Some("dispatch"),
            Some("authority_unavailable"),
        ]
    );
    for row in rows {
        assert!(!row.to_string().contains(secret));
        assert_eq!(row["routing"], "unmeasured");
        assert_eq!(row["schema"], SCHEMA);
        assert!(row.get("call_id").is_none());
        assert!(row.get("arguments").is_none());
    }
    assert!(
        receive.try_recv().is_err(),
        "duplicate terminal was recorded"
    );
    let oversized = "PRIVATE_SHADOW_COORDINATE".repeat(COORDINATE_BYTES);
    let attempt =
        grapt_types::PhysicalAttemptId::new(oversized.clone()).ok_or("fixture attempt invalid")?;
    let dispatch = ledger
        .dispatch(
            &format!("ses_{oversized}").parse()?,
            Some(&attempt),
            Origin::Direct,
            SHELL_TOOL,
        )
        .ok_or("bounded observation missing")?;
    dispatch.finish(&ToolTerminalOutcome::ResultMissing);
    for _ in 0..2 {
        let row = row(&receive)?;
        assert_eq!(row["coordinates_over_bound"], true);
        assert!(row["session"].is_null() && row["attempt"].is_null());
        assert!(!row.to_string().contains("PRIVATE_SHADOW_COORDINATE"));
        assert!(row.to_string().len() < SHAPE_INPUT_BYTES);
    }
    Ok(())
}

#[test]
fn dropping_an_unfinished_dispatch_records_abandonment_without_completion() -> TestResult {
    let (ledger, receive) = capture()?;
    let dispatch = ledger
        .dispatch(
            &"ses_shadow_drop".parse()?,
            None,
            Origin::PluginHost,
            SHELL_TOOL,
        )
        .ok_or("observation missing")?;
    drop(dispatch);
    assert_eq!(row(&receive)?["phase"], "dispatch");
    let abandoned = row(&receive)?;
    assert_eq!(abandoned["phase"], "abandoned");
    assert_eq!(abandoned["origin"], "plugin_host");
    Ok(())
}

#[test]
fn command_shapes_cannot_echo_scripts_targets_or_unknown_names() {
    let secret = "PRIVATE_SHADOW_COMMAND_74192";
    for command in [
        format!("git -C '{secret}' status"),
        format!("python3 -c 'print(\"{secret}\")'"),
        format!("{secret} --token {secret}"),
        format!("cat '{secret}'"),
        secret.repeat(SHAPE_INPUT_BYTES),
    ] {
        assert!(!command_shape(&command).contains(secret));
    }
    assert_eq!(command_shape("git status"), "git_status");
    assert_eq!(command_shape("cargo test"), "cargo_test");
    assert_eq!(command_shape("git status && cargo test"), "mixed");
    assert_eq!(command_shape("git $PRIVATE_COMMAND"), "opaque");
}

#[test]
fn incomplete_quotes_and_continuations_stay_opaque() {
    for command in [
        "git status 'SYNTHETIC_UNCLOSED",
        "git status \"SYNTHETIC_UNCLOSED",
        "git status \\",
        "git status `",
        "git status ^",
        "git status 'SYNTHETIC_UNCLOSED\n",
    ] {
        assert_eq!(command_shape(command), "opaque");
    }
    for command in [
        "git status 'SYNTHETIC_CLOSED'",
        "git status \"SYNTHETIC_CLOSED\"",
        "git status # 'SYNTHETIC_COMMENT",
        "git status\n",
    ] {
        assert_eq!(command_shape(command), "git_status");
    }
}

#[test]
fn repairs_and_hints_record_only_their_closed_labels() -> TestResult {
    let (ledger, receive) = capture()?;
    let session = "ses_shadow_fixture".parse()?;
    let secret = "PRIVATE_SHADOW_PAYLOAD_51077";
    let dispatch = ledger
        .dispatch(&session, None, Origin::Direct, secret)
        .ok_or("observation missing")?;
    dispatch.repaired(call_repair::Repair::AliasAndCoercion);
    dispatch.hinted(call_repair::Hint::UnknownParameter);
    dispatch.finish(&ToolTerminalOutcome::Completed { output: None });
    let rows = (0..4)
        .map(|_| row(&receive))
        .collect::<Result<Vec<_>, _>>()?;
    let labels: Vec<_> = rows
        .iter()
        .map(|row| (row["phase"].as_str(), row["shape"].as_str()))
        .collect();
    assert_eq!(
        labels[1],
        (Some("input_repaired"), Some("alias_and_coercion"))
    );
    assert_eq!(
        labels[2],
        (Some("recovery_hint"), Some("unknown_parameter"))
    );
    assert!(rows.iter().all(|row| !row.to_string().contains(secret)));
    Ok(())
}
