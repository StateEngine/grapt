use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

mod catalog_contract;
mod developer_help;
mod shell;
mod shell_guidance;
mod terminal_prefix;
mod tool_errors;

static SCRATCH_ORDINAL: AtomicUsize = AtomicUsize::new(0);

pub(super) struct Scratch(pub(super) PathBuf);

impl Scratch {
    pub(super) fn new() -> Result<Self, String> {
        Self::new_in(&std::env::temp_dir())
    }

    pub(super) fn new_in(base: &Path) -> Result<Self, String> {
        let ordinal = SCRATCH_ORDINAL.fetch_add(1, Ordering::Relaxed);
        let path = base.join(format!(
            "grapt-product-minimum-tools-{}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).map_err(|error| error.to_string())?;
        let canonical = std::fs::canonicalize(path).map_err(|error| error.to_string())?;
        Ok(Self(canonical))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// No MCP routes: these witnesses cover the built-in tools, whose behaviour
/// must not change because a configured server exists.
fn no_routes() -> std::collections::BTreeMap<String, crate::dogfood::mcp::McpRoute> {
    std::collections::BTreeMap::new()
}

fn input(raw: &str) -> Result<BoundedToolInput, String> {
    BoundedToolInput::new(raw.as_bytes().to_vec()).map_err(|error| error.to_string())
}

#[tokio::test]
async fn rejected_plugin_effect_cannot_report_success() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let artifacts = FilesystemArtifactStore::open(
        scratch.0.join("artifacts"),
        b"plugin-permission-rejection".to_vec(),
        Box::new(SystemClock),
    )
    .map_err(|error| error.to_string())?;
    for refused in [
        None,
        Some(sandbox::Receipt {
            enforcement: sandbox::degraded(sandbox::msys::REASON),
            ran: Some(false),
        }),
    ] {
        let permission = "per_plugin_rejected"
            .parse()
            .map_err(|_| "static permission identity was invalid".to_owned())?;
        let terminal = settle_authorized_effect(
            Err(ToolAuthorizationError::Rejected(permission)),
            &artifacts,
            refused.as_ref(),
        )
        .await;
        let ToolTerminalOutcome::Failed {
            reason,
            detail: Some(detail),
        } = terminal
        else {
            return Err("a rejected effect did not retain its failure".into());
        };
        assert_eq!(reason, ToolFailureReason::AuthorityUnavailable);
        let page = artifacts
            .fetch(
                detail.reference(),
                0,
                ReadBudget::new(2048).ok_or("budget")?,
            )
            .map_err(|error| error.to_string())?;
        if let Some(expected) = refused {
            let raw = std::str::from_utf8(page.bytes()).map_err(|error| error.to_string())?;
            let actual =
                sandbox::Receipt::parse(raw).ok_or("rejection lost its enforcement receipt")?;
            assert_eq!(actual.enforcement, expected.enforcement);
            assert_eq!(actual.ran, Some(false));
            let long = runner::refusal_detail(Some(&expected), &"long reason\"\\".repeat(4096));
            assert!(long.len() <= 2048 && sandbox::receipt(&long).is_some());
        }
    }
    Ok(())
}

#[test]
fn no_built_in_schema_combines_alternatives_at_its_top_level() -> Result<(), String> {
    // Anthropic refuses the whole request when one tool's input schema has a
    // top-level combinator, on every host OpenRouter routes to. With the
    // history tool's former top-level oneOf, SE's exact request got HTTP 400
    // "tools.5.custom.input_schema: input_schema does not support oneOf,
    // allOf, or anyOf at the top level"; flattened, it got HTTP 200
    // (the 2026-10-07 OpenRouter schema probe receipts).
    // Every route publishes these definitions.
    for definition in definitions() {
        let schema: serde_json::Value =
            serde_json::from_str(&definition.input_schema).map_err(|error| error.to_string())?;
        assert_eq!(schema["type"], "object", "{}", definition.name);
        for keyword in ["oneOf", "anyOf", "allOf"] {
            assert!(
                schema.get(keyword).is_none(),
                "{} publishes a top-level {keyword}",
                definition.name
            );
        }
    }
    Ok(())
}

#[test]
fn sr_h01_history_call_selects_fetch_and_listing_keeps_its_cursor() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let definition = tool_definition::<HistoryArguments>(HISTORY_DESCRIPTION);
    for valid in [
        r#"{}"#,
        r#"{"cursor":12}"#,
        r#"{"call":"call_2bzz9Zp0p9johRznPHgW6s1B"}"#,
        r#"{"call":"call_2bzz9Zp0p9johRznPHgW6s1B","offset":65536}"#,
        r#"{"call":null}"#,
        r#"{"cursor":null}"#,
        r#"{"call":"call_recorded","cursor":null}"#,
    ] {
        schema::validate(&definition.input_schema, valid.as_bytes()).map_err(str::to_owned)?;
        let prepared = prepare_effect(&scratch.0, HISTORY_TOOL, &input(valid)?, &no_routes())?;
        let EffectTarget::History { call, cursor, .. } = prepared.target else {
            return Err("history did not prepare a history target".into());
        };
        let expected: serde_json::Value = serde_json::from_str(valid).map_err(|e| e.to_string())?;
        assert_eq!(
            call.as_ref().map(ToolCallId::as_str),
            expected["call"].as_str()
        );
        assert_eq!(cursor, expected["cursor"].as_u64());
    }
    let mixed_fetches = [
        r#"{"call":"call_2bzz9Zp0p9johRznPHgW6s1B","cursor":12,"offset":0}"#,
        r#"{"call":"x","cursor":1,"offset":0}"#,
        r#"{"call":"call_2bzz9Zp0p9johRznPHgW6s1B","cursor":0,"offset":1}"#,
        r#"{"call":"call_2bzz9Zp0p9johRznPHgW6s1B","cursor":0,"offset":0}"#,
    ];
    // The flat provider schema may produce all fields together. Call selects
    // the existing fetch target even when cursor is nonzero.
    for raw in mixed_fetches {
        schema::validate(&definition.input_schema, raw.as_bytes()).map_err(str::to_owned)?;
        let prepared = prepare_effect(&scratch.0, HISTORY_TOOL, &input(raw)?, &no_routes())?;
        let EffectTarget::History {
            call,
            cursor,
            offset,
        } = prepared.target
        else {
            return Err("history did not prepare a history target".into());
        };
        let expected: serde_json::Value = serde_json::from_str(raw).map_err(|e| e.to_string())?;
        assert_eq!(
            call.as_ref().map(ToolCallId::as_str),
            expected["call"].as_str()
        );
        assert_eq!(cursor, None);
        assert_eq!(offset, expected["offset"].as_u64());
    }
    for invalid in [
        r#"{"call":""}"#,
        r#"{"cursor":-1}"#,
        r#"{"call":"call_recorded","cursor":"0"}"#,
        r#"{"call":"call_recorded","offset":-1}"#,
        r#"{"unknown":true}"#,
    ] {
        assert!(schema::validate(&definition.input_schema, invalid.as_bytes()).is_err());
        assert!(prepare_effect(&scratch.0, HISTORY_TOOL, &input(invalid)?, &no_routes()).is_err());
    }
    assert_eq!(
        prepare_effect(
            &scratch.0,
            HISTORY_TOOL,
            &input(r#"{"offset":0}"#)?,
            &no_routes()
        )
        .err()
        .as_deref(),
        Some("history offset requires a call identity")
    );
    Ok(())
}

#[test]
fn unknown_fields_and_unknown_tools_refuse() -> Result<(), String> {
    let input = serde_json::from_slice::<ReadArguments>(br#"{"path":"src/lib.rs","escape":true}"#);
    assert!(input.is_err());
    Ok(())
}

#[test]
fn file_effects_are_exact_bounded_and_project_contained() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let target = scratch.0.join("sample.txt");
    std::fs::write(&target, b"before value\n").map_err(|error| error.to_string())?;

    let read_input = input(r#"{"path":"sample.txt","offset":1,"limit":1}"#)?;
    let prepared = prepare_effect(&scratch.0, READ_TOOL, &read_input, &no_routes())?;
    let EffectTarget::Read(expected) = prepared.target else {
        return Err("read preparation changed effect class".to_owned());
    };
    let read = String::from_utf8(run_read(&scratch.0, &expected, &read_input)?)
        .map_err(|error| error.to_string())?;
    assert!(
        read.starts_with("1: before value\n[sha256 for patch: "),
        "{read}"
    );

    let edit_input = input(r#"{"path":"sample.txt","old_text":"before","new_text":"after"}"#)?;
    let prepared = prepare_effect(&scratch.0, EDIT_TOOL, &edit_input, &no_routes())?;
    assert_eq!(
        prepared.action, "fs.modify",
        "the retained permission UI must identify the edit consequence"
    );
    let EffectTarget::Edit(expected) = prepared.target else {
        return Err("edit preparation changed effect class".to_owned());
    };
    let receipt = run_edit(&scratch.0, &expected, &edit_input)?;
    let durable: serde_json::Value =
        serde_json::from_slice(&receipt).map_err(|error| error.to_string())?;
    assert_eq!(
        durable["files"][0]["patch"],
        "--- sample.txt\n+++ sample.txt\n@@ -1,1 +1,1 @@\n-before value\n+after value\n"
    );
    assert_eq!(durable["files"][0]["relativePath"], "sample.txt");
    assert!(String::from_utf8_lossy(&receipt).contains("\"exactReplacements\":1"));
    assert_eq!(
        std::fs::read_to_string(target).map_err(|error| error.to_string())?,
        "after value\n"
    );

    let absent_edit =
        input(r#"{"path":"sample.txt","old_text":"absent","new_text":"replacement"}"#)?;
    let absent = prepare_effect(&scratch.0, EDIT_TOOL, &absent_edit, &no_routes())?;
    let EffectTarget::Edit(absent_target) = absent.target else {
        return Err("absent edit preparation changed effect class".to_owned());
    };
    assert!(
        run_edit(&scratch.0, &absent_target, &absent_edit).is_err(),
        "an edit with zero exact matches reached its terminal write"
    );
    let duplicate_target = scratch.0.join("duplicate.txt");
    std::fs::write(&duplicate_target, b"same same").map_err(|error| error.to_string())?;
    let duplicate_edit =
        input(r#"{"path":"duplicate.txt","old_text":"same","new_text":"replacement"}"#)?;
    let duplicate = prepare_effect(&scratch.0, EDIT_TOOL, &duplicate_edit, &no_routes())?;
    let EffectTarget::Edit(duplicate_target) = duplicate.target else {
        return Err("duplicate edit preparation changed effect class".to_owned());
    };
    assert!(
        run_edit(&scratch.0, &duplicate_target, &duplicate_edit).is_err(),
        "an edit with duplicate exact matches reached its terminal write"
    );

    assert!(prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(r#"{"path":"../escape"}"#)?,
        &no_routes()
    )
    .is_err());
    std::fs::create_dir_all(scratch.0.join(".git")).map_err(|error| error.to_string())?;
    std::fs::write(scratch.0.join(".git/config"), b"protected")
        .map_err(|error| error.to_string())?;
    assert!(
        prepare_effect(
            &scratch.0,
            EDIT_TOOL,
            &input(r#"{"path":".git/config","old_text":"p","new_text":"q"}"#)?,
            &no_routes()
        )
        .is_err(),
        "a protected control path reached writable effect preparation"
    );
    Ok(())
}

#[test]
fn absolute_paths_preserve_root_and_control_metadata_boundaries() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let outside = Scratch::new()?;
    let path = scratch.0.join("sample.txt");
    std::fs::write(&path, b"before").map_err(|error| error.to_string())?;
    let absolute = serde_json::json!({"path": path});
    let prepared = prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(&absolute.to_string())?,
        &no_routes(),
    )?;
    let EffectTarget::Read(target) = prepared.target else {
        return Err("read target changed class".into());
    };
    assert!(
        run_read(&scratch.0, &target, &input(&absolute.to_string())?)?.starts_with(b"1: before\n")
    );
    let edit = input(
        &serde_json::json!({"path": path,"old_text":"before","new_text":"after"}).to_string(),
    )?;
    let prepared = prepare_effect(&scratch.0, EDIT_TOOL, &edit, &no_routes())?;
    let EffectTarget::Edit(target) = prepared.target else {
        return Err("edit target changed class".into());
    };
    run_edit(&scratch.0, &target, &edit)?;
    assert_eq!(
        std::fs::read(&path).map_err(|error| error.to_string())?,
        b"after"
    );
    let foreign = outside.0.join("foreign.txt");
    std::fs::write(&foreign, b"untouched").map_err(|error| error.to_string())?;
    assert!(prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(&serde_json::json!({"path":foreign}).to_string())?,
        &no_routes()
    )
    .is_err());
    std::fs::create_dir_all(scratch.0.join(".git")).map_err(|error| error.to_string())?;
    std::fs::write(scratch.0.join(".git/config"), b"control").map_err(|error| error.to_string())?;
    for path in [
        scratch.0.join(".git/config"),
        scratch.0.join("./.git/config"),
    ] {
        assert!(prepare_effect(
            &scratch.0,
            EDIT_TOOL,
            &input(
                &serde_json::json!({"path":path,"old_text":"control","new_text":"changed"})
                    .to_string()
            )?,
            &no_routes()
        )
        .is_err());
    }
    assert_eq!(
        std::fs::read(scratch.0.join(".git/config")).map_err(|error| error.to_string())?,
        b"control"
    );
    Ok(())
}

#[tokio::test]
async fn shell_is_cwd_bound_timed_and_output_bounded() -> Result<(), String> {
    let scratch = Scratch::new()?;
    let shell_input = input(r#"{"command":"echo harness","timeout_ms":5000}"#)?;
    let prepared = prepare_effect(&scratch.0, SHELL_TOOL, &shell_input, &no_routes())?;
    let EffectTarget::Shell {
        command: expected,
        program,
        environment,
    } = prepared.target
    else {
        return Err("shell preparation changed effect class".to_owned());
    };
    let output = run_shell(
        &scratch.0,
        &expected,
        &program,
        &environment,
        &shell_input,
        &[],
        None,
    )
    .await?
    .model_output()?;
    assert!(String::from_utf8_lossy(&output).contains("harness"));
    assert!(output.len() <= SHELL_STREAM_MAX_BYTES * 2 + 256);
    assert!(prepare_effect(
        &scratch.0,
        SHELL_TOOL,
        &input(r#"{"command":"echo late","timeout_ms":120001}"#)?,
        &no_routes()
    )
    .is_err());
    let failed_input = input(r#"{"command":"exit 7","timeout_ms":5000}"#)?;
    let failed = prepare_effect(&scratch.0, SHELL_TOOL, &failed_input, &no_routes())?;
    let EffectTarget::Shell {
        command: failed_command,
        program: failed_program,
        environment: failed_environment,
    } = failed.target
    else {
        return Err("failed shell preparation changed effect class".to_owned());
    };
    let failed_output = run_shell(
        &scratch.0,
        &failed_command,
        &failed_program,
        &failed_environment,
        &failed_input,
        &[],
        None,
    )
    .await?
    .model_output()?;
    assert!(String::from_utf8_lossy(&failed_output).ends_with("\n\nCommand exited with code 7."));
    Ok(())
}

#[cfg(windows)]
#[tokio::test]
async fn shell_preserves_quoted_program_and_spaced_arguments_after_authorization(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    std::fs::write(
        scratch.0.join("quoted program.cmd"),
        b"@echo off\r\necho [%~1]\r\n",
    )
    .map_err(|error| error.to_string())?;
    for text in [
        r#"".\quoted program.cmd" "two words""#,
        r#"echo before&& ".\quoted program.cmd" "two words""#,
    ] {
        let shell_input =
            input(&serde_json::json!({"command":text,"timeout_ms":5000}).to_string())?;
        let prepared = prepare_effect(&scratch.0, SHELL_TOOL, &shell_input, &no_routes())?;
        let EffectTarget::Shell {
            command: expected,
            program,
            environment,
        } = prepared.target
        else {
            return Err("shell preparation changed effect class".to_owned());
        };
        let output = run_shell(
            &scratch.0,
            &expected,
            &program,
            &environment,
            &shell_input,
            &[],
            None,
        )
        .await?
        .model_output()?;
        assert!(
            String::from_utf8_lossy(&output)
                .lines()
                .any(|line| line == "[two words]"),
            "quoted program or spaced argument changed: {output:?}"
        );
        let changed = input(r#"{"command":"echo forbidden>unauthorized.txt","timeout_ms":5000}"#)?;
        let refusal = run_shell(
            &scratch.0,
            &expected,
            &program,
            &environment,
            &changed,
            &[],
            None,
        )
        .await;
        assert_eq!(
            refusal.err(),
            Some("shell argv changed after authorization".to_owned())
        );
        assert!(!scratch
            .0
            .join("unauthorized.txt")
            .try_exists()
            .map_err(|error| error.to_string())?);
    }
    Ok(())
}

fn mcp_route(server: &str, tool: &str, command: &[&str]) -> crate::dogfood::mcp::McpRoute {
    mcp_route_at(server, tool, command, 1)
}

fn mcp_route_at(
    server: &str,
    tool: &str,
    command: &[&str],
    incarnation: u64,
) -> crate::dogfood::mcp::McpRoute {
    let (program, arguments) = match command.split_first() {
        Some((program, rest)) => (
            (*program).to_owned(),
            rest.iter().map(|part| (*part).to_owned()).collect(),
        ),
        None => (String::new(), Vec::new()),
    };
    crate::dogfood::mcp::McpRoute {
        description: None,
        output_schema: None,
        server: server.to_owned(),
        tool: match grapt_mcp::McpToolName::new(tool) {
            Some(name) => name,
            None => panic!("{tool:?} must be a usable tool name"),
        },
        launch: crate::dogfood::mcp::LaunchIdentity {
            declaration: std::sync::Arc::new(crate::dogfood::mcp::Declaration::Local {
                command: command.iter().map(|part| (*part).to_owned()).collect(),
                cwd: "E:/projects/fixture".to_owned(),
                environment: std::collections::BTreeMap::new(),
                timeouts: crate::dogfood::mcp::McpTimeouts::default(),
            }),
            program,
            cwd: "E:/projects/fixture".to_owned(),
            arguments,
            incarnation,
        },
    }
}

fn routes_with(
    name: &str,
    route: crate::dogfood::mcp::McpRoute,
) -> std::collections::BTreeMap<String, crate::dogfood::mcp::McpRoute> {
    let mut routes = std::collections::BTreeMap::new();
    routes.insert(name.to_owned(), route);
    routes
}

/// The approval binds the ARGUMENTS, not merely the tool.
///
/// Ratified law C5: approval binds the exact program and argument vector, and
/// a later argv rewrite is a different capability rather than the approved
/// one. Approving `srv_issue` to file one issue must not approve filing a
/// different one.
#[test]
fn approving_an_mcp_call_does_not_approve_it_with_different_arguments(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    let routes = routes_with(
        "srv_issue",
        mcp_route("srv", "issue", &["node", "server.js"]),
    );

    let first = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes,
    )?;
    let second = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"two"}"#)?,
        &routes,
    )?;
    assert_ne!(
        first.capability, second.capability,
        "different arguments are a different capability, not a reused approval"
    );
    Ok(())
}

/// Two spellings of one argument set are ONE capability. Otherwise a refusal
/// could be dodged by reordering keys.
#[test]
fn argument_spelling_does_not_change_the_capability() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    let routes = routes_with(
        "srv_issue",
        mcp_route("srv", "issue", &["node", "server.js"]),
    );

    let one = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"a":1,"b":2}"#)?,
        &routes,
    )?;
    let other = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"b":2,"a":1}"#)?,
        &routes,
    )?;
    assert_eq!(
        one.capability, other.capability,
        "canonical arguments make one approval cover one request, however it is spelled"
    );
    Ok(())
}

/// A declaration repointed at a different COMMAND is a different capability.
/// Otherwise an approval given for one program would be inherited by another
/// under the same configured name.
#[test]
fn repointing_a_server_at_another_command_is_a_different_capability(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    let before = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with(
            "srv_issue",
            mcp_route("srv", "issue", &["node", "server.js"]),
        ),
    )?;
    let after = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with(
            "srv_issue",
            mcp_route("srv", "issue", &["node", "other.js"]),
        ),
    )?;
    assert_ne!(
        before.capability, after.capability,
        "a repointed declaration must not inherit the old approval"
    );
    Ok(())
}

