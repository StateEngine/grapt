pub const HELP: &str = "Grapt native terminal
Start coding: grapt
Options: grapt code [--profile PROFILE] [DIRECTORY]
Attach directly: grapt-tui --service PROFILE [--directory DIR | --session ID]
Prepared seat: add --prompt TEXT to an exact --session; --reasoning-effort LEVEL overrides its effort before submission.
Display: --light selects a light theme; --dark selects a dark theme (default).
When both theme flags are present, the last one wins.
Notifications are enabled by default; --no-notifications disables them.

Defaults: Ctrl+A connect · F7 agents · F3 model/effort · Ctrl+P commands · F1 help · F4 usage · F6 questions
Enter sends · Shift+Enter newline · Ctrl+R recall · Esc interrupts · Ctrl+C interrupts/clears; press twice to exit · /quit exits";

use grapt_api::ProductHealth;
use grapt_client::{wire::SessionWireId, BasicCredential, Bounds, HttpConnection};
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone)]
pub struct Config {
    pub root: PathBuf,
    pub directory: Option<String>,
    pub session: Option<SessionWireId>,
    pub expected: Option<ProductHealth>,
    pub reasoning_effort: Option<String>,
    pub light: bool,
    pub notifications: bool,
    pub prompt: Option<String>,
    pub preset: Option<grapt_client::preset::Preset>,
}

#[derive(serde::Deserialize)]
struct Ready {
    schema: String,
    operation: String,
    pid: u32,
    build: String,
    environment: String,
    service_instance: String,
    observed_endpoint: String,
}

pub fn read(path: &Path, bound: u64) -> Result<String, String> {
    let mut data = String::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(bound + 1)
        .read_to_string(&mut data)
        .map_err(|e| e.to_string())?;
    if data.len() as u64 > bound {
        return Err("profile record exceeds its bound".into());
    }
    Ok(data)
}

impl Config {
    pub fn parse_args(arguments: impl IntoIterator<Item = String>) -> Result<Self, String> {
        let mut args = arguments.into_iter();
        let (mut root, mut directory, mut session, mut preset) = (None, None, None, None);
        let mut light = None;
        let mut notifications = true;
        let mut prompt = None;
        let mut expected = None;
        let mut reasoning_effort = None;
        let mut options = 0;
        while let Some(arg) = args.next() {
            options += 1;
            if options > 12 {
                return Err(option_error("too many launch options"));
            }
            match arg.as_str() {
                "--service" if root.is_none() => {
                    root = Some(PathBuf::from(value(&mut args, "--service")?))
                }
                "--directory" if directory.is_none() => {
                    directory = Some(value(&mut args, "--directory")?)
                }
                "--session" if session.is_none() => session = Some(value(&mut args, "--session")?),
                "--expect-health" if expected.is_none() => {
                    let raw = value(&mut args, "--expect-health")?;
                    let health: ProductHealth = serde_json::from_str(&raw)
                        .map_err(|_| "invalid expected product identity")?;
                    let checked = ProductHealth::new(
                        health.pid,
                        health.build.clone(),
                        health.environment.clone(),
                        health.service_instance.clone(),
                    )
                    .map_err(|_| "invalid expected product identity")?;
                    if checked != health {
                        return Err("invalid expected product identity".into());
                    }
                    expected = Some(checked);
                }
                "--reasoning-effort" if reasoning_effort.is_none() => {
                    let effort = value(&mut args, "--reasoning-effort")?;
                    if grapt_client::wire::ModelVariantWireId::parse(effort.clone()).is_none() {
                        return Err(option_error("invalid --reasoning-effort level"));
                    }
                    reasoning_effort = Some(effort);
                }
                "--prompt" if prompt.is_none() => {
                    let text = args
                        .next()
                        .ok_or_else(|| option_error("missing value for --prompt"))?;
                    if !grapt_tui_transcript::prompt::valid_prompt(&text) {
                        return Err("startup prompt must be nonempty text of at most 64 KiB".into());
                    }
                    prompt = Some(text);
                }
                "--light" => light = Some(true),
                "--dark" => light = Some(false),
                "--no-notifications" => notifications = false,
                "--service" | "--directory" | "--session" | "--expect-health"
                | "--reasoning-effort" | "--prompt" => {
                    return Err(option_error(&format!("duplicate {arg} option")));
                }
                flag => match grapt_client::preset::from_flag(flag) {
                    Some(next) => preset = Some(grapt_client::preset::strictest(preset, next)),
                    None => return Err(unknown_option(flag)),
                },
            }
        }
        if prompt.is_some() && session.is_none() {
            return Err("--prompt requires an exact --session".into());
        }
        if session.is_none() && directory.is_none() {
            return Err("--directory is required without --session".into());
        }
        let session = session
            .map(|session: String| {
                if session.len() > 256 {
                    return Err("session identity exceeds its bound");
                }
                serde_json::from_value(serde_json::Value::String(session))
                    .map_err(|_| "invalid session identity")
            })
            .transpose()?;
        let root = root.ok_or("--service is required")?;
        let saved = crate::settings::Preferences::load(&root)?.values;
        Ok(Self {
            root,
            directory,
            session,
            expected,
            reasoning_effort,
            light: light.unwrap_or(saved.light),
            notifications,
            prompt,
            preset,
        })
    }

