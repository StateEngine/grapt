//! Regression inputs use the public JSON configuration shape and catalog origins.
use super::*;
use grapt_plugin::PluginOrigin;
use serde_json::{json, Map, Value};

fn executable() -> String {
    std::env::temp_dir()
        .join("grapt-hook-checker")
        .to_string_lossy()
        .into_owned()
}

fn command() -> Value {
    json!({"command": [executable(), "check.js"]})
}

fn source(origin: PluginOrigin, value: Value) -> Result<CatalogDocument, String> {
    let bytes = serde_json::to_vec(&value).map_err(|error| error.to_string())?;
    let value = grapt_config::decode(&bytes, crate::dogfood::catalog_config::bounds()?)
        .map_err(|error| error.to_string())?
        .into_value();
    let directory = std::env::temp_dir().join("grapt-hooks-config-user");
    Ok(CatalogDocument {
        source: directory.join("grapt.jsonc").to_string_lossy().into_owned(),
        directory,
        origin,
        value,
    })
}

fn configured(value: Value) -> Result<HookSettings, String> {
    HookSettings::from_sources(&[source(
        PluginOrigin::UserGlobal,
        json!({"hook_commands": {"gate": value}, "hooks": {"PreToolUse": ["gate"]}}),
    )?])
}

fn refused(result: Result<HookSettings, String>, expected: &str) -> Result<(), String> {
    let reason = result
        .err()
        .ok_or("configuration was unexpectedly accepted")?;
    assert!(reason.contains(expected), "unexpected refusal: {reason}");
    Ok(())
}

fn declarations_json(range: std::ops::Range<usize>) -> Value {
    Value::Object(
        range
            .map(|index| (format!("gate{index}"), command()))
            .collect::<Map<_, _>>(),
    )
}

#[test]
fn absent_hooks_are_empty_for_every_typed_event() -> Result<(), String> {
    let settings = HookSettings::from_sources(&[source(
        PluginOrigin::UserGlobal,
        json!({"model": "unrelated"}),
    )?])?;
    assert_eq!(settings, HookSettings::default());
    for event in Event::ALL {
        assert!(settings.commands(event).is_empty());
    }
    assert_eq!(HookSettings::from_sources(&[])?, settings);
    Ok(())
}

#[test]
fn all_json_event_names_resolve_with_user_selected_argv_and_cwd() -> Result<(), String> {
    let events = [
        ("SessionStart", Event::SessionStart),
        ("UserPromptSubmit", Event::UserPromptSubmit),
        ("PreToolUse", Event::PreToolUse),
        ("PostToolUse", Event::PostToolUse),
        ("Stop", Event::Stop),
        ("Notification", Event::Notification),
        ("SessionEnd", Event::SessionEnd),
        ("PreCompact", Event::PreCompact),
        ("PostCompact", Event::PostCompact),
        ("SubagentStart", Event::SubagentStart),
        ("SubagentStop", Event::SubagentStop),
        ("PermissionRequest", Event::PermissionRequest),
        ("WaitingForInput", Event::WaitingForInput),
    ];
    let bindings = events
        .iter()
        .map(|(name, _)| ((*name).to_owned(), json!(["gate"])))
        .collect::<Map<_, _>>();
    let document = source(
        PluginOrigin::UserGlobal,
        json!({"hook_commands": {"gate": command()}, "hooks": bindings}),
    )?;
    let expected_directory = document.directory.clone();
    let settings = HookSettings::from_sources(&[document])?;
    for (name, event) in events {
        assert_eq!(event.as_str(), name);
        assert_eq!(
            settings.commands(event),
            [HookCommand {
                name: grapt_types::HookId::new("gate").ok_or("invalid hook id")?,
                directory: expected_directory.clone(),
                command: vec![executable(), "check.js".to_owned()],
                timeout_ms: 10_000,
                matcher: None,
                permits_allow: false,
                plugin: None,
            }]
        );
    }
    Ok(())
}