/// The user approves the QUALIFIED NAME -- the same string the model was shown
/// and a receipt records. One identity in three places.
#[test]
fn the_approved_action_is_the_name_the_model_was_shown() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    let prepared = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with(
            "srv_issue",
            mcp_route("srv", "issue", &["node", "server.js"]),
        ),
    )?;
    assert_eq!(prepared.action, "srv_issue");
    assert_eq!(prepared.resource, "srv/issue");
    Ok(())
}

/// A name with NO ROUTE is refused rather than guessed at. The qualified form
/// cannot be taken apart to recover the server, so a name this execution did
/// not publish names nothing.
#[test]
fn a_qualified_name_this_execution_never_published_is_refused(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    assert!(prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &no_routes(),
    )
    .is_err());
    Ok(())
}

/// A connected server with no recorded command cannot be bound to a program,
/// and an approval that binds nothing is not an approval.
#[test]
fn a_route_with_no_command_cannot_be_approved() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    assert!(prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with("srv_issue", mcp_route("srv", "issue", &[])),
    )
    .is_err());
    Ok(())
}

/// A built-in name is NOT routed through MCP even when a server publishes a
/// route under that exact string. The built-in owns its name.
#[test]
fn a_route_cannot_capture_a_builtin_name() -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    std::fs::write(scratch.0.join("sample.txt"), b"bounded").map_err(|error| error.to_string())?;
    let hijack = routes_with(READ_TOOL, mcp_route("srv", "read", &["node", "server.js"]));
    let prepared = prepare_effect(
        &scratch.0,
        READ_TOOL,
        &input(r#"{"path":"sample.txt"}"#)?,
        &hijack,
    )?;
    assert_eq!(
        prepared.action, "read",
        "the built-in read tool answers for its own name"
    );
    assert!(
        !matches!(prepared.target, EffectTarget::Mcp { .. }),
        "a configured server must not capture a built-in"
    );
    let hijack = routes_with(
        PLAN_TOOL,
        mcp_route("srv", "execute", &["node", "server.js"]),
    );
    let prepared = prepare_effect(&scratch.0, PLAN_TOOL, &input(r#"{"plan":[]}"#)?, &hijack)?;
    assert!(
        matches!(prepared.target, EffectTarget::Plan),
        "a server must not acquire the prompt-free checklist capability"
    );
    assert_eq!(prepared.resource, "task checklist");
    let hijack = routes_with(
        APPLY_PATCH_TOOL,
        mcp_route("srv", "execute", &["node", "server.js"]),
    );
    let args = serde_json::json!({"input":"*** Begin Patch\n*** Add File: created.txt\n+bounded\n*** End Patch"});
    let prepared = prepare_effect(
        &scratch.0,
        APPLY_PATCH_TOOL,
        &input(&args.to_string())?,
        &hijack,
    )?;
    assert_eq!(prepared.action, PATCH_TOOL);
    assert!(
        matches!(
            prepared.target,
            EffectTarget::Coding(coding::CodingTarget::Patch(_))
        ),
        "a server must not capture the native patch executor"
    );
    let hijack = routes_with(
        terminal::EXEC_COMMAND,
        mcp_route("srv", "exec", &["node", "server.js"]),
    );
    let prepared = prepare_effect(
        &scratch.0,
        terminal::EXEC_COMMAND,
        &input(r#"{"cmd":"printf bounded"}"#)?,
        &hijack,
    )?;
    assert_eq!(prepared.action, "shell");
    assert!(
        matches!(prepared.target, EffectTarget::TerminalExec(_)),
        "a server must not capture terminal startup"
    );
    let hijack = routes_with(
        terminal::WRITE_STDIN,
        mcp_route("srv", "stdin", &["node", "server.js"]),
    );
    assert!(
        prepare_effect(
            &scratch.0,
            terminal::WRITE_STDIN,
            &input(r#"{"session_id":"op_foreign"}"#)?,
            &hijack
        )
        .is_err(),
        "stdin must require the Session-owned registry"
    );
    Ok(())
}

/// A RESTART is a different capability even when nothing else changed.
///
/// Same server, same command, same arguments -- but a new start attempt. An
/// approval granted against the previous connection must not carry over, or a
/// user's consent survives the thing they consented to.
#[test]
fn a_reconnect_of_the_same_server_is_a_different_capability(
) -> Result<(), Box<dyn std::error::Error>> {
    let scratch = Scratch::new()?;
    let before = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with(
            "srv_issue",
            mcp_route_at("srv", "issue", &["node", "server.js"], 1),
        ),
    )?;
    let after = prepare_effect(
        &scratch.0,
        "srv_issue",
        &input(r#"{"title":"one"}"#)?,
        &routes_with(
            "srv_issue",
            mcp_route_at("srv", "issue", &["node", "server.js"], 2),
        ),
    )?;
    assert_ne!(
        before.capability, after.capability,
        "a restarted connection must not inherit the previous approval"
    );
    Ok(())
}

#[test]
fn an_edit_copied_from_read_output_applies_to_a_crlf_file_and_keeps_its_endings(
) -> Result<(), String> {
    let scratch = Scratch::new()?;
    let target = scratch.0.join("windows.txt");
    std::fs::write(&target, b"first\r\nsecond\r\nthird\r\n").map_err(|error| error.to_string())?;
    let read_input = input(r#"{"path":"windows.txt"}"#)?;
    let EffectTarget::Read(expected) =
        prepare_effect(&scratch.0, READ_TOOL, &read_input, &no_routes())?.target
    else {
        return Err("read preparation changed effect class".to_owned());
    };
    let shown = String::from_utf8(run_read(&scratch.0, &expected, &read_input)?)
        .map_err(|error| error.to_string())?;
    // Copy two lines exactly as read shows them, without their numbers.
    let copied: Vec<&str> = shown
        .lines()
        .filter_map(|line| line.split_once(": ").map(|(_, text)| text))
        .take(3)
        .skip(1)
        .collect();
    assert_eq!(copied, ["second", "third"], "{shown}");
    let edit = serde_json::json!({
        "path": "windows.txt",
        "old_text": copied.join("\n"),
        "new_text": "second\nnew line\nthird",
    });
    let edit_input = input(&edit.to_string())?;
    let EffectTarget::Edit(expected) =
        prepare_effect(&scratch.0, EDIT_TOOL, &edit_input, &no_routes())?.target
    else {
        return Err("edit preparation changed effect class".to_owned());
    };
    run_edit(&scratch.0, &expected, &edit_input)?;
    assert_eq!(
        std::fs::read(&target).map_err(|error| error.to_string())?,
        b"first\r\nsecond\r\nnew line\r\nthird\r\n",
        "the edit applied and every line kept its CRLF ending"
    );
    Ok(())
}

#[test]
fn unexecuted_approvals_clears_on_dispatch_denial_and_abort() -> Result<(), String> {
    let tracked = UnexecutedApprovals::default();
    let dispatched = ToolCallId::new("call_dispatched").ok_or("call id")?;
    let denied = ToolCallId::new("call_denied").ok_or("call id")?;
    let aborted = ToolCallId::new("call_aborted").ok_or("call id")?;
    let unknown = ToolCallId::new("call_never_pending").ok_or("call id")?;

    assert!(
        !tracked.take_if_unexecuted(&unknown),
        "unknown/untracked calls must fail closed as false (EffectUnknown)"
    );

    // Normal dispatch clears the entry before effect execution.
    tracked.mark_pending(&dispatched);
    tracked.clear(&dispatched);
    assert!(!tracked.take_if_unexecuted(&dispatched));

    // Denial/cancellation clears the entry so it never accumulates.
    tracked.mark_pending(&denied);
    tracked.clear(&denied);
    assert!(!tracked.take_if_unexecuted(&denied));

    // Aborted pending approval reports known_unexecuted=true once and removes the entry.
    tracked.mark_pending(&aborted);
    assert!(tracked.take_if_unexecuted(&aborted));
    assert!(
        tracked.is_empty(),
        "settled/denied/aborted approvals must never accumulate"
    );
    Ok(())
}

mod image;

#[cfg(windows)]
mod windows_authority;