    pub fn connect(&self, bounds: Bounds) -> Result<(HttpConnection, HttpConnection), String> {
        // Same readiness/identity checks as the existing TUI startup adapter.
        let path = self.root.join("serve-ready.json");
        let receipt = read(&path, 8192)?;
        let ready: Ready =
            serde_json::from_str(&receipt).map_err(|_| "invalid readiness receipt")?;
        if ready.schema != grapt_api::product_health::PRODUCT_READY_SCHEMA
            || ready.operation != "serve"
        {
            return Err("incompatible readiness receipt".into());
        }
        let health = ProductHealth::new(
            ready.pid,
            ready.build,
            ready.environment,
            ready.service_instance,
        )
        .map_err(|_| "invalid product identity")?;
        if self
            .expected
            .as_ref()
            .is_some_and(|expected| expected != &health)
        {
            return Err("ready service differs from the launcher-selected product".into());
        }
        let raw = read(&self.root.join("basic.credential"), 512)?;
        if raw.contains('\r') || raw.lines().count() != 1 {
            return Err("invalid profile credential".into());
        }
        let (user, secret) = raw
            .trim_end_matches('\n')
            .split_once(':')
            .ok_or("invalid profile credential")?;
        let credential =
            BasicCredential::new(user, secret).map_err(|_| "invalid profile credential")?;
        let mut stream = HttpConnection::attach_endpoint(
            &ready.observed_endpoint,
            credential.clone(),
            Bounds {
                max_pending_events: 1024,
                ..bounds
            },
        )
        .map_err(|e| e.to_string())?;
        let mut peer =
            HttpConnection::attach_endpoint(&ready.observed_endpoint, credential, bounds)
                .map_err(|e| e.to_string())?;
        if !peer.address().ip().is_loopback() {
            return Err("profile endpoint must be loopback".into());
        }
        peer.verify_product(&health).map_err(|e| e.to_string())?;
        if receipt != read(&path, 8192)? || raw != read(&self.root.join("basic.credential"), 512)? {
            return Err("readiness changed during attachment".into());
        }
        stream.pin_product(health.clone());
        peer.pin_product(health);
        Ok((peer, stream))
    }
}

fn option_error(message: &str) -> String {
    format!("{message}\nRun `grapt-tui --help` for usage.")
}

fn unknown_option(argument: &str) -> String {
    // Only an option-shaped ASCII name may be displayed. Never echo an
    // attached value, positional text, controls, or an unbounded argument.
    let name = argument.split('=').next().unwrap_or_default();
    let tail = name.trim_start_matches('-');
    if name.starts_with('-')
        && name.len() <= 64
        && !tail.is_empty()
        && tail
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-')
    {
        option_error(&format!("unknown launch option {name}"))
    } else {
        option_error("unknown launch option")
    }
}

