//! Command-line front of the tmux TUI driver. Exit status: 0 done, 1 the
//! awaited text did not appear, 2 usage or tmux error.

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;

use grapt_tui_driver::{session_name, DriverError, Launch, Tmux};

const USAGE: &str = "\
usage:
  grapt-tui-driver start --seat <seat> --journey <journey> [--width N] [--height N]
                          [--cwd DIR] [--env KEY=VALUE]... [--socket NAME]
                          -- <command> [args...]
  grapt-tui-driver send --session <name> [--socket NAME] [--text <literal>] [<key>...]
  grapt-tui-driver capture --session <name> [--socket NAME] [--history]
  grapt-tui-driver wait --session <name> [--socket NAME] --text <expected> [--timeout-ms N]
  grapt-tui-driver stop --session <name> [--socket NAME]

Keys use tmux send-keys names (Enter, Escape, Tab, Up, C-c, C-q, ...).
Sessions are named grapt-tui-<seat>-<journey>; watch one with tmux attach -t <name>.
A --socket NAME routes the call through that tmux socket (-L NAME), so it
never shares a server with the machine's default socket.";

const DEFAULT_TIMEOUT_MS: u64 = 10_000;

fn main() -> ExitCode {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match run(&arguments) {
        Ok(output) => {
            if !output.is_empty() {
                print!("{output}");
            }
            ExitCode::SUCCESS
        }
        Err(Failure::Usage(reason)) => {
            eprintln!("grapt-tui-driver: {reason}\n\n{USAGE}");
            ExitCode::from(2)
        }
        Err(Failure::Driver(error)) => {
            eprintln!("grapt-tui-driver: {error}");
            match error {
                DriverError::Timeout { .. } | DriverError::Exited { .. } => ExitCode::from(1),
                _ => ExitCode::from(2),
            }
        }
    }
}

enum Failure {
    Usage(String),
    Driver(DriverError),
}

impl From<DriverError> for Failure {
    fn from(error: DriverError) -> Self {
        Self::Driver(error)
    }
}

fn run(arguments: &[String]) -> Result<String, Failure> {
    let Some((command, rest)) = arguments.split_first() else {
        return Err(Failure::Usage("no command".to_owned()));
    };
    match command.as_str() {
        "start" => start(rest),
        "send" => {
            let (flags, keys) = parse_flags(rest, &["--session", "--socket", "--text"])?;
            let tmux = tmux_for(optional(&flags, "--socket"))?;
            let session = tmux.existing(required(&flags, "--session")?)?;
            if let Some(text) = optional(&flags, "--text") {
                session.send_text(text)?;
            }
            if !keys.is_empty() {
                let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
                session.send_keys(&keys)?;
            }
            Ok(String::new())
        }
        "capture" => {
            let history = rest.iter().any(|argument| argument == "--history");
            let rest: Vec<String> = rest
                .iter()
                .filter(|argument| *argument != "--history")
                .cloned()
                .collect();
            let (flags, _) = parse_flags(&rest, &["--session", "--socket"])?;
            let tmux = tmux_for(optional(&flags, "--socket"))?;
            let session = tmux.existing(required(&flags, "--session")?)?;
            Ok(if history {
                session.capture_with_history()?
            } else {
                session.capture()?
            })
        }
        "wait" => {
            let (flags, _) =
                parse_flags(rest, &["--session", "--socket", "--text", "--timeout-ms"])?;
            let tmux = tmux_for(optional(&flags, "--socket"))?;
            let session = tmux.existing(required(&flags, "--session")?)?;
            let timeout = match optional(&flags, "--timeout-ms") {
                Some(value) => number(value, "--timeout-ms")?,
                None => DEFAULT_TIMEOUT_MS,
            };
            Ok(session.wait_for(required(&flags, "--text")?, Duration::from_millis(timeout))?)
        }
        "stop" => {
            let (flags, _) = parse_flags(rest, &["--session", "--socket"])?;
            let tmux = tmux_for(optional(&flags, "--socket"))?;
            tmux.existing(required(&flags, "--session")?)?.stop()?;
            Ok(String::new())
        }
        "-h" | "--help" | "help" => Ok(format!("{USAGE}\n")),
        other => Err(Failure::Usage(format!("unknown command {other:?}"))),
    }
}

