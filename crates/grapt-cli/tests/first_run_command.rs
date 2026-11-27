use std::ffi::OsString;
use std::path::PathBuf;

use grapt_cli::command::terminal::{user_profile, TerminalCommand};
use grapt_cli::CliCommand;

/// A scratch repository directory, removed on drop.
struct Repository(PathBuf);

impl Repository {
    fn new(case: &str) -> Result<Self, String> {
        let path =
            std::env::temp_dir().join(format!("grapt-first-run-{case}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir(&path).map_err(|error| error.to_string())?;
        Ok(Self(path))
    }
}

impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[cfg(target_os = "linux")]
#[test]
fn frc_fresh_terminal_initializes_an_empty_profile_before_starting_service() -> Result<(), String> {
    let path = std::env::var_os("PATH").ok_or("PTY fixture requires PATH to locate python3")?;
    let output = std::process::Command::new("python3")
        .arg("-I")
        .arg(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/first_run_pty.py"
        ))
        .arg(env!("CARGO_BIN_EXE_grapt"))
        .env_clear()
        .env("PATH", path)
        .output()
        .map_err(|error| format!("first-run PTY fixture cannot start: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "first-run PTY fixture failed: {}\n{}\n{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let text = String::from_utf8(output.stdout).map_err(|error| error.to_string())?;
    assert!(
        text.lines().any(|line| line == "PASS empty_profile"),
        "{text}"
    );
    Ok(())
}

#[test]
fn frc_terminal_defaults_to_the_user_profile_outside_the_repository() -> Result<(), String> {
    let repository = Repository::new("user")?;
    let user = PathBuf::from("/home/example/.config/grapt/default");
    let parsed = TerminalCommand::parse(&[], repository.0.clone(), Some(user.clone()))?;
    assert_eq!(parsed.repository, repository.0);
    assert_eq!(parsed.profile, user);
    assert!(!repository.0.join(".grapt").exists());
    Ok(())
}

#[test]
fn frc_terminal_keeps_a_repository_profile_that_already_exists() -> Result<(), String> {
    let repository = Repository::new("local")?;
    std::fs::create_dir(repository.0.join(".grapt")).map_err(|error| error.to_string())?;
    let user = PathBuf::from("/home/example/.config/grapt/default");
    let parsed = TerminalCommand::parse(&[], repository.0.clone(), Some(user))?;
    assert_eq!(parsed.profile, repository.0.join(".grapt"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn frc_terminal_reports_an_unreadable_repository_profile_instead_of_switching() -> Result<(), String>
{
    use std::os::unix::fs::PermissionsExt;
    let repository = Repository::new("unreadable")?;
    std::fs::create_dir(repository.0.join(".grapt")).map_err(|error| error.to_string())?;
    let set = |mode: u32| {
        std::fs::set_permissions(&repository.0, std::fs::Permissions::from_mode(mode))
            .map_err(|error| error.to_string())
    };
    // Without search permission the .grapt lookup fails with PermissionDenied.
    set(0o000)?;
    let unreadable = std::fs::symlink_metadata(repository.0.join(".grapt")).is_err();
    let user = PathBuf::from("/home/example/.config/grapt/default");
    let parsed = TerminalCommand::parse(&[], repository.0.clone(), Some(user));
    set(0o700)?;
    if !unreadable {
        // A privileged runner reads through the mode, so the guard was not
        // exercised. That is missing evidence, not a pass.
        return Err(
            "fixture unestablished: this runner reads through mode 000 (run unprivileged)".into(),
        );
    }
    let Err(error) = parsed else {
        return Err(format!(
            "an unreadable .grapt must not fall back: {parsed:?}"
        ));
    };
    assert!(error.contains("unreadable"), "{error}");
    Ok(())
}

#[test]
fn frc_terminal_without_a_user_directory_asks_for_a_profile() -> Result<(), String> {
    let repository = Repository::new("homeless")?;
    let Err(error) = TerminalCommand::parse(&[], repository.0.clone(), None) else {
        return Err("a default profile needs a user directory".into());
    };
    assert!(error.contains("--profile"), "{error}");
    Ok(())
}

#[test]
fn frc_user_profile_follows_the_platform_configuration_directory() {
    let environment = |pairs: &'static [(&'static str, &'static str)]| {
        move |name: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == name)
                .map(|(_, value)| OsString::from(value))
        }
    };
    if cfg!(windows) {
        assert_eq!(
            user_profile(environment(&[(
                "APPDATA",
                r"C:\Users\example\AppData\Roaming"
            )])),
            Some(PathBuf::from(
                r"C:\Users\example\AppData\Roaming\grapt\default"
            ))
        );
        assert_eq!(
            user_profile(environment(&[("HOME", "/home/example")])),
            None
        );
    } else {
        assert_eq!(
            user_profile(environment(&[("HOME", "/home/example")])),
            Some(PathBuf::from("/home/example/.config/grapt/default"))
        );
        assert_eq!(
            user_profile(environment(&[
                ("HOME", "/home/example"),
                ("XDG_CONFIG_HOME", "/cfg")
            ])),
            Some(PathBuf::from("/cfg/grapt/default"))
        );
        // A relative XDG_CONFIG_HOME is ignored, as the specification says.
        assert_eq!(
            user_profile(environment(&[
                ("HOME", "/home/example"),
                ("XDG_CONFIG_HOME", "cfg")
            ])),
            Some(PathBuf::from("/home/example/.config/grapt/default"))
        );
        assert_eq!(user_profile(environment(&[("HOME", "relative")])), None);
    }
    assert_eq!(user_profile(environment(&[])), None);
}

#[test]
fn frc_terminal_explicit_profile_keeps_repository_independent() -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let parsed = TerminalCommand::parse(
        &["repo".into(), "--profile".into(), "profile".into()],
        current.clone(),
        None,
    )?;
    assert_eq!(parsed.repository, current.join("repo"));
    assert_eq!(parsed.profile, current.join("profile"));
    Ok(())
}

#[test]
fn frc_terminal_refuses_ambiguous_or_unbounded_arguments() {
    for args in [
        vec!["--profile".into()],
        vec!["one".into(), "two".into()],
        vec!["--unknown".into()],
        vec!["--setup".into(), "--profile".into(), "new".into()],
        vec!["--profile".into(), "--profile".into(), "new".into()],
        vec!["repo\nforged-status".into()],
        vec!["a".repeat(4_097)],
    ] {
        assert!(TerminalCommand::parse(&args, PathBuf::from("/work"), None).is_err());
    }
}

#[test]
fn frc_terminal_option_errors_explain_the_problem_without_echoing_values() -> Result<(), String> {
    for (arguments, reason) in [
        (vec!["--profile"], "missing value for --profile"),
        (vec!["--setup"], "missing value for --setup"),
        (
            vec!["--reasoning-effort"],
            "missing value for --reasoning-effort",
        ),
        (
            vec!["--profile", "--inspect"],
            "missing value for --profile",
        ),
        (vec!["--setup", "--inspect"], "missing value for --setup"),
        (
            vec!["--reasoning-effort", "--inspect"],
            "missing value for --reasoning-effort",
        ),
        (
            vec!["--profile", "PRIVATE_PROFILE", "--profile", "PRIVATE_OTHER"],
            "duplicate --profile option",
        ),
        (
            vec!["--setup", "PRIVATE_ACCOUNT", "--setup", "PRIVATE_OTHER"],
            "duplicate --setup option",
        ),
        (
            vec![
                "--reasoning-effort",
                "high",
                "--reasoning-effort",
                "PRIVATE_OTHER",
            ],
            "duplicate --reasoning-effort option",
        ),
        (
            vec!["--PRIVATE_UNKNOWN=PRIVATE_VALUE"],
            "unknown terminal option",
        ),
        (
            vec!["PRIVATE_REPO", "PRIVATE_OTHER"],
            "too many repository paths (expected at most one)",
        ),
        (
            vec!["--trust-profile", "--trust-profile"],
            "--trust-profile and --revoke-profile-trust cannot be repeated or combined",
        ),
        (
            vec!["--trust-profile", "--revoke-profile-trust"],
            "--trust-profile and --revoke-profile-trust cannot be repeated or combined",
        ),
        (
            vec!["--revoke-profile-trust", "--revoke-profile-trust"],
            "--trust-profile and --revoke-profile-trust cannot be repeated or combined",
        ),
        (
            vec!["--trust-profile"],
            "--trust-profile and --revoke-profile-trust require an explicit --profile",
        ),
    ] {
        let args = arguments.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let Err(error) = TerminalCommand::parse(&args, PathBuf::from("/work"), None) else {
            return Err("malformed terminal options must fail before profile discovery".into());
        };
        assert_eq!(
            error,
            format!("{reason}\nRun `grapt code --help` for usage.")
        );
        assert!(!error.contains("PRIVATE"), "{error}");
    }
    Ok(())
}

#[test]
fn frc_terminal_bounds_keep_diagnostics_short_and_values_private() -> Result<(), String> {
    let excess = TerminalCommand::parse(
        &vec!["--inspect".to_owned(); 100],
        PathBuf::from("/work"),
        None,
    )
    .err()
    .ok_or("unbounded terminal arguments must fail")?;
    assert!(excess.starts_with("too many terminal arguments"));
    for (args, reason) in [
        (
            vec!["--inspect".to_owned(); 9],
            "too many terminal arguments for the selected options",
        ),
        (
            vec![String::new()],
            "terminal arguments must be nonempty, at most 4096 bytes and contain no control characters",
        ),
        (
            vec!["PRIVATE".repeat(586)],
            "terminal arguments must be nonempty, at most 4096 bytes and contain no control characters",
        ),
        (
            vec!["PRIVATE\nforged-status".to_owned()],
            "terminal arguments must be nonempty, at most 4096 bytes and contain no control characters",
        ),
    ] {
        let Err(error) = TerminalCommand::parse(&args, PathBuf::from("/work"), None) else {
            return Err("unbounded terminal arguments must fail".into());
        };
        assert_eq!(error, format!("{reason}\nRun `grapt code --help` for usage."));
        assert!(!error.contains("PRIVATE"), "{error}");
    }
    Ok(())
}

#[test]
fn frc_terminal_keeps_provider_defined_effort_levels() -> Result<(), String> {
    let args = [
        "--profile",
        "profile",
        "--reasoning-effort",
        "provider-custom",
    ]
    .map(str::to_owned);
    let parsed = TerminalCommand::parse(&args, PathBuf::from("/work"), None)?;
    assert_eq!(parsed.reasoning_effort.as_deref(), Some("provider-custom"));
    Ok(())
}

#[test]
fn frc_legacy_no_command_and_interactive_refusals_remain_typed() {
    assert!(CliCommand::parse(Vec::<String>::new()).is_err());
    assert!(CliCommand::parse(["--model".to_owned(), "some-model".to_owned()]).is_err());
}

#[test]
fn frc_terminal_end_of_options_keeps_later_flags_as_one_repository() -> Result<(), String> {
    use grapt_cli::command::terminal::{ProfileSource, ProfileTrustDecision};
    let root = Repository::new("end-options")?;
    for path in [
        "repo",
        "--dangerously-bypass-permissions",
        "--trust-profile",
        "--revoke-profile-trust",
        "--profile",
        "--setup",
        "--reasoning-effort",
        "--",
    ] {
        let args = ["--", path].map(str::to_owned);
        let parsed = TerminalCommand::parse(&args, root.0.clone(), Some(root.0.join("user")))?;
        assert_eq!(parsed.repository, root.0.join(path));
        assert_eq!(parsed.profile, root.0.join("user"));
        assert_eq!(parsed.profile_source, ProfileSource::UserDefault);
        assert_eq!(parsed.profile_trust, ProfileTrustDecision::Check);
        assert_eq!(parsed.preset, None);
        assert_eq!(parsed.setup, None);
        assert_eq!(parsed.reasoning_effort, None);
    }
    let parsed = TerminalCommand::parse(&["--".into()], root.0.clone(), Some(root.0.join("user")))?;
    assert_eq!(parsed.repository, root.0);
    Ok(())
}

#[test]
fn frc_terminal_end_of_options_preserves_options_and_argument_bounds() -> Result<(), String> {
    use grapt_cli::command::terminal::ProfileTrustDecision;
    let root = Repository::new("end-options-bounds")?;
    let mut args = [
        "--profile",
        "profile",
        "--trust-profile",
        "--setup",
        "account",
        "--reasoning-effort",
        "provider-custom",
        "--author",
        "--inspect",
        "--",
        "--maintainer",
    ]
    .map(str::to_owned)
    .to_vec();
    let parsed = TerminalCommand::parse(&args, root.0.clone(), None)?;
    assert_eq!(parsed.repository, root.0.join("--maintainer"));
    assert_eq!(parsed.profile, root.0.join("profile"));
    assert_eq!(parsed.profile_trust, ProfileTrustDecision::Remember);
    assert_eq!(parsed.setup, Some(root.0.join("account")));
    assert_eq!(parsed.reasoning_effort.as_deref(), Some("provider-custom"));
    assert_eq!(parsed.preset, Some("inspect"));
    args.splice(0..0, std::iter::repeat_n("--author".into(), 100));
    assert!(TerminalCommand::parse(&args, root.0.clone(), None).is_err());
    for path in [String::new(), "x".repeat(4097), "repo\nforged".into()] {
        let args = vec!["--".into(), path];
        assert!(TerminalCommand::parse(&args, root.0.clone(), Some(root.0.join("user"))).is_err());
    }
    Ok(())
}

#[test]
fn frc_terminal_end_of_options_rejects_extra_paths_and_missing_option_values() -> Result<(), String>
{
    let root = Repository::new("end-options-errors")?;
    for (args, reason) in [
        (
            vec!["--", "PRIVATE_REPO", "PRIVATE_OTHER"],
            "too many repository paths",
        ),
        (
            vec!["PRIVATE_REPO", "--", "--inspect"],
            "too many repository paths",
        ),
        (vec!["--", "--", "--"], "too many repository paths"),
        (
            vec!["--profile", "--", "PRIVATE_REPO"],
            "missing value for --profile",
        ),
        (
            vec!["--setup", "--", "PRIVATE_REPO"],
            "missing value for --setup",
        ),
        (
            vec!["--reasoning-effort", "--", "PRIVATE_REPO"],
            "missing value for --reasoning-effort",
        ),
    ] {
        let args = args.into_iter().map(str::to_owned).collect::<Vec<_>>();
        let Err(error) = TerminalCommand::parse(&args, root.0.clone(), Some(root.0.join("user")))
        else {
            return Err("invalid end-of-options arguments were accepted".into());
        };
        assert!(error.contains(reason), "{error}");
        assert!(error.contains("grapt code --help"), "{error}");
        assert!(!error.contains("PRIVATE"), "{error}");
    }
    Ok(())
}

#[test]
fn frc_setup_names_the_account_source_without_changing_repository() -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    let parsed = TerminalCommand::parse(
        &[
            "--setup".into(),
            "account".into(),
            "--profile".into(),
            "new".into(),
            "repo".into(),
        ],
        current.clone(),
        None,
    )?;
    assert_eq!(parsed.setup, Some(current.join("account")));
    assert_eq!(parsed.profile, current.join("new"));
    assert_eq!(parsed.repository, current.join("repo"));
    Ok(())
}

#[test]
fn launch_preset_flags_choose_the_strictest_in_any_order() -> Result<(), String> {
    let current = std::env::current_dir().map_err(|error| error.to_string())?;
    for (flags, expected) in [
        (vec![], None),
        (vec!["--inspect"], Some("inspect")),
        (vec!["--author"], Some("author")),
        (vec!["--maintainer"], Some("maintainer")),
        (
            vec!["--maintainer", "--inspect", "--author"],
            Some("inspect"),
        ),
        (
            vec!["--author", "--maintainer", "--inspect"],
            Some("inspect"),
        ),
        (vec!["--author", "--maintainer", "--author"], Some("author")),
    ] {
        let mut args: Vec<String> = flags.into_iter().map(str::to_owned).collect();
        args.extend(["--profile", "profile", "--setup", "account", "repo"].map(str::to_owned));
        let parsed = TerminalCommand::parse(&args, current.clone(), None)?;
        assert_eq!(parsed.preset, expected);
        assert_eq!(parsed.profile, current.join("profile"));
        assert_eq!(parsed.repository, current.join("repo"));
        assert_eq!(parsed.setup, Some(current.join("account")));
    }
    for flags in [
        vec!["--preset", "inspect"],
        vec!["--inspect=1"],
        vec!["--inspect"; 9],
    ] {
        let args = flags.into_iter().map(str::to_owned).collect::<Vec<_>>();
        assert!(TerminalCommand::parse(&args, current.clone(), Some(current.clone())).is_err());
    }
    Ok(())
}
