use super::*;
use crate::dogfood::catalog_config::{bounds, CatalogDocument};
use grapt_plugin::PluginOrigin;

pub(super) fn document(raw: &str, origin: PluginOrigin) -> Result<CatalogDocument, String> {
    Ok(CatalogDocument {
        directory: "/repo".into(),
        source: "/repo/policy.json".into(),
        origin,
        value: grapt_config::decode(raw.as_bytes(), bounds()?)
            .map_err(|error| error.to_string())?
            .into_value(),
    })
}

pub(super) fn policy(documents: &[CatalogDocument], agent: &str) -> Result<ResolvedPolicy, String> {
    Ok(ResolvedPolicy {
        preset: Preset::Maintainer,
        owner_paths: Default::default(),
        workspace: "/repo".into(),
        configured: config::layers(documents, agent)?,
        agent_tools: Vec::new(),
        agent_roots: Default::default(),
        ceilings: Vec::new(),
    })
}

pub(super) fn decide(policy: &ResolvedPolicy, action: &str, resource: &str) -> Effect {
    let check = grapt_permission::PermissionConsequence {
        action: action.into(),
        resource: resource.into(),
    };
    grapt_permission::preset::decide(action, &[resource], &policy.layers(None, &[check])).effect
}

#[test]
fn planning_edits_only_markdown_documents_in_the_literal_plan_directory() -> Result<(), String> {
    let mut policy = policy(&[], "plan")?;
    policy
        .agent_tools
        .push(crate::dogfood::agent_definitions::planning_tools());
    policy.configured.push(config::ConfiguredLayer::planning(
        "/repo/weird?*/plans".into(),
    ));
    assert!(policy.withholds("exec_command"));
    assert!(!policy.withholds(crate::dogfood::tools::WRITE_TOOL));
    for action in ["fs.create", "fs.modify", "fs.overwrite"] {
        assert_eq!(
            decide(&policy, action, "/repo/weird?*/plans/design.md"),
            Effect::Allow
        );
        for path in [
            "/repo/main.rs",
            "/repo/weird?*/.grapt",
            "/repo/weird?*/plans",
            "/repo/weird?*/plans/review",
            "/repo/weird?*/plans/design.rs",
            "/repo/weirdXX/plans/design.md",
            "/repo/weird?*/plans/../source.md",
        ] {
            assert_eq!(decide(&policy, action, path), Effect::Deny);
        }
    }
    assert_eq!(
        decide(&policy, "fs.delete", "/repo/weird?*/plans/design.md"),
        Effect::Deny
    );
    Ok(())
}

#[test]
fn seat01_profile_grants_only_the_bound_stateengine_server_after_launcher_config(
) -> Result<(), String> {
    let profile = document(
        r#"{"permission":{"stateengine_*":{"stateengine/*":"allow","stateengine/*/*":"ask"}}}"#,
        PluginOrigin::UserGlobal,
    )?;
    let launcher = document(
        r#"{"mcp":{"stateengine":{"type":"local","command":["stateengine","mcp"]}}}"#,
        PluginOrigin::LaunchExplicit,
    )?;
    let mut policy = policy(&[profile, launcher], "selected")?;
    policy.preset = Preset::Author;
    for tool in [
        "stateengine_tools",
        "stateengine_identity_whoami",
        "stateengine_msg_read",
        "stateengine_msg_send",
    ] {
        assert_eq!(
            decide(
                &policy,
                &format!("stateengine_{tool}"),
                &format!("stateengine/{tool}")
            ),
            Effect::Allow,
            "the launcher document erased the profile grant for {tool}"
        );
    }
    for (action, resource) in [
        ("other_stateengine_msg_send", "other/stateengine_msg_send"),
        (
            "stateengine_other_stateengine_msg_send",
            "stateengine_other/stateengine_msg_send",
        ),
        (
            "stateengine_other_stateengine_msg_send",
            "stateengine/other/stateengine_msg_send",
        ),
        (
            "stateengine_other_stateengine_msg_send",
            "stateengine\\other/stateengine_msg_send",
        ),
        (
            "stateengine_stateengine_msg_send",
            "other/stateengine_msg_send",
        ),
        ("stateengine_nested_tool", "stateengine/nested/tool"),
    ] {
        assert_eq!(
            decide(&policy, action, resource),
            Effect::Ask,
            "server grant escaped: {action} {resource}"
        );
    }
    Ok(())
}

