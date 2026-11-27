//! The primary interactive entry is separate from the frozen noninteractive CLI.

use grapt_types::profile_names::{choose, Spelling};
use std::ffi::OsString;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileTrustDecision {
    #[default]
    Check,
    Remember,
    Revoke,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProfileSource {
    Explicit,
    UserDefault,
    RepositoryDefault,
}

pub const HELP: &str = "usage: grapt\n       grapt code [--profile <profile>] [--trust-profile | --revoke-profile-trust] [--setup <account-directory>] [--model <provider/model>] [--agent <id>] [--reasoning-effort <level>] [--inspect | --author | --maintainer | --dangerously-bypass-permissions] [--] [repository]\n\nOpen the native terminal, reusing a running service or starting one for this window.\nThe repository defaults to the current directory. A repository .grapt profile requires a content-bound trust decision or\nexplicit --profile selection; otherwise use grapt/default in user configuration.\n--trust-profile remembers the selected configuration; --revoke-profile-trust\nrevokes it and exits. Profile trust grants no MCP or plugin process consent.\nOn first start, choose ChatGPT sign-in, an Anthropic API key or an OpenAI API key.
--setup imports explicitly named account inputs into a NEW profile without a provider call.\nUse --model (-m) and --agent to start a new Session with explicit choices.\nOtherwise choose an agent, then a model and effort for this directory; Ctrl-P resumes a session.\nMaintainer is the default and asks before destructive commands or authority edits.\n--dangerously-bypass-permissions disables ALL permission checks, including configured\ndenials and authority-file protection; use it only for explicitly unattended work.\nA preset flag selects trust for each explicitly opened Session; strictest wins.\nIt persists on that Session and changes no profile or repository default.\nEsc interrupts work; /quit exits and drains the owned product.";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalCommand {
    pub profile: PathBuf,
    pub profile_source: ProfileSource,
    pub user_profile: Option<PathBuf>,
    pub profile_trust: ProfileTrustDecision,
    pub repository: PathBuf,
    pub setup: Option<PathBuf>,
    pub reasoning_effort: Option<String>,
    pub selection: crate::StartupSelection,
    pub preset: Option<&'static str>,
}

