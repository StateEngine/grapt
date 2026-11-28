use super::*;

#[test]
fn file_grant_cannot_fall_back_to_an_msys_shell() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let workspace = scratch.0.join("workspace");
    let service = scratch.0.join("service");
    let msys = scratch.0.join("msys");
    for directory in [&workspace, &service, &msys] {
        std::fs::create_dir(directory).map_err(|error| error.to_string())?;
    }
    let target = scratch.0.join("requested.txt");
    let shell = msys.join(if cfg!(windows) { "bash.exe" } else { "bash" });
    for (path, contents) in [
        (&target, "unchanged"),
        (&shell, "fixture; never executed"),
        (&msys.join("msys-2.0.dll"), "fixture; never loaded"),
        (
            &workspace.join("grapt.jsonc"),
            r#"{"sandbox":{"enabled":true,"network":false}}"#,
        ),
    ] {
        std::fs::write(path, contents).map_err(|error| error.to_string())?;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&shell, std::fs::Permissions::from_mode(0o700))
            .map_err(|error| error.to_string())?;
    }
    let session: SessionId = "ses_msys_file_grant"
        .parse()
        .map_err(|error| format!("{error}"))?;
    let mut arguments = serde_json::json!({"cmd":"exit 0","shell":shell});
    let input = |value: &serde_json::Value| {
        BoundedToolInput::new(serde_json::to_vec(value).map_err(|error| error.to_string())?)
            .map_err(|error| error.to_string())
    };
    let ordinary = prepare_session(
        &service,
        &workspace,
        terminal::EXEC_COMMAND,
        &input(&arguments)?,
        &[],
        &session,
        false,
    )?;
    assert_eq!(ordinary.action, grapt_permission::SANDBOX_MSYS_ACTION);
    arguments["path"] = serde_json::json!(target);
    for session_granted in [false, true] {
        let refusal = prepare_session(
            &service,
            &workspace,
            sandbox_access::TOOL,
            &input(&arguments)?,
            &[],
            &session,
            session_granted,
        )
        .err()
        .ok_or("A file grant escaped containment through MSYS preparation")?;
        assert!(
            refusal.contains("Sandbox cannot enforce this file grant for an MSYS command"),
            "{refusal}"
        );
    }
    assert_eq!(
        std::fs::read_to_string(target).map_err(|error| error.to_string())?,
        "unchanged"
    );
    Ok(())
}

#[tokio::test]
async fn withdrawn_consent_refuses_a_prepared_retry_before_native_startup() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let path = scratch.0.join("requested.txt");
    std::fs::write(&path, b"unchanged").map_err(|error| error.to_string())?;
    let plan: sandbox::Plan = serde_json::from_value(serde_json::json!({
        "readable":[scratch.0],"writable":[],"protected":[],"network":false,
        "git_metadata_read_only":false
    }))
    .map_err(|error| error.to_string())?;
    let target = grapt_sandbox::WriteTarget::resolve(&plan.policy(), &path)?;
    let mut environment = Environment::parent()?;
    environment.1 = Some(plan);
    let input = BoundedToolInput::new(br#"{"cmd":"exit 0","yield_time_ms":250}"#.to_vec())
        .map_err(|error| error.to_string())?;
    let prepared =
        terminal::prepare_exec(&scratch.0, &input, environment.with_write_target(target)?)?;
    let EffectTarget::TerminalExec(mut spec) = prepared.target else {
        return Err("Retry preparation did not produce a terminal command".into());
    };
    let project =
        grapt_types::ProjectId::new("prj_withdraw_prepared").ok_or("invalid project identity")?;
    let session = "ses_withdraw_prepared"
        .parse()
        .map_err(|error| format!("session: {error}"))?;
    let operation = grapt_types::OperationId::new("op_withdraw_prepared")
        .ok_or("invalid operation identity")?;
    let terminals = TerminalRegistry::default();
    terminals.prepare_grants(&project, &mut spec)?;
    terminals
        .revoke_write_grant(&project, Some(&session), &path_text(&path), None)
        .await?;
    let Err(refusal) = terminals.exec(&session, operation, spec).await else {
        return Err("A withdrawn approval started its prepared command".into());
    };
    assert!(
        refusal
            .detail
            .contains("consent changed before command startup"),
        "{}",
        refusal.detail
    );
    assert_eq!(
        std::fs::read(path).map_err(|error| error.to_string())?,
        b"unchanged"
    );
    Ok(())
}

