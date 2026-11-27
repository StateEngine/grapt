//! Published tool schemas agree with the strict Rust decoders.
use super::*;

#[test]
fn sandbox_inactive_resource_matches_published_schema() -> Result<(), String> {
    let definition = definitions()
        .into_iter()
        .find(|definition| definition.name == "request_sandbox_access")
        .ok_or("sandbox tool missing")?;
    let mut strict: serde_json::Value =
        serde_json::from_str(&definition.input_schema).map_err(|error| error.to_string())?;
    strict["required"] = serde_json::json!(strict["properties"]
        .as_object()
        .ok_or("sandbox properties missing")?
        .keys()
        .collect::<Vec<_>>());
    for raw in [
        r#"{"cmd":"echo bounded","description":"Check bounded fixture output","workdir":"/fixture","shell":"sh","login":false,"tty":false,"yield_time_ms":250,"timeout_ms":1000,"max_output_tokens":1024,"path":"/fixture","host":null}"#,
        r#"{"cmd":"echo bounded","description":"Check bounded fixture output","workdir":"/fixture","shell":"sh","login":false,"tty":false,"yield_time_ms":250,"timeout_ms":1000,"max_output_tokens":1024,"path":null,"host":"example.invalid"}"#,
    ] {
        schema::validate(&strict.to_string(), raw.as_bytes())?;
        let _: sandbox_access::Arguments = decode_input(&input(raw)?)?;
    }
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn provider_projection_and_strict_rust_decode_agree() -> Result<(), String> {
    let definitions = definitions();
    let projections = vec![
        tool_definition::<ReadArguments>(READ_DESCRIPTION),
        tool_definition::<EditArguments>(EDIT_DESCRIPTION),
        tool_definition::<ShellArguments>(&format!(
            "{SHELL_DESCRIPTION} {}",
            catalog::SHELL_HOST_DESCRIPTION
        )),
        tool_definition::<SubagentArguments>(SUBAGENT_DESCRIPTION),
        tool_definition::<skill::SkillArguments>(skill::SKILL_DESCRIPTION),
        tool_definition::<HistoryArguments>(HISTORY_DESCRIPTION),
        tool_definition::<coding::WriteArguments>(coding::WRITE_DESCRIPTION),
        tool_definition::<coding::GlobArguments>(coding::GLOB_DESCRIPTION),
        tool_definition::<coding::GrepArguments>(coding::GREP_DESCRIPTION),
        tool_definition::<coding::ReadLinesArguments>(coding::READ_LINES_DESCRIPTION),
        tool_definition::<coding::ListArguments>(coding::LIST_DESCRIPTION),
        tool_definition::<coding::patch::PatchArguments>(coding::patch::PATCH_DESCRIPTION),
        coding::patch::apply_patch::definition(),
        tool_definition::<web::FetchArguments>(web::FETCH_DESCRIPTION),
        tool_definition::<super::super::image::Arguments>(super::super::image::DESCRIPTION),
        tool_definition::<agent_interrupt::InterruptAgentArguments>(
            agent_interrupt::INTERRUPT_AGENT_DESCRIPTION,
        ),
        tool_definition::<agent_spawn::SpawnAgentArguments>(agent_spawn::SPAWN_AGENT_DESCRIPTION),
        tool_definition::<agents::ListAgentsArguments>(agents::LIST_AGENTS_DESCRIPTION),
        tool_definition::<agent_wait::WaitAgentArguments>(agent_wait::WAIT_AGENT_DESCRIPTION),
        tool_definition::<plan::UpdatePlanArgs>(plan::DESCRIPTION),
        tool_definition::<ask::Arguments>(ask::DESCRIPTION),
        tool_definition::<agent_message::SendMessageArguments>(
            agent_message::SEND_MESSAGE_DESCRIPTION,
        ),
        tool_definition::<agent_followup::FollowupTaskArguments>(
            agent_followup::FOLLOWUP_TASK_DESCRIPTION,
        ),
        tool_definition::<terminal::ExecArguments>(terminal::EXEC_DESCRIPTION),
        tool_definition::<terminal::StdinArguments>(terminal::STDIN_DESCRIPTION),
        tool_definition::<read_history::Arguments>(read_history::DESCRIPTION),
    ];
    for projection in projections {
        assert!(
            definitions.contains(&projection),
            "provider schema diverged from its strict Rust decoder: {}",
            projection.name
        );
    }
    assert!(definitions.iter().all(|definition| definition
        .description
        .as_ref()
        .is_some_and(|description| !description.is_empty() && description.len() <= 512)));
    let schemas = definitions
        .iter()
        .map(|definition| {
            serde_json::from_str::<serde_json::Value>(&definition.input_schema)
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    for schema in &schemas {
        assert_eq!(
            schema.get("additionalProperties"),
            Some(&serde_json::Value::Bool(false))
        );
        assert_eq!(schema.get("type"), Some(&serde_json::json!("object")));
    }

    // glob and grep: the two schemas with exclude lists.
    for schema in &schemas[7..=8] {
        assert_eq!(schema["properties"]["exclude"]["maxItems"], 32);
        assert_eq!(schema["properties"]["exclude"]["items"]["maxLength"], 256);
    }
    assert_eq!(schemas[0]["required"], serde_json::json!(["path"]));
    assert_eq!(
        schemas[0]["properties"]["path"],
        <String as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(
        schemas[0]["properties"]["offset"],
        <BoundedU64<1, READ_MAX_LINE> as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(
        schemas[0]["properties"]["limit"],
        <BoundedU64<1, READ_PAGE_LINES> as ProviderFieldSchema>::provider_schema(),
        "read arguments lost the positive bounded Rust type shared by decoding and schema projection"
    );
    assert_eq!(
        schemas[1]["required"],
        serde_json::json!(["path", "old_text", "new_text"])
    );
    assert_eq!(
        schemas[1]["properties"]["path"],
        <String as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(
        schemas[1]["properties"]["old_text"],
        <NonEmptyString as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(
        schemas[1]["properties"]["new_text"],
        <String as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(schemas[2]["required"], serde_json::json!(["command"]));
    assert_eq!(
        schemas[2]["properties"]["command"],
        <NonEmptyString as ProviderFieldSchema>::provider_schema()
    );
    assert_eq!(
        schemas[2]["properties"]["timeout_ms"],
        <BoundedU64<1, SHELL_MAX_TIMEOUT_MS> as ProviderFieldSchema>::provider_schema()
    );

    let decoded_read: ReadArguments =
        decode_input(&input(r#"{"path":"src/lib.rs","offset":4,"limit":2000}"#)?)?;
    assert_eq!(
        decoded_read,
        ReadArguments {
            path: "src/lib.rs".to_owned(),
            offset: Some(BoundedU64(4)),
            limit: Some(BoundedU64(READ_PAGE_LINES)),
        }
    );
    let _: EditArguments = decode_input(&input(
        r#"{"path":"src/lib.rs","old_text":"old","new_text":"new"}"#,
    )?)?;
    let _: ShellArguments =
        decode_input(&input(r#"{"command":"echo bounded","timeout_ms":120000}"#)?)?;
    let absent_read: ReadArguments = decode_input(&input(r#"{"path":"src/lib.rs"}"#)?)?;
    assert_eq!(absent_read.offset, None);
    assert_eq!(absent_read.limit, None);
    let absent_shell: ShellArguments = decode_input(&input(r#"{"command":"echo bounded"}"#)?)?;
    assert_eq!(absent_shell.timeout_ms, None);

    let read_offset_null =
        decode_input::<ReadArguments>(&input(r#"{"path":"src/lib.rs","offset":null}"#)?);
    let read_limit_null =
        decode_input::<ReadArguments>(&input(r#"{"path":"src/lib.rs","limit":null}"#)?);
    let shell_timeout_null =
        decode_input::<ShellArguments>(&input(r#"{"command":"echo","timeout_ms":null}"#)?);
    assert!(
        read_offset_null.is_err() && read_limit_null.is_err() && shell_timeout_null.is_err(),
        "an optional tool argument accepted explicit null outside its non-null provider schema"
    );

    for raw in [
        r#"{}"#,
        r#"{"path":1}"#,
        r#"{"path":"src/lib.rs","unknown":true}"#,
        r#"{"path":"src/lib.rs","limit":2001}"#,
        r#"{"path":"src/lib.rs","offset":0}"#,
    ] {
        assert!(decode_input::<ReadArguments>(&input(raw)?).is_err());
    }
    assert!(
            decode_input::<ReadArguments>(&input(r#"{"path":"src/lib.rs","limit":0}"#)?)
                .is_err(),
            "read arguments lost the positive bounded Rust type shared by decoding and schema projection"
        );
    for raw in [
        r#"{"path":"src/lib.rs","old_text":"old"}"#,
        r#"{"path":"src/lib.rs","old_text":1,"new_text":"new"}"#,
        r#"{"path":"src/lib.rs","old_text":"","new_text":"new"}"#,
        r#"{"path":"src/lib.rs","old_text":"old","new_text":"new","unknown":true}"#,
    ] {
        assert!(decode_input::<EditArguments>(&input(raw)?).is_err());
    }
    for raw in [
        r#"{}"#,
        r#"{"command":1}"#,
        r#"{"command":"echo","unknown":true}"#,
        r#"{"command":"echo","timeout_ms":0}"#,
        r#"{"command":"echo","timeout_ms":120001}"#,
    ] {
        assert!(decode_input::<ShellArguments>(&input(raw)?).is_err());
    }

    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("sample.txt"), b"bounded").map_err(|error| error.to_string())?;
    assert!(prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(r#"{"path":"sample.txt","limit":0}"#)?,
        &no_routes()
    )
    .is_err());
    assert!(prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(r#"{"path":"sample.txt","limit":1048577}"#)?,
        &no_routes()
    )
    .is_err());
    assert!(prepare_effect(
        &scratch.0,
        EDIT_TOOL,
        &input(r#"{"path":"sample.txt","old_text":"","new_text":"new"}"#)?,
        &no_routes()
    )
    .is_err());
    assert!(prepare_effect(
        &scratch.0,
        SHELL_TOOL,
        &input(r#"{"command":"echo","timeout_ms":120001}"#)?,
        &no_routes()
    )
    .is_err());
    Ok(())
}

#[test]
fn advertised_integer_bounds_are_json_safe() -> Result<(), String> {
    let safe = JSON_SAFE_INTEGER_MAX as i64;
    for definition in definitions() {
        let schema: serde_json::Value =
            serde_json::from_str(&definition.input_schema).map_err(|error| error.to_string())?;
        let mut pending = vec![schema];
        while let Some(value) = pending.pop() {
            match value {
                serde_json::Value::Object(fields) => {
                    for (key, value) in fields {
                        if matches!(
                            key.as_str(),
                            "minimum" | "maximum" | "exclusiveMinimum" | "exclusiveMaximum"
                        ) {
                            let bound = value.as_i64().ok_or_else(|| {
                                format!(
                                    "{}: {key} must be an integer, got {value}",
                                    definition.name
                                )
                            })?;
                            assert!(
                                (-safe..=safe).contains(&bound),
                                "{}: {key} {bound} exceeds the JSON-safe range",
                                definition.name
                            );
                        }
                        pending.push(value);
                    }
                }
                serde_json::Value::Array(values) => pending.extend(values),
                _ => {}
            }
        }
    }
    Ok(())
}