impl TerminalCommand {
    /// `user_profile` is where a profile goes when none is named and the
    /// repository has none of its own; see [`user_profile`].
    #[expect(clippy::too_many_lines, reason = "legacy")]
    pub fn parse(
        arguments: &[String],
        current: PathBuf,
        user_profile: Option<PathBuf>,
    ) -> Result<Self, String> {
        if crate::command::ephemeral::parse_early(arguments).is_some() {
            return Err(crate::CommandError::EphemeralTarget.to_string());
        }
        if arguments.len() > 15 {
            return Err(argument_error("too many terminal arguments (maximum 15)"));
        }
        if arguments.iter().any(|value| {
            value.is_empty() || value.len() > 4_096 || value.chars().any(char::is_control)
        }) {
            return Err(argument_error(
                "terminal arguments must be nonempty, at most 4096 bytes and contain no control characters",
            ));
        }
        let mut profile = None;
        let mut profile_trust = ProfileTrustDecision::Check;
        let mut repository = None;
        let mut setup = None;
        let mut preset = None;
        let mut reasoning_effort = None;
        let mut selection = crate::StartupSelection::default();
        let mut positional_only = false;
        let mut values = arguments.iter();
        while let Some(value) = values.next() {
            if !positional_only && value == "--" {
                positional_only = true;
                continue;
            }
            if positional_only {
                if repository.replace(PathBuf::from(value)).is_some() {
                    return Err(argument_error(
                        "too many repository paths (expected at most one)",
                    ));
                }
            } else if matches!(value.as_str(), "--trust-profile" | "--revoke-profile-trust") {
                if profile_trust != ProfileTrustDecision::Check {
                    return Err(argument_error(
                        "--trust-profile and --revoke-profile-trust cannot be repeated or combined",
                    ));
                }
                profile_trust = if value == "--trust-profile" {
                    ProfileTrustDecision::Remember
                } else {
                    ProfileTrustDecision::Revoke
                };
            } else if matches!(
                value.as_str(),
                "--inspect" | "--author" | "--maintainer" | "--dangerously-bypass-permissions"
            ) {
                let name = match value.as_str() {
                    "--inspect" => "inspect",
                    "--author" => "author",
                    "--dangerously-bypass-permissions" => "bypass",
                    _ => "maintainer",
                };
                super::choose_preset(&mut preset, name);
            } else if matches!(value.as_str(), "--model" | "-m" | "--agent") {
                let selected = values
                    .next()
                    .filter(|selected| !selected.starts_with('-'))
                    .ok_or_else(|| argument_error("--model and --agent require a value"))?;
                if value == "--agent" {
                    selection.set_agent(selected)
                } else {
                    selection.set_model(selected)
                }
                .map_err(|error| format!("{error}\nRun `grapt code --help` for usage."))?;
            } else if value == "--reasoning-effort" {
                if reasoning_effort.is_some() {
                    return Err(argument_error("duplicate --reasoning-effort option"));
                }
                let effort = values
                    .next()
                    .filter(|effort| !effort.starts_with('-'))
                    .ok_or_else(|| argument_error("missing value for --reasoning-effort"))?;
                if grapt_types::ModelVariantId::new(effort).is_none() {
                    return Err(argument_error("invalid --reasoning-effort level"));
                }
                reasoning_effort = Some(effort.clone());
            } else if value == "--profile" {
                if profile.is_some() {
                    return Err(argument_error("duplicate --profile option"));
                }
                profile = Some(PathBuf::from(
                    values
                        .next()
                        .filter(|path| !path.starts_with('-'))
                        .ok_or_else(|| argument_error("missing value for --profile"))?,
                ));
            } else if value == "--setup" {
                if setup.is_some() {
                    return Err(argument_error("duplicate --setup option"));
                }
                setup = Some(PathBuf::from(
                    values
                        .next()
                        .filter(|path| !path.starts_with('-'))
                        .ok_or_else(|| argument_error("missing value for --setup"))?,
                ));
            } else if value.starts_with('-') {
                return Err(argument_error("unknown terminal option"));
            } else if repository.is_none() {
                repository = Some(PathBuf::from(value));
            } else {
                return Err(argument_error(
                    "too many repository paths (expected at most one)",
                ));
            }
        }
        if arguments.len()
            > 8 + usize::from(reasoning_effort.is_some()) * 2
                + usize::from(profile_trust != ProfileTrustDecision::Check)
                + usize::from(positional_only)
                + usize::from(selection.model.is_some()) * 2
                + usize::from(selection.agent.is_some()) * 2
        {
            return Err(argument_error(
                "too many terminal arguments for the selected options",
            ));
        }
        if profile_trust != ProfileTrustDecision::Check && profile.is_none() {
            return Err(argument_error(
                "--trust-profile and --revoke-profile-trust require an explicit --profile",
            ));
        }
        let absolute = |path: PathBuf| {
            if path.is_absolute() {
                path
            } else {
                current.join(path)
            }
        };
        let repository = absolute(repository.unwrap_or_else(|| current.clone()));
        let (profile, profile_source) = match profile {
            Some(profile) => (absolute(profile), ProfileSource::Explicit),
            None => default_profile(&repository, user_profile.clone())?,
        };
        Ok(Self {
            profile,
            profile_source,
            user_profile,
            profile_trust,
            repository,
            setup: setup.map(absolute),
            preset,
            reasoning_effort,
            selection,
        })
    }
}

// Diagnostics name only known options, never caller-supplied values or unknown flags.
fn argument_error(message: &'static str) -> String {
    format!("{message}\nRun `grapt code --help` for usage.")
}