fn policy(raws: &[&str]) -> Result<Policy, String> {
    let documents = raws
        .iter()
        .map(|raw| {
            grapt_config::decode(raw.as_bytes(), crate::dogfood::catalog_config::bounds()?)
                .map(|value| value.into_value())
                .map_err(|error| error.to_string())
        })
        .collect::<Result<Vec<_>, _>>()?;
    Policy::fold(documents.iter())
}

fn parent(pairs: &[(&str, &str)]) -> Vec<(OsString, OsString)> {
    pairs
        .iter()
        .map(|(k, v)| ((*k).into(), (*v).into()))
        .collect()
}

#[test]
fn credentials_cannot_be_restored_by_overrides_or_include_filters() -> Result<(), String> {
    let policy = policy(&[
        r#"{"provider":{"credential":"env:PROVIDER_SESSION"},"shell_environment_policy":{
      "set":{"mixed_Key":"secret-sentinel","some_SECRET":"secret-sentinel","auth_toKen":"secret-sentinel",
      "pAssWord":"secret-sentinel","provider_session":"secret-sentinel","BUILD_MODE":"checked"},"include_only":["*"]}}"#,
    ])?;
    for windows in [false, true] {
        let env = Environment::from_parent(
            parent(&[
                ("PATH", "developer-path"),
                ("HOME", "developer-home"),
                ("API_KEY", "secret-sentinel"),
                ("MY_SECRET", "secret-sentinel"),
                ("ACCESS_TOKEN", "secret-sentinel"),
                ("PASSWORD", "secret-sentinel"),
                ("PROVIDER_SESSION", "secret-sentinel"),
            ]),
            &policy,
            windows,
        )?;
        assert!(
            env.iter().all(|(_, v)| v != OsStr::new("secret-sentinel")),
            "credential escaped final filter"
        );
        assert_eq!(env.names(), ["BUILD_MODE", "HOME", "PATH"]);
        assert!(!format!("{env:?}").contains("checked"));
        assert!(!format!("{env:?}").contains("developer-path"));
    }
    Ok(())
}

#[test]
fn launcher_identity_cannot_be_restored_by_overrides_or_include_filters() -> Result<(), String> {
    let policy = policy(
        &[r#"{"shell_environment_policy":{"include_only":["*"],"set":{
        "STATEENGINE_AGENT_ID":"identity-sentinel",
        "SECODE_STATEENGINE_SEAT_LAUNCH_PIN":"identity-sentinel",
        "secode_session_id":"identity-sentinel",
        "BUILD_MODE":"checked"}}}"#],
    )?;
    for windows in [false, true] {
        let env = Environment::from_parent(
            parent(&[
                ("stateengine_agent_id", "identity-sentinel"),
                ("SECODE_STATEENGINE_LIFECYCLE_ID", "identity-sentinel"),
                ("SECODE_SESSION_ID", "identity-sentinel"),
                ("PATH", "developer-path"),
                ("HOME", "developer-home"),
            ]),
            &policy,
            windows,
        )?;
        assert_eq!(env.names(), ["BUILD_MODE", "HOME", "PATH"]);
        assert!(env
            .iter()
            .all(|(_, value)| value != OsStr::new("identity-sentinel")));
    }
    Ok(())
}