#[test]
fn project_adds_only_allowlisted_bindings_and_never_reorders_user_enforcement() -> Result<(), String>
{
    let project = source(
        PluginOrigin::Project,
        json!({"hooks": {"PreToolUse": ["extra", "gate"], "Stop": ["extra"]}}),
    )?;
    let user = source(
        PluginOrigin::LaunchExplicit,
        json!({
            "hook_commands": {"gate": command(), "extra": command()},
            "hooks": {"PreToolUse": ["gate"]}
        }),
    )?;
    let directory = user.directory.clone();
    let settings = HookSettings::from_sources(&[project, user])?;
    assert_eq!(
        settings
            .commands(Event::PreToolUse)
            .iter()
            .map(|hook| hook.name.as_str())
            .collect::<Vec<_>>(),
        ["gate", "extra"]
    );
    assert_eq!(settings.commands(Event::Stop)[0].name.as_str(), "extra");
    assert!(settings
        .commands(Event::PreToolUse)
        .iter()
        .all(|hook| hook.directory == directory));
    Ok(())
}

#[test]
fn empty_subset_and_repeated_project_bindings_cannot_remove_or_repeat_user_hooks(
) -> Result<(), String> {
    for project in [
        json!({}),
        json!({"hooks": {}}),
        json!({"hooks": {"PreToolUse": []}}),
        json!({"hooks": {"PreToolUse": ["second"]}}),
        json!({"hooks": {"PreToolUse": ["gate", "gate", "second"]}}),
    ] {
        let settings = HookSettings::from_sources(&[
            source(
                PluginOrigin::UserGlobal,
                json!({
                    "hook_commands": {"gate": command(), "second": command()},
                    "hooks": {"PreToolUse": ["gate", "second"]}
                }),
            )?,
            source(PluginOrigin::Project, project)?,
        ])?;
        assert_eq!(
            settings
                .commands(Event::PreToolUse)
                .iter()
                .map(|hook| hook.name.as_str())
                .collect::<Vec<_>>(),
            ["gate", "second"]
        );
    }
    Ok(())
}

#[test]
fn project_cannot_declare_change_or_retime_commands_even_to_identical_values() -> Result<(), String>
{
    for declaration in [
        json!({}),
        json!({"gate": command()}),
        json!({
            "gate": {"command": [executable(), "other.js"], "timeout_ms": 1}
        }),
    ] {
        refused(
            HookSettings::from_sources(&[
                source(
                    PluginOrigin::UserGlobal,
                    json!({"hook_commands": {"gate": command()}}),
                )?,
                source(PluginOrigin::Project, json!({"hook_commands": declaration}))?,
            ]),
            "project config cannot declare hook_commands",
        )?;
    }
    Ok(())
}

#[test]
fn unknown_names_are_refused_in_user_and_project_bindings_without_echoing_them(
) -> Result<(), String> {
    for origin in [
        PluginOrigin::UserGlobal,
        PluginOrigin::LaunchExplicit,
        PluginOrigin::Project,
    ] {
        let result = HookSettings::from_sources(&[source(
            origin,
            json!({
                "hooks": {"PreToolUse": ["secret-undeclared-command"]}
            }),
        )?]);
        let reason = result.err().ok_or("unknown hook was accepted")?;
        assert!(reason.contains("not declared in user hook_commands"));
        assert!(!reason.contains("secret-undeclared-command"));
    }
    Ok(())
}

#[test]
fn malformed_hook_maps_events_and_binding_shapes_fail_closed() -> Result<(), String> {
    for (value, expected) in [
        (json!(null), "document must be an object"),
        (json!({"hook_commands": []}), "named-command object"),
        (json!({"hooks": false}), "event object"),
        (json!({"hooks": null}), "event object"),
        (
            json!({"hooks": {"pre_tool_use": []}}),
            "unknown lifecycle event",
        ),
        (
            json!({"hooks": {"SessionEnded": []}}),
            "unknown lifecycle event",
        ),
        (json!({"hooks": {"Stop": "gate"}}), "array of command names"),
        (
            json!({"hooks": {"Stop": [false]}}),
            "binding must be a command name",
        ),
        (
            json!({"hooks": {"Stop": [{"command": [executable()]}]}}),
            "unknown or repeated field",
        ),
    ] {
        refused(
            HookSettings::from_sources(&[source(PluginOrigin::UserGlobal, value)?]),
            expected,
        )?;
    }
    Ok(())
}

#[test]
fn timeout_defaults_and_inclusive_limits_are_explicit() -> Result<(), String> {
    assert_eq!(
        configured(command())?.commands(Event::PreToolUse)[0].timeout_ms,
        10_000
    );
    for timeout in [1, 60_000] {
        let settings = configured(json!({"command": [executable()], "timeout_ms": timeout}))?;
        assert_eq!(settings.commands(Event::PreToolUse)[0].timeout_ms, timeout);
    }
    for timeout in [
        json!(0),
        json!(-1),
        json!(60_001),
        json!(1.5),
        json!("10000"),
        json!(null),
        json!(true),
    ] {
        refused(
            configured(json!({"command": [executable()], "timeout_ms": timeout})),
            "timeout_ms",
        )?;
    }
    Ok(())
}