fn value(args: &mut impl Iterator<Item = String>, option: &'static str) -> Result<String, String> {
    let value = args
        .next()
        .ok_or_else(|| option_error(&format!("missing value for {option}")))?;
    if value.is_empty() || value.len() > 8192 || value.chars().any(char::is_control) {
        return Err(option_error(&format!("invalid value for {option}")));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(extra: &[&str]) -> Result<Config, String> {
        Config::parse_args(
            ["--service", "/tmp/fixture", "--session", "ses_000000000001"]
                .into_iter()
                .chain(extra.iter().copied())
                .map(str::to_owned),
        )
    }

    #[test]
    fn notifications_default_on_and_can_be_disabled() -> Result<(), String> {
        assert!(parse(&[])?.notifications);
        assert!(!parse(&["--no-notifications"])?.notifications);
        Ok(())
    }

    #[test]
    fn display_help_matches_parser_defaults_and_last_theme_flag() -> Result<(), String> {
        for hint in [
            "--light selects a light theme",
            "--dark selects a dark theme (default)",
            "the last one wins",
            "Notifications are enabled by default",
            "--no-notifications disables them",
        ] {
            assert!(HELP.contains(hint), "missing help: {hint}");
        }
        for (flags, light, notifications) in [
            (vec![], false, true),
            (vec!["--light"], true, true),
            (vec!["--dark"], false, true),
            (vec!["--dark", "--light"], true, true),
            (vec!["--light", "--dark"], false, true),
            (vec!["--light", "--no-notifications"], true, false),
            (vec!["--no-notifications", "--dark"], false, false),
        ] {
            let config = parse(&flags)?;
            assert_eq!((config.light, config.notifications), (light, notifications));
        }
        Ok(())
    }

    #[test]
    fn malformed_options_name_the_option_but_never_its_value() -> Result<(), String> {
        let refusal =
            |args: Vec<&str>| match Config::parse_args(args.into_iter().map(str::to_owned)) {
                Err(error) => Ok(error),
                Ok(_) => Err("malformed launch arguments were accepted".to_owned()),
            };
        for option in [
            "--service",
            "--directory",
            "--session",
            "--expect-health",
            "--reasoning-effort",
            "--prompt",
        ] {
            assert_eq!(
                refusal(vec![option])?,
                option_error(&format!("missing value for {option}"))
            );
            let first = match option {
                "--expect-health" => {
                    r#"{"schema":"secode.api.health.v1","pid":1,"build":"test","environment":"test","service_instance":"test"}"#
                }
                "--reasoning-effort" => "high",
                _ => "DO_NOT_PRINT",
            };
            let error = refusal(vec![option, first, option, "SECOND_SECRET"])?;
            assert_eq!(error, option_error(&format!("duplicate {option} option")));
            assert!(!error.contains(first) && !error.contains("SECOND_SECRET"));
        }
        for option in ["--service", "--directory", "--session"] {
            for invalid in ["".to_owned(), "SECRET\u{1b}".to_owned(), "x".repeat(8193)] {
                assert_eq!(
                    refusal(vec![option, &invalid])?,
                    option_error(&format!("invalid value for {option}"))
                );
            }
        }
        assert_eq!(
            refusal(vec!["--unknown=SECRET"])?,
            option_error("unknown launch option --unknown")
        );
        for unknown in [
            "POSITIONAL_SECRET".to_owned(),
            "--bad\u{1b}SECRET".to_owned(),
            format!("--{}", "x".repeat(100)),
        ] {
            assert_eq!(
                refusal(vec![&unknown])?,
                option_error("unknown launch option")
            );
        }
        assert_eq!(
            refusal(vec!["--dark"; 13])?,
            option_error("too many launch options")
        );
        Ok(())
    }

    #[test]
    fn repeated_display_flags_keep_the_existing_precedence() -> Result<(), String> {
        let config = parse(&[
            "--light",
            "--light",
            "--dark",
            "--no-notifications",
            "--no-notifications",
        ])?;
        assert!(!config.light && !config.notifications);
        Ok(())
    }

    #[test]
    fn prompt_is_literal_multiline_and_bounded() -> Result<(), String> {
        let text = "/literal\r\n第二行\tvalue\n";
        assert_eq!(parse(&["--prompt", text])?.prompt.as_deref(), Some(text));
        assert!(parse(&["--prompt", &"x".repeat(crate::MAX_DRAFT)]).is_ok());
        for invalid in [
            "",
            " ",
            "bad\0text",
            "bad\x1btext",
            &"x".repeat(crate::MAX_DRAFT + 1),
        ] {
            assert!(parse(&["--prompt", invalid]).is_err());
        }
        assert!(parse(&["--prompt"]).is_err());
        assert!(parse(&["--prompt", "one", "--prompt", "two"]).is_err());
        assert_eq!(
            parse(&["--reasoning-effort", "high"])?
                .reasoning_effort
                .as_deref(),
            Some("high")
        );
        assert!(Config::parse_args(
            [
                "--service",
                "/tmp/fixture",
                "--directory",
                "/repo",
                "--reasoning-effort",
                "high"
            ]
            .into_iter()
            .map(str::to_owned)
        )
        .is_ok());
        assert!(parse(&["--directory", "--prompt", "text"]).is_err());
        Ok(())
    }
}
