use grapt_cli::command::terminal::TerminalCommand;
use grapt_cli::CliCommand;

#[test]
fn seat02_cli_bypass_requires_the_explicit_flag_and_does_not_change_defaults() -> Result<(), String>
{
    for flags in [
        vec![],
        vec!["--dangerously-bypass-permissions"],
        vec!["--dangerously-bypass-permissions", "--inspect"],
    ] {
        let mut args = vec!["run", "--service", "profile", "--new", "--prompt", "hello"];
        args.extend(flags.iter().copied());
        let command = CliCommand::parse(args).map_err(|error| error.to_string())?;
        let CliCommand::Run { preset, .. } = command else {
            return Err("run was not parsed".into());
        };
        assert_eq!(
            preset,
            if flags.contains(&"--inspect") {
                Some("inspect")
            } else if flags.is_empty() {
                None
            } else {
                Some("bypass")
            }
        );
    }
    for flag in ["--bypass", "--dangerously-bypass-permissions=true"] {
        assert!(CliCommand::parse([
            "run",
            "--service",
            "profile",
            "--new",
            "--prompt",
            "hello",
            flag
        ])
        .is_err());
    }
    Ok(())
}

#[test]
fn seat02_native_launch_uses_the_same_explicit_bypass_selection() -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let command = TerminalCommand::parse(
        &["--profile", "profile", "--dangerously-bypass-permissions"].map(str::to_owned),
        current.clone(),
        None,
    )?;
    assert_eq!(command.preset, Some("bypass"));
    let default =
        TerminalCommand::parse(&["--profile", "profile"].map(str::to_owned), current, None)?;
    assert_eq!(default.preset, None);
    assert!(grapt_cli::command::terminal::HELP.contains("disables ALL permission checks"));
    Ok(())
}