/// Select a candidate, not authority to use it. Repository defaults must pass
/// the content-bound trust check before any account input or process is opened.
fn default_profile(
    repository: &Path,
    user_profile: Option<PathBuf>,
) -> Result<(PathBuf, ProfileSource), String> {
    let local = prefer_current(
        repository.join(Spelling::Current.project_dir()),
        repository.join(Spelling::Legacy.project_dir()),
    );
    match std::fs::symlink_metadata(&local) {
        Ok(_) => return Ok((local, ProfileSource::RepositoryDefault)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        // An unreadable repository profile is not an absent one: falling
        // back would silently switch the user to a different account.
        Err(error) => {
            return Err(format!(
                "repository profile {} is unreadable: {:?}; repair access or name a profile with --profile",
                local.display(),
                error.kind()
            ));
        }
    }
    user_profile.map(|profile| (profile, ProfileSource::UserDefault)).ok_or_else(|| {
        "no user configuration directory is known (set HOME, XDG_CONFIG_HOME or APPDATA); name one with --profile <profile>".to_owned()
    })
}

/// The current name of a profile location, or its legacy name from before the
/// product rename when only that one exists. A location that cannot be read
/// counts as present, so a fault never switches the user to another account.
fn prefer_current(current: PathBuf, legacy: PathBuf) -> PathBuf {
    let present = |path: &Path| !matches!(std::fs::symlink_metadata(path), Err(error) if error.kind() == std::io::ErrorKind::NotFound);
    let choice = choose(present(&current), present(&legacy));
    match (choice.spelling, choice.ignored) {
        (Spelling::Legacy, _) => {
            notice(format!(
                "using {} (its name from before the rename); rename it to {} when convenient",
                legacy.display(),
                current.display()
            ));
            legacy
        }
        (Spelling::Current, Some(_)) => {
            notice(format!(
                "ignoring {} because {} exists",
                legacy.display(),
                current.display()
            ));
            current
        }
        (Spelling::Current, None) => current,
    }
}

/// One line on stderr per distinct notice in this process.
fn notice(message: String) {
    static SHOWN: std::sync::Mutex<std::collections::BTreeSet<String>> =
        std::sync::Mutex::new(std::collections::BTreeSet::new());
    if let Ok(mut shown) = SHOWN.lock() {
        if shown.insert(message.clone()) {
            eprintln!("grapt: {message}");
        }
    }
}

/// The default profile in the user's configuration directory, from the
/// process environment `variable`: `%APPDATA%\grapt\default` on Windows,
/// otherwise `$XDG_CONFIG_HOME/grapt/default` or `~/.config/grapt/default`.
/// An install from before the rename keeps its pre-rename directory while the
/// `grapt` one is absent. Relative or empty values are ignored, as the XDG
/// specification requires.
pub fn user_profile(variable: impl Fn(&str) -> Option<OsString>) -> Option<PathBuf> {
    let absolute = |name: &str| {
        variable(name)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let configuration = if cfg!(windows) {
        absolute("APPDATA")
    } else {
        absolute("XDG_CONFIG_HOME").or_else(|| absolute("HOME").map(|home| home.join(".config")))
    }?;
    Some(prefer_current(
        configuration
            .join(Spelling::Current.profiles_dir())
            .join("default"),
        configuration
            .join(Spelling::Legacy.profiles_dir())
            .join("default"),
    ))
}

#[cfg(test)]
mod effort_tests {
    use super::*;
    #[test]
    fn reasoning_effort_is_an_explicit_native_launch_option() -> Result<(), String> {
        let args = [
            "--profile",
            "profile",
            "--setup",
            "account",
            "--inspect",
            "--author",
            "--maintainer",
            "--reasoning-effort",
            "high",
            "repository",
        ]
        .map(str::to_owned);
        let current = std::env::current_dir().map_err(|error| error.to_string())?;
        let command = TerminalCommand::parse(&args, current, None)?;
        assert_eq!(command.reasoning_effort.as_deref(), Some("high"));
        Ok(())
    }
}