#[test]
fn argv_is_literal_nonempty_nul_free_and_has_an_absolute_executable() -> Result<(), String> {
    for (value, expected) in [
        (json!(null), "must be an object"),
        (json!({}), "argv array"),
        (json!({"command": "shell command"}), "argv array"),
        (json!({"command": []}), "1..128 arguments"),
        (json!({"command": [42]}), "must be strings"),
        (json!({"command": [""]}), "absolute path"),
        (json!({"command": ["./checker"]}), "absolute path"),
        (json!({"command": ["checker"]}), "absolute path"),
        (
            json!({"command": [executable(), "bad\u{0}argument"]}),
            "NUL-free",
        ),
        (
            json!({"command": [format!("{}\0", executable())]}),
            "NUL-free",
        ),
        (
            json!({"command": [executable()], "enabled": false}),
            "unknown or repeated field",
        ),
        (
            json!({"command": [executable()], "env": {"secret": "value"}}),
            "unknown or repeated field",
        ),
    ] {
        refused(configured(value), expected)?;
    }
    let settings =
        configured(json!({"command": [executable(), "", "$(literal)", "space argument"]}))?;
    assert_eq!(
        &settings.commands(Event::PreToolUse)[0].command[1..],
        ["", "$(literal)", "space argument"]
    );
    Ok(())
}

#[test]
fn argv_count_and_utf8_byte_limits_include_argument_terminators() -> Result<(), String> {
    let mut argv = vec![Value::String(executable())];
    argv.extend((1..128).map(|_| json!("")));
    configured(json!({"command": argv}))?;
    argv.push(json!(""));
    refused(configured(json!({"command": argv})), "1..128 arguments")?;
    let available = 8192usize
        .checked_sub(executable().len() + 2)
        .ok_or("fixture executable too long")?;
    let exact = "a".repeat(available);
    configured(json!({"command": [executable(), exact]}))?;
    refused(
        configured(json!({"command": [executable(), "a".repeat(available + 1)]})),
        "8192 bytes",
    )?;
    // Multi-byte arguments are measured in UTF-8 bytes, not scalar count.
    refused(
        configured(json!({"command": [executable(), "é".repeat(available)]})),
        "8192 bytes",
    )?;
    Ok(())
}

#[test]
fn names_are_bounded_and_diagnostics_do_not_disclose_command_values() -> Result<(), String> {
    assert!(source(
        PluginOrigin::UserGlobal,
        json!({"hook_commands":{"":command()}})
    )
    .is_err());
    for name in [
        " ".to_owned(),
        "bad/name".to_owned(),
        "é".to_owned(),
        "a".repeat(129),
    ] {
        let mut declarations = Map::new();
        declarations.insert(name, command());
        refused(
            HookSettings::from_sources(&[source(
                PluginOrigin::UserGlobal,
                json!({
                    "hook_commands": declarations
                }),
            )?]),
            "command name must be",
        )?;
    }
    let name = "a".repeat(128);
    let mut declarations = Map::new();
    declarations.insert(name.clone(), command());
    let settings = HookSettings::from_sources(&[source(
        PluginOrigin::UserGlobal,
        json!({
            "hook_commands": declarations, "hooks": {"Stop": [name]}
        }),
    )?])?;
    assert_eq!(settings.commands(Event::Stop).len(), 1);
    let reason = configured(json!({"command": ["secret-relative-executable"]}))
        .err()
        .ok_or("relative executable accepted")?;
    assert!(reason.contains("grapt.jsonc"));
    assert!(!reason.contains("secret-relative-executable"));
    Ok(())
}

#[test]
fn declarations_are_bounded_per_document_and_across_user_sources() -> Result<(), String> {
    HookSettings::from_sources(&[source(
        PluginOrigin::UserGlobal,
        json!({
            "hook_commands": declarations_json(0..208)
        }),
    )?])?;
    refused(
        HookSettings::from_sources(&[source(
            PluginOrigin::UserGlobal,
            json!({
                "hook_commands": declarations_json(0..209)
            }),
        )?]),
        "exceeds 208 declarations",
    )?;
    refused(
        HookSettings::from_sources(&[
            source(
                PluginOrigin::UserGlobal,
                json!({"hook_commands": declarations_json(0..120)}),
            )?,
            source(
                PluginOrigin::LaunchExplicit,
                json!({"hook_commands": declarations_json(120..209)}),
            )?,
        ]),
        "effective hook_commands exceeds 208 declarations",
    )?;
    Ok(())
}

