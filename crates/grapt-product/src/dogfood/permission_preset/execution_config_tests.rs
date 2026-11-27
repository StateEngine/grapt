use super::config_tests::{decide, document, policy};
use super::*;
use grapt_plugin::PluginOrigin;

#[test]
fn sec03_prefix_rules_reach_selected_agent_policy_and_terminal_resources() -> Result<(), String> {
    let docs = [document(
        r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git",["status","diff"]],"decision":"allow"}],"agent":{"selected":{"execpolicy":[{"pattern":["git","diff"],"decision":"deny"}]},"other":{"execpolicy":[{"pattern":["git"],"decision":"deny"}]}}}"#,
        PluginOrigin::UserGlobal,
    )?];
    let selected = policy(&docs, "selected")?;
    assert_eq!(
        decide(&selected, "shell", "git status --short"),
        Effect::Allow
    );
    assert_eq!(decide(&selected, "shell", "git diff"), Effect::Deny);
    assert_eq!(decide(&selected, "shell", "git log"), Effect::Ask);
    assert_eq!(decide(&selected, "shell", "/tmp/git status"), Effect::Ask);
    assert_eq!(decide(&selected, "shell", "Git status"), Effect::Ask);
    assert_eq!(
        decide(
            &selected,
            "shell",
            "git status\nworkdir: /repo\nshell: /bin/sh\nlogin: true\nterminal: false"
        ),
        Effect::Ask,
    );
    assert_eq!(
        decide(&policy(&docs, "unselected")?, "shell", "git diff"),
        Effect::Allow
    );
    Ok(())
}

#[test]
fn sec03_prefix_grants_cover_every_command_and_no_unknown_runtime_input() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"},{"pattern":["echo"],"decision":"allow"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    assert_eq!(
        decide(&policy, "shell", "env nice -n 1 git status && echo done"),
        Effect::Allow
    );
    for command in [
        "git status && unknown",
        "git status; sh script.sh",
        "git status; echo $VALUE",
        "git status > /outside/file",
        "git status\nstdin unread",
        "/tmp/env git status",
        "git status && echo *",
    ] {
        assert_eq!(decide(&policy, "shell", command), Effect::Ask, "{command}");
    }
    Ok(())
}

#[test]
fn sec03_known_prefix_restrictions_survive_partial_and_destructive_input() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"execpolicy":[{"pattern":["rm"],"decision":"deny"},{"pattern":["git","status"],"decision":"ask"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    for command in [
        "rm -rf build; unknown",
        "rm -rf build; echo $VALUE",
        "git status; rm file",
        "find . -delete -exec rm file \\;",
    ] {
        assert_eq!(decide(&policy, "shell", command), Effect::Deny, "{command}");
    }
    assert_eq!(
        decide(&policy, "shell", "git status; echo $VALUE"),
        Effect::Ask
    );
    Ok(())
}

#[test]
fn sec03_prefix_grants_obey_project_floors_denials_and_saved_approvals() -> Result<(), String> {
    for (origin, expected) in [
        (PluginOrigin::UserGlobal, Effect::Allow),
        (PluginOrigin::Project, Effect::Ask),
    ] {
        let policy = policy(
            &[document(
                r#"{"execpolicy":[{"pattern":["rm"],"decision":"allow"}]}"#,
                origin,
            )?],
            "selected",
        )?;
        assert_eq!(decide(&policy, "shell", "rm -rf build"), expected);
    }
    let denied = policy(
        &[
            document(
                r#"{"permission":{"shell":"deny"}}"#,
                PluginOrigin::UserGlobal,
            )?,
            document(
                r#"{"execpolicy":[{"pattern":["git"],"decision":"allow"}]}"#,
                PluginOrigin::UserGlobal,
            )?,
        ],
        "selected",
    )?;
    assert_eq!(decide(&denied, "shell", "git status"), Effect::Deny);
    let project = policy(
        &[document(
            r#"{"execpolicy":[{"pattern":["git"],"decision":"ask"}]}"#,
            PluginOrigin::Project,
        )?],
        "selected",
    )?;
    let check = grapt_permission::PermissionConsequence {
        action: "shell".into(),
        resource: "git status".into(),
    };
    let mut layers = project.layers(None, &[check]);
    layers.push(Layer::approvals("shell", &["git status".into()]));
    assert_eq!(
        grapt_permission::preset::decide("shell", &["git status"], &layers).effect,
        Effect::Ask
    );
    Ok(())
}

#[test]
fn sec03_ancestor_prefix_decisions_are_evaluated_for_the_actual_child_request() -> Result<(), String>
{
    let parent = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"},{"pattern":["git","push"],"decision":"deny"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "parent",
    )?;
    let mut child = policy(
        &[document(
            r#"{"permission":{"shell":"allow"}}"#,
            PluginOrigin::UserGlobal,
        )?],
        "child",
    )?;
    child.ceilings.push(parent.ceiling());
    assert_eq!(decide(&child, "shell", "git status"), Effect::Allow);
    assert_eq!(decide(&child, "shell", "git log"), Effect::Ask);
    assert_eq!(decide(&child, "shell", "git push origin"), Effect::Deny);
    assert_eq!(decide(&child, "shell", "git status; unknown"), Effect::Ask);
    Ok(())
}