fn start(arguments: &[String]) -> Result<String, Failure> {
    let split = arguments.iter().position(|argument| argument == "--");
    let (options, command) = match split {
        Some(index) => (&arguments[..index], &arguments[index + 1..]),
        None => return Err(Failure::Usage("start needs -- <command>".to_owned())),
    };
    let mut seat = None;
    let mut journey = None;
    let mut socket: Option<&str> = None;
    let mut launch = Launch::new(command.to_vec());
    let mut options = options.iter();
    while let Some(flag) = options.next() {
        let value = options
            .next()
            .ok_or_else(|| Failure::Usage(format!("{flag} needs a value")))?;
        match flag.as_str() {
            "--seat" => seat = Some(value.as_str()),
            "--journey" => journey = Some(value.as_str()),
            "--width" => launch.width = number(value, flag)?,
            "--height" => launch.height = number(value, flag)?,
            "--cwd" => launch.directory = Some(PathBuf::from(value)),
            "--socket" => socket = Some(value.as_str()),
            "--env" => {
                let (key, value) = value
                    .split_once('=')
                    .ok_or_else(|| Failure::Usage(format!("--env {value:?} is not KEY=VALUE")))?;
                launch.environment.push((key.to_owned(), value.to_owned()));
            }
            other => return Err(Failure::Usage(format!("unknown start option {other:?}"))),
        }
    }
    if launch.command.is_empty() {
        return Err(Failure::Usage("start needs a command after --".to_owned()));
    }
    let name = session_name(
        seat.ok_or_else(|| Failure::Usage("start needs --seat".to_owned()))?,
        journey.ok_or_else(|| Failure::Usage("start needs --journey".to_owned()))?,
    )?;
    let session = tmux_for(socket)?.start(&name, &launch)?;
    Ok(format!("{}\n", session.name()))
}

/// The tmux to reach, through `--socket` when given.
fn tmux_for(socket: Option<&str>) -> Result<Tmux, Failure> {
    match socket {
        Some(value) => Ok(Tmux::default().with_socket(value)?),
        None => Ok(Tmux::default()),
    }
}

type Flags<'a> = Vec<(&'a str, &'a str)>;

/// Split `--flag value` pairs from positional arguments.
fn parse_flags<'a>(
    arguments: &'a [String],
    known: &[&str],
) -> Result<(Flags<'a>, Vec<String>), Failure> {
    let mut flags = Vec::new();
    let mut positional = Vec::new();
    let mut arguments = arguments.iter();
    while let Some(argument) = arguments.next() {
        if known.contains(&argument.as_str()) {
            let value = arguments
                .next()
                .ok_or_else(|| Failure::Usage(format!("{argument} needs a value")))?;
            flags.push((argument.as_str(), value.as_str()));
        } else if argument.starts_with("--") {
            return Err(Failure::Usage(format!("unknown option {argument:?}")));
        } else {
            positional.push(argument.clone());
        }
    }
    Ok((flags, positional))
}

fn optional<'a>(flags: &Flags<'a>, name: &str) -> Option<&'a str> {
    flags
        .iter()
        .rev()
        .find(|(flag, _)| *flag == name)
        .map(|(_, value)| *value)
}

fn required<'a>(flags: &Flags<'a>, name: &str) -> Result<&'a str, Failure> {
    optional(flags, name).ok_or_else(|| Failure::Usage(format!("{name} is required")))
}

fn number<T: std::str::FromStr>(value: &str, flag: &str) -> Result<T, Failure> {
    value
        .parse()
        .map_err(|_| Failure::Usage(format!("{flag} {value:?} is not a valid number")))
}
