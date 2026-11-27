use grapt_cli::{CliCommand, CommandError, ConnectionChoice, SessionSelector};

fn arguments(extra: &[&str]) -> Vec<String> {
    [
        "run",
        "--ephemeral",
        "--embedded",
        "--service",
        "existing-provider-profile",
        "--product",
        "grapt-product",
        "--prompt",
        "review this code",
    ]
    .into_iter()
    .chain(extra.iter().copied())
    .map(str::to_owned)
    .collect()
}

#[test]
fn ephemeral_public_argv_selects_a_new_embedded_foreground_run() -> Result<(), CommandError> {
    for extra in [&[][..], &["--new"][..], &["--jsonl"][..]] {
        let argv = arguments(extra);
        let parsed = grapt_cli::command::ephemeral::parse_early(&argv)
            .ok_or(CommandError::EphemeralTarget)??;
        assert!(matches!(
            parsed,
            CliCommand::Run {
                ephemeral: true,
                connection: ConnectionChoice::Embedded { .. },
                selector: SessionSelector::New,
                background: false,
                resident: false,
                worktree: None,
                ..
            }
        ));
    }
    Ok(())
}

#[test]
fn ephemeral_public_argv_refuses_persistent_and_existing_session_shapes() {
    for extra in [
        &["--session", "ses_a"][..],
        &["--resume"],
        &["--continue"],
        &["--fork"],
        &["--project", "project_a"],
        &["--background"],
        &["--resident", "--jsonl"],
        &["--worktree", "new-worktree"],
        &[
            "--remote",
            "https://example.test",
            "--credential",
            "credential",
        ],
    ] {
        assert!(CliCommand::parse(arguments(extra)).is_err(), "{extra:?}");
    }
    for argv in [
        vec![
            "run",
            "--ephemeral",
            "--service",
            "profile",
            "--prompt",
            "hi",
        ],
        vec!["run", "--ephemeral", "--embedded", "--prompt", "hi"],
        vec!["code", "--ephemeral"],
        vec!["--ephemeral"],
        vec![
            "attach",
            "--ephemeral",
            "--embedded",
            "--service",
            "profile",
        ],
    ] {
        let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(matches!(
            grapt_cli::command::ephemeral::parse_early(&argv),
            Some(Err(_))
        ));
    }
}

#[test]
fn literal_ephemeral_prompt_is_not_an_early_mode_switch() {
    for argv in [
        vec!["run", "--service", "profile", "--prompt", "--ephemeral"],
        vec!["run", "--service", "profile", "--", "--ephemeral"],
        vec!["run", "--service", "profile", "--image", "--ephemeral"],
        vec!["run", "--service", "profile", "-i", "--ephemeral"],
    ] {
        let argv = argv.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(grapt_cli::command::ephemeral::parse_early(&argv).is_none());
        assert!(matches!(
            CliCommand::parse(argv),
            Ok(CliCommand::Run {
                ephemeral: false,
                ..
            })
        ));
    }
}