#[test]
fn loaded_credential_name_stays_private_after_configuration_reference_is_removed(
) -> Result<(), String> {
    let mut policy =
        policy(&[r#"{"shell_environment_policy":{"set":{"UNUSUAL_AUTH":"sentinel"}}}"#])?;
    policy.credentials = vec!["unusual_auth".into()];
    let env = Environment::from_parent(
        parent(&[("UNUSUAL_AUTH", "sentinel"), ("PATH", "tools")]),
        &policy,
        false,
    )?;
    assert_eq!(
        env.names(),
        ["PATH"],
        "credential grant name escaped final filter"
    );
    Ok(())
}

#[test]
fn ordered_config_filters_and_overrides_follow_codex_order() -> Result<(), String> {
    let policy = policy(&[
        r#"{"shell_environment_policy":{"inherit":"none","set":{"PATH":"base","BASE":"kept"},"exclude":["*NO*"]}}"#,
        r#"{"shell_environment_policy":{"inherit":"all","exclude":["remove_*"],"set":{"PATH":"project","RESTORED":"override"},"include_only":["p?th","BASE","RESTORED"]}}"#,
    ])?;
    let env = Environment::from_parent(
        parent(&[
            ("PATH", "ambient"),
            ("remove_me", "gone"),
            ("OTHER", "gone"),
        ]),
        &policy,
        false,
    )?;
    assert_eq!(
        env.0.get(OsStr::new("PATH")),
        Some(&OsString::from("project"))
    );
    assert_eq!(env.names(), ["BASE", "PATH", "RESTORED"]);
    let none = policy_for_none()?;
    assert_eq!(
        Environment::from_parent(parent(&[("PATH", "gone")]), &none, false)?.names(),
        ["ONLY"]
    );
    Ok(())
}

fn policy_for_none() -> Result<Policy, String> {
    policy(&[r#"{"shell_environment_policy":{"inherit":"none","set":{"ONLY":"explicit"}}}"#])
}

#[test]
fn core_and_windows_names_preserve_path_and_respect_case() -> Result<(), String> {
    let policy =
        policy(&[r#"{"shell_environment_policy":{"inherit":"core","set":{"PATH":"override"}}}"#])?;
    for windows in [false, true] {
        let env = Environment::from_parent(
            parent(&[
                ("Path", "parent"),
                ("HOME", "home"),
                ("SystemRoot", "Windows"),
                ("CUSTOM", "gone"),
            ]),
            &policy,
            windows,
        )?;
        assert!(!env.names().contains(&"CUSTOM".into()));
        assert_eq!(
            env.0.get(OsStr::new("PATH")),
            Some(&OsString::from("override"))
        );
        assert_eq!(
            env.names()
                .iter()
                .filter(|k| k.eq_ignore_ascii_case("path"))
                .count(),
            if windows { 1 } else { 2 }
        );
        assert_eq!(env.names().contains(&"SystemRoot".into()), windows);
    }
    Ok(())
}

#[test]
fn malformed_or_oversized_configuration_refuses_without_echoing_values() -> Result<(), String> {
    for raw in [
        r#"{"shell_environment_policy":null}"#,
        r#"{"shell_environment_policy":{"inherit":"sentinel-private"}}"#,
        r#"{"shell_environment_policy":{"set":{"EXAMPLE":12}}}"#,
        r#"{"shell_environment_policy":{"exclude":[12]}}"#,
        r#"{"shell_environment_policy":{"ignore_default_excludes":true}}"#,
        r#"{"shell_environment_policy":{"set":{"BAD=NAME":"sentinel-private"}}}"#,
    ] {
        let Err(error) = policy(&[raw]) else {
            return Err("malformed environment policy accepted".into());
        };
        assert!(!error.contains("sentinel-private"));
    }
    let huge = "x".repeat(MAX_VALUE_BYTES + 1);
    assert!(Environment::from_parent(
        vec![("EXAMPLE".into(), huge.into())],
        &Policy::default(),
        false
    )
    .is_err());
    assert!(Environment::from_parent(
        (0..=MAX_ENTRIES).map(|n| (format!("VAR_{n}").into(), "x".into())),
        &Policy::default(),
        false
    )
    .is_err());
    Ok(())
}

#[cfg(unix)]
#[test]
fn non_utf8_values_survive_without_non_utf8_name_bypass() -> Result<(), String> {
    use std::os::unix::ffi::OsStringExt;
    let bytes = OsString::from_vec(vec![0xff]);
    let env = Environment::from_parent(
        vec![
            ("SAFE".into(), bytes.clone()),
            (bytes.clone(), "discard".into()),
        ],
        &Policy::default(),
        false,
    )?;
    assert_eq!(env.0.get(OsStr::new("SAFE")), Some(&bytes));
    assert_eq!(env.names(), ["SAFE"]);
    Ok(())
}

#[tokio::test]
async fn the_real_shell_resolves_parent_cargo_and_git_without_secret_variables(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let mut inherited: Vec<_> = std::env::vars_os().collect();
    inherited.push(("GRAPT_PROBE_SECRET".into(), "secret-sentinel".into()));
    let env = Environment::from_parent(inherited, &Policy::default(), cfg!(windows))?;
    let command = if cfg!(windows) {
        "if defined GRAPT_PROBE_SECRET (exit /b 8) else (cargo --version && git --version)"
    } else {
        "test -z \"${GRAPT_PROBE_SECRET+x}\" && cargo --version && git --version"
    };
    let input = BoundedToolInput::new(
        serde_json::to_vec(&serde_json::json!({"command":command,"timeout_ms":60000}))
            .map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let output = run_shell(
        &scratch.0,
        command,
        &shell_program()?,
        &env,
        &input,
        &[],
        None,
    )
    .await?
    .command_text()?;
    let text = String::from_utf8_lossy(&output);
    assert!(
        text.contains("cargo "),
        "cargo did not resolve from the inherited PATH"
    );
    assert!(
        text.contains("git version "),
        "git did not resolve from the inherited PATH"
    );
    Ok(())
}

#[tokio::test]
async fn prepared_shell_uses_project_policy_and_keeps_the_snapshot() -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let config = scratch.0.join("grapt.jsonc");
    let settings = serde_json::json!({"shell_environment_policy":{"inherit":"none","set":{"SC_ENV_MARKER":"project"}}});
    #[cfg(windows)]
    let settings = {
        let mut settings = settings;
        settings["shell_environment_policy"]["set"]["SystemRoot"] = std::env::var("SystemRoot")
            .map_err(|_| "native SystemRoot missing")?
            .into();
        settings
    };
    std::fs::write(
        &config,
        serde_json::to_vec(&settings).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let command = if cfg!(windows) {
        "echo %SC_ENV_MARKER%"
    } else {
        "printf '%s' \"$SC_ENV_MARKER\""
    };
    let input = BoundedToolInput::new(
        serde_json::to_vec(&serde_json::json!({"command":command})).map_err(|e| e.to_string())?,
    )
    .map_err(|e| e.to_string())?;
    let effect = prepare_shell(&scratch.0, &scratch.0, &input, &[])?;
    std::fs::write(
        &config,
        r#"{"shell_environment_policy":{"set":{"SC_ENV_MARKER":"changed"}}}"#,
    )
    .map_err(|e| e.to_string())?;
    let EffectTarget::Shell {
        program,
        mut environment,
        ..
    } = effect.target
    else {
        return Err("wrong shell target".into());
    };
    environment.without_sandbox();
    assert!(environment.names().contains(&"SC_ENV_MARKER".into()));
    let output = run_shell(
        &scratch.0,
        command,
        &program,
        &environment,
        &input,
        &[],
        None,
    )
    .await?
    .command_text()?;
    assert_eq!(String::from_utf8_lossy(&output).trim(), "project");
    Ok(())
}

#[tokio::test]
async fn agent_commands_default_to_noninteractive_and_allow_explicit_overrides(
) -> Result<(), String> {
    let scratch = super::super::tests::Scratch::new()?;
    let registry = terminal::TerminalRegistry::default();
    let session: SessionId = "ses_agent_environment"
        .parse()
        .map_err(|e| format!("{e}"))?;
    let modes: &[bool] = if cfg!(windows) {
        &[false]
    } else {
        &[false, true]
    };
    for tty in modes {
        for overridden in [false, true] {
            let mut settings = if overridden {
                serde_json::json!({"PAGER":"chosen","GIT_PAGER":"chosen","CI":"chosen","NO_COLOR":"chosen","TERM":"chosen"})
            } else {
                serde_json::json!({})
            };
            settings["SC_ENV_MARKER"] = "from-policy".into();
            settings["SC_ENV_PASSWORD"] = "secret-sentinel".into();
            std::fs::write(
                scratch.0.join("grapt.jsonc"),
                serde_json::to_vec(
                    &serde_json::json!({"shell_environment_policy":{"inherit":"core","set":settings}}),
                )
                .map_err(|e| e.to_string())?,
            )
            .map_err(|e| e.to_string())?;
            let cmd = if cfg!(windows) {
                "if defined SC_ENV_PASSWORD (exit /b 9) else (echo %PAGER%:%GIT_PAGER%:%CI%:%NO_COLOR%:%TERM% && echo %SC_ENV_MARKER% && cargo --version && git --version)"
            } else {
                "test -z \"${SC_ENV_PASSWORD+x}\" && printf '%s:%s:%s:%s:%s\\n%s\\n' \"$PAGER\" \"$GIT_PAGER\" \"$CI\" \"$NO_COLOR\" \"$TERM\" \"$SC_ENV_MARKER\" && cargo --version && git --version"
            };
            let input = BoundedToolInput::new(serde_json::to_vec(&serde_json::json!({"cmd":cmd,"tty":tty,"yield_time_ms":30000,"timeout_ms":60000})).map_err(|e|e.to_string())?).map_err(|e|e.to_string())?;
            let prepared = prepare_session(
                &scratch.0,
                &scratch.0,
                terminal::EXEC_COMMAND,
                &input,
                &[],
                &session,
                false,
            )?;
            let EffectTarget::TerminalExec(mut spec) = prepared.target else {
                return Err("wrong terminal target".into());
            };
            spec.environment.without_sandbox();
            let operation =
                grapt_types::OperationId::new(format!("op_agent_env_{tty}_{overridden}"))
                    .ok_or("invalid operation")?;
            let (bytes, delivery) = registry
                .exec(&session, operation, spec)
                .await
                .map_err(|e| e.detail)?;
            delivery.captured();
            let report: serde_json::Value =
                serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
            assert_eq!(report["done"], true);
            assert_eq!(report["exit_code"], 0);
            let expected = if overridden {
                "chosen:chosen:chosen:chosen:chosen"
            } else {
                "cat:cat:1:1:dumb"
            };
            let output = report["output"].as_str().ok_or("missing output")?;
            assert!(
                output.contains("from-policy")
                    && output.contains("cargo ")
                    && output.contains("git version ")
            );
            assert!(!output.contains("secret-sentinel"));
            assert!(
                output.contains(expected),
                "agent environment did not reach child: {report}"
            );
        }
    }
    Ok(())
}

#[test]
fn runtime_session_replaces_launcher_identity_after_every_policy_filter() -> Result<(), String> {
    for inherit in ["all", "core", "none"] {
        let policy = policy(&[&format!(
            r#"{{"shell_environment_policy":{{"inherit":"{inherit}","exclude":["*"],"include_only":["CODEX_THREAD_ID","CLAUDE_CODE_SESSION_ID"],"set":{{"SECODE_SESSION_ID":"launcher-session","STATEENGINE_SEAT_SURFACE":"codex","STATEENGINE_AGENT_ID":"launcher-agent","CODEX_THREAD_ID":"launcher-codex","CLAUDE_CODE_SESSION_ID":"launcher-claude"}}}}}}"#
        )])?;
        for id in ["ses_shell_alice", "ses_shell_bob"] {
            let session: SessionId = id.parse::<SessionId>().map_err(|error| error.to_string())?;
            let env = Environment::from_parent(
                parent(&[("SECODE_SESSION_ID", "launcher-session")]),
                &policy,
                cfg!(windows),
            )?
            .with_session(&session)?;
            let expected = parent(&[
                ("GCM_INTERACTIVE", "never"),
                ("GIT_ASKPASS", ""),
                ("GIT_EDITOR", "true"),
                ("GIT_TERMINAL_PROMPT", "0"),
                ("SECODE_SESSION_ID", id),
                ("SSH_ASKPASS", ""),
                ("SSH_ASKPASS_REQUIRE", "never"),
                ("STATEENGINE_SEAT_SURFACE", "secode"), // grapt-rename: keep
            ]);
            assert_eq!(env.0, expected.into_iter().collect());
            assert!(!format!("{env:?}").contains(id));
        }
    }
    Ok(())
}