#[test]
fn sec03_ordered_config_rules_and_selected_agent_reach_the_shared_stack() -> Result<(), String> {
    let docs = [document(
        r#"{"permission":{"bash":{"*":"allow","rm *":"deny"},"edit":"ask"},"agent":{"selected":{"permission":{"read":"deny"}},"other":{"permission":"deny"}}}"#,
        PluginOrigin::UserGlobal,
    )?];
    let selected = policy(&docs, "selected")?;
    assert_eq!(decide(&selected, "shell", "echo ordinary"), Effect::Allow);
    assert_eq!(decide(&selected, "shell", "rm ordinary"), Effect::Deny);
    assert_eq!(decide(&selected, "read", "/repo/file"), Effect::Deny);
    assert_eq!(decide(&selected, "fs.modify", "/repo/file"), Effect::Ask);
    assert_eq!(
        decide(&policy(&docs, "unselected")?, "read", "/repo/file"),
        Effect::Allow
    );
    assert!(selected.withholds("read"));
    assert!(!selected.withholds("exec_command"));
    Ok(())
}

#[test]
fn sec03_only_user_rules_can_lift_asks_and_authority_files_still_ask() -> Result<(), String> {
    for (origin, expected) in [
        (PluginOrigin::UserGlobal, Effect::Allow),
        (PluginOrigin::Project, Effect::Ask),
    ] {
        let policy = policy(
            &[document(r#"{"permission":{"shell":"allow"}}"#, origin)?],
            "selected",
        )?;
        assert_eq!(decide(&policy, "shell", "rm -rf build"), expected);
        assert_eq!(
            decide(&policy, "fs.overwrite", "/repo/grapt.json"),
            Effect::Ask
        );
    }
    Ok(())
}

#[test]
fn reclassifying_user_rules_as_project_cannot_widen_a_decision() -> Result<(), String> {
    let restriction = |effect| match effect {
        Effect::Allow => 0,
        Effect::Ask => 1,
        Effect::Deny => 2,
    };
    for first in ["allow", "ask", "deny"] {
        for second in ["allow", "ask", "deny"] {
            for preset in Preset::ALL {
                let raw = [
                    format!(r#"{{"permission":"{first}"}}"#),
                    format!(r#"{{"permission":"{second}"}}"#),
                ];
                let mut before = policy(
                    &[
                        document(&raw[0], PluginOrigin::UserGlobal)?,
                        document(&raw[1], PluginOrigin::LaunchExplicit)?,
                    ],
                    "selected",
                )?;
                before.preset = preset;
                for mask in 1..4 {
                    let mut after = policy(
                        &[
                            document(
                                &raw[0],
                                if mask & 1 == 0 {
                                    PluginOrigin::UserGlobal
                                } else {
                                    PluginOrigin::Project
                                },
                            )?,
                            document(
                                &raw[1],
                                if mask & 2 == 0 {
                                    PluginOrigin::LaunchExplicit
                                } else {
                                    PluginOrigin::Project
                                },
                            )?,
                        ],
                        "selected",
                    )?;
                    after.preset = preset;
                    for (action, resource) in [
                        ("shell", "rm -rf build"),
                        ("read", "/repo/file"),
                        ("fs.modify", "/repo/file"),
                    ] {
                        assert!(restriction(decide(&after, action, resource)) >= restriction(decide(&before, action, resource)),
                            "source demotion widened {preset:?} {first}/{second} mask={mask} {action}");
                    }
                }
            }
        }
    }
    Ok(())
}

#[test]
fn sec03_ancestor_decisions_survive_child_rules_and_saved_approvals() -> Result<(), String> {
    for inherited in ["ask", "deny"] {
        let parent = policy(
            &[document(
                &format!(r#"{{"permission":{{"edit":"{inherited}"}}}}"#),
                PluginOrigin::UserGlobal,
            )?],
            "parent",
        )?;
        let mut child = policy(
            &[document(
                r#"{"permission":{"edit":"allow"}}"#,
                PluginOrigin::UserGlobal,
            )?],
            "child",
        )?;
        child.ceilings.push(parent.ceiling());
        let expected = if inherited == "deny" {
            Effect::Deny
        } else {
            Effect::Ask
        };
        assert_eq!(decide(&child, "fs.overwrite", "/repo/file"), expected);
        let checks = [grapt_permission::PermissionConsequence {
            action: "fs.overwrite".into(),
            resource: "/repo/file".into(),
        }];
        let mut layers = child.layers(None, &checks);
        layers.push(Layer::approvals("fs.overwrite", &["/repo/file".into()]));
        assert_eq!(
            grapt_permission::preset::decide("fs.overwrite", &["/repo/file"], &layers).effect,
            expected
        );
    }
    // Preserve an ancestor's own effective allow: its earlier preset ask must
    // not be flattened into a stricter ceiling than the parent actually had.
    let mut parent = policy(
        &[document(
            r#"{"permission":{"edit":"allow"}}"#,
            PluginOrigin::UserGlobal,
        )?],
        "parent",
    )?;
    parent.preset = Preset::Author;
    let mut child = policy(
        &[document(
            r#"{"permission":{"edit":"allow"}}"#,
            PluginOrigin::UserGlobal,
        )?],
        "child",
    )?;
    child.ceilings.push(parent.ceiling());
    assert_eq!(decide(&child, "fs.overwrite", "/repo/file"), Effect::Allow);
    assert_eq!(decide(&child, "fs.modify", "/repo/file"), Effect::Allow);
    Ok(())
}

#[test]
fn sec03_bad_or_excessive_rules_refuse_without_echoing_values() -> Result<(), String> {
    for raw in [
        r#"{"permission":null}"#,
        r#"{"permission":{"shell":"PRIVATE_INVALID_EFFECT"}}"#,
        r#"{"permission":{"shell":[]}}"#,
    ] {
        let docs = [document(raw, PluginOrigin::UserGlobal)?];
        let error = policy(&docs, "selected")
            .err()
            .ok_or("invalid rules were silently ignored")?;
        assert!(error.contains("/repo/policy.json"));
        assert!(!error.contains("PRIVATE_INVALID_EFFECT"));
    }
    let actions = (0..1025)
        .map(|index| format!("\"tool{index}\":\"deny\""))
        .collect::<Vec<_>>()
        .join(",");
    let docs = [document(
        &format!("{{\"permission\":{{{actions}}}}}"),
        PluginOrigin::UserGlobal,
    )?];
    assert!(policy(&docs, "selected").is_err());
    Ok(())
}

#[test]
fn sec03_project_ask_cannot_be_raised_by_a_global_allow_or_saved_approval() -> Result<(), String> {
    let policy = policy(
        &[
            document(
                r#"{"permission":{"shell":"allow"}}"#,
                PluginOrigin::UserGlobal,
            )?,
            document(r#"{"permission":{"shell":"ask"}}"#, PluginOrigin::Project)?,
        ],
        "selected",
    )?;
    assert_eq!(decide(&policy, "shell", "echo ordinary"), Effect::Ask);
    let checks = [grapt_permission::PermissionConsequence {
        action: "shell".into(),
        resource: "echo ordinary".into(),
    }];
    let mut layers = policy.layers(None, &checks);
    layers.push(Layer::approvals("shell", &["echo ordinary".into()]));
    assert_eq!(
        grapt_permission::preset::decide("shell", &["echo ordinary"], &layers).effect,
        Effect::Ask
    );
    Ok(())
}

#[test]
fn sec03_a_configured_deny_is_stricter_than_the_authority_file_ask() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"edit":"deny"}}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    assert_eq!(
        decide(&policy, "fs.overwrite", "/repo/grapt.json"),
        Effect::Deny
    );
    assert_eq!(
        decide(&policy, "fs.modify", "/repo/service.conf"),
        Effect::Deny
    );
    Ok(())
}