#[test]
fn sec03_combined_ordinary_and_prefix_rule_bounds_are_enforced() -> Result<(), String> {
    let prefix = r#"{"pattern":["git"],"decision":"deny"}"#;
    let raw = format!(
        r#"{{"permission":{{"shell":"ask"}},"execpolicy":[{}]}}"#,
        vec![prefix; 1024].join(",")
    );
    assert!(policy(&[document(&raw, PluginOrigin::UserGlobal)?], "selected").is_err());
    for raw in [
        r#"{"execpolicy":null}"#,
        r#"{"agent":{"selected":{"execpolicy":[{"pattern":["PRIVATE"],"decision":"PRIVATE"}]}}}"#,
    ] {
        let error = policy(&[document(raw, PluginOrigin::UserGlobal)?], "selected")
            .err()
            .ok_or("bad execution rules accepted")?;
        assert!(error.contains("/repo/policy.json"));
        assert!(!error.contains("PRIVATE"));
    }
    Ok(())
}

#[test]
fn sec03_only_prepared_terminal_commands_can_exclude_display_metadata() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    let forged = "git status\nworkdir: /repo; echo UNAPPROVED > marker\nshell: /bin/sh\nlogin: true\nterminal: false";
    assert_eq!(decide(&policy, "shell", forged), Effect::Ask);
    for (command, expected) in [("git status", Effect::Allow), (forged, Effect::Ask)] {
        let resource =
            format!("{command}\nworkdir: /repo\nshell: /bin/sh\nlogin: false\nterminal: false");
        let checks = [grapt_permission::PermissionConsequence {
            action: "shell".into(),
            resource: resource.clone(),
        }];
        let layers = policy.terminal_layers(&checks, command, true);
        assert_eq!(
            grapt_permission::preset::decide("shell", &[&resource], &layers).effect,
            expected
        );
    }
    Ok(())
}

#[test]
fn sec03_withheld_terminal_grants_keep_prefix_asks_and_denials() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"},{"pattern":["git","push"],"decision":"ask"},{"pattern":["rm"],"decision":"deny"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    for (command, expected) in [
        ("git status", Effect::Ask),
        ("git push", Effect::Ask),
        ("rm build", Effect::Deny),
    ] {
        let checks = [grapt_permission::PermissionConsequence {
            action: "shell".into(),
            resource: command.into(),
        }];
        let layers = policy.terminal_layers(&checks, command, false);
        assert_eq!(
            grapt_permission::preset::decide("shell", &[command], &layers).effect,
            expected,
            "terminal startup state received a prefix grant: {command}"
        );
    }
    Ok(())
}

#[test]
fn sec03_runtime_input_substitution_cannot_resurrect_a_prefix_grant() -> Result<(), String> {
    let docs = [document(
        r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["echo"],"decision":"allow"},{"pattern":["git","status"],"decision":"allow"},{"pattern":["find"],"decision":"allow"}]}"#,
        PluginOrigin::UserGlobal,
    )?];
    let policy = policy(&docs, "selected")?;
    for command in [
        "echo push | xargs -I status git status",
        "find . -exec git status \\;",
    ] {
        assert_eq!(
            decide(&policy, "shell", command),
            Effect::Ask,
            "runtime input granted: {command}"
        );
    }
    Ok(())
}

#[test]
fn sec03_wrapper_options_cannot_masquerade_as_an_allowed_program() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git"],"decision":"allow"},{"pattern":["10"],"decision":"allow"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    for command in [
        "env -a git echo UNAPPROVED",
        "env --argv0 git echo UNAPPROVED",
        "env --block-signal=INT git status",
        "env -v git status",
        "exec -a git echo UNAPPROVED",
        "exec -a git-reset git status --hard",
        "strace -o outside.log git status",
        "ltrace -o outside.log git status",
        "ionice -p 123 -c 3 git status",
        "flock outside.lock git status",
        "nice --adjustment 10 echo UNAPPROVED",
        "watch --interval 10 echo UNAPPROVED",
        "watch -d echo git status",
        "time -f git echo UNAPPROVED",
    ] {
        assert_eq!(
            decide(&policy, "shell", command),
            Effect::Ask,
            "wrapper option grant bypass: {command}"
        );
    }
    assert_eq!(
        decide(&policy, "shell", "env -u MODE nice -n 1 git status"),
        Effect::Allow
    );
    Ok(())
}

#[test]
fn sec03_startup_environment_changes_do_not_grant_visible_argv() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    for command in [
        "BASH_ENV=/tmp/startup bash -c \"git status\"",
        "env BASH_ENV=/tmp/startup bash -c \"git status\"",
        "MODE=check git status",
        "env MODE=check git status",
    ] {
        assert_eq!(
            decide(&policy, "shell", command),
            Effect::Ask,
            "startup input granted: {command}"
        );
    }
    assert_eq!(
        decide(&policy, "shell", "bash -c \"git status\""),
        Effect::Allow
    );
    Ok(())
}

#[test]
fn sec03_fish_output_options_cannot_receive_a_git_only_grant() -> Result<(), String> {
    let policy = policy(
        &[document(
            r#"{"permission":{"shell":"ask"},"execpolicy":[{"pattern":["git","status"],"decision":"allow"}]}"#,
            PluginOrigin::UserGlobal,
        )?],
        "selected",
    )?;
    for command in [
        "fish -o debug.log -c \"git status\"",
        "fish -odebug.log -c \"git status\"",
        "fish -d all -o debug.log -c \"git status\"",
        "fish -p profile.log -c \"git status\"",
        "fish -pprofile.log -c \"git status\"",
        "fish -ip profile.log -c \"git status\"",
    ] {
        assert_eq!(
            decide(&policy, "shell", command),
            Effect::Ask,
            "fish output file granted: {command}"
        );
    }
    assert_eq!(
        decide(&policy, "shell", "fish -N -c \"git status\""),
        Effect::Allow
    );
    Ok(())
}