#[test]
fn event_bounds_apply_to_source_arrays_and_the_effective_additive_set() -> Result<(), String> {
    let names = (0..16)
        .map(|index| format!("gate{index}"))
        .collect::<Vec<_>>();
    let user = || {
        source(
            PluginOrigin::UserGlobal,
            json!({
                "hook_commands": declarations_json(0..17), "hooks": {"PreToolUse": names}
            }),
        )
    };
    assert_eq!(
        HookSettings::from_sources(&[user()?])?
            .commands(Event::PreToolUse)
            .len(),
        16
    );
    refused(
        HookSettings::from_sources(&[
            user()?,
            source(
                PluginOrigin::Project,
                json!({"hooks": {"PreToolUse": ["gate16"]}}),
            )?,
        ]),
        "effective hooks event exceeds 16 commands",
    )?;
    refused(
        HookSettings::from_sources(&[
            user()?,
            source(
                PluginOrigin::Project,
                json!({"hooks": {"Stop": vec!["gate0"; 17]}}),
            )?,
        ]),
        "event exceeds 16 bindings",
    )?;
    Ok(())
}

#[test]
fn declarations_cannot_be_redefined_or_rebased_to_a_different_directory() -> Result<(), String> {
    let base = || {
        source(
            PluginOrigin::UserGlobal,
            json!({"hook_commands": {"gate": command()}}),
        )
    };
    HookSettings::from_sources(&[base()?, base()?])?;
    for changed in [
        json!({"command": [executable(), "other.js"]}),
        json!({"command": [executable(), "check.js"], "timeout_ms": 1}),
    ] {
        refused(
            HookSettings::from_sources(&[
                base()?,
                source(
                    PluginOrigin::LaunchExplicit,
                    json!({"hook_commands": {"gate": changed}}),
                )?,
            ]),
            "cannot redefine",
        )?;
    }
    let mut moved = base()?;
    moved.directory = moved.directory.join("another-user-config");
    refused(
        HookSettings::from_sources(&[base()?, moved]),
        "cannot redefine",
    )?;
    let mut relative = base()?;
    relative.directory = PathBuf::from("relative-config");
    refused(
        HookSettings::from_sources(&[relative]),
        "declaration directory must be absolute",
    )?;
    Ok(())
}

#[test]
fn matcher_groups_load_claude_commands_without_granting_project_authority() -> Result<(), String> {
    let user = source(
        PluginOrigin::UserGlobal,
        json!({
            "hook_commands":{"gate":command()},
            "hooks":{
                "PreToolUse":[{"matcher":"Bash|mcp__server__*", "hooks":[{"type":"command","command":"echo checked","timeout":3}]}],
                "PostToolUse":[{"matcher":"Edit|Write", "hooks":["gate"]}]
            }
        }),
    )?;
    let settings = HookSettings::from_sources(std::slice::from_ref(&user))?;
    let hook = &settings.commands(Event::PreToolUse)[0];
    assert!(hook.matches(Some("exec_command"), None));
    assert!(hook.matches(Some("server_search"), Some("mcp__server__search")));
    assert!(!hook.matches(Some("read"), None));
    assert!(hook.matches(None, None));
    assert!(hook.permits_allow);
    assert_eq!(hook.timeout_ms, 3000);
    let project = source(
        PluginOrigin::Project,
        json!({"hooks":{"PreToolUse":[{
            "matcher":"Bash", "hooks":["gate"]
        }]}}),
    )?;
    let mut sources = vec![user, project];
    let settings = HookSettings::from_sources(&sources)?;
    assert!(!settings.commands(Event::PreToolUse)[1].permits_allow);
    let project = source(
        PluginOrigin::Project,
        json!({"hooks":{"PreToolUse":[{
            "hooks":[{"type":"command", "command":"echo unsafe"}]
        }]}}),
    )?;
    sources[1] = project;
    refused(
        HookSettings::from_sources(&sources),
        "project config cannot declare inline",
    )
}
