//! Drives a terminal program, normally the native TUI, inside a named tmux
//! session: start it at a fixed size, send keys, capture the exact visible
//! screen text and wait for expected text within a bound.
//!
//! Sessions are named `grapt-tui-<seat>-<journey>` on the default tmux
//! server, so a person can `tmux attach -t <name>` and watch a check live.
//! The driver only touches sessions that carry that prefix, and targets them
//! by exact name.
//!
//! tmux treats an argument ending in `;` as a command separator, so nothing
//! caller-supplied reaches tmux's parser: the program's argv, directory and
//! environment go into a generated launcher script whose path is all tmux
//! sees, typed text and single-character keys go as hex bytes, and named keys
//! are restricted to tmux key-name spelling. Every tmux call is bounded in
//! time and output bytes, and a wait never runs a call past its own deadline.

use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

/// Every session this driver creates or touches starts with this prefix.
pub const SESSION_PREFIX: &str = "grapt-tui-";

/// The longest seat or journey part of a session name.
pub const NAME_PART_MAX: usize = 40;

/// The longest any single tmux call may run.
pub const CALL_BOUND: Duration = Duration::from_secs(5);

/// The most bytes a single tmux call may print on stdout or stderr. A full
/// history capture of a large screen fits comfortably.
pub const OUTPUT_CAP: usize = 4 * 1024 * 1024;

const POLL_INTERVAL: Duration = Duration::from_millis(50);
const CHILD_POLL: Duration = Duration::from_millis(5);

/// How long a failed tmux call waits, after killing its process group, for
/// that group's pipes to close and its readers to finish.
const CLEANUP_BOUND: Duration = Duration::from_secs(1);

/// The name of every pipe-reader thread, so tests can count them.
const READER_THREAD: &str = "tui-driver-read";

#[derive(Debug)]
pub enum DriverError {
    /// A seat, journey, session name or key the driver refuses.
    Name(String),
    /// A session with this name is already running.
    Exists(String),
    /// tmux could not be run, refused the command, overran its bound or
    /// printed more than [`OUTPUT_CAP`].
    Tmux {
        action: &'static str,
        detail: String,
    },
    /// The expected text did not appear before the bound.
    Timeout { text: String, screen: String },
    /// The program exited before the expected text appeared.
    Exited {
        text: String,
        status: i32,
        screen: String,
    },
}

impl fmt::Display for DriverError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Name(reason) => write!(f, "invalid name or key: {reason}"),
            Self::Exists(name) => write!(
                f,
                "tmux session {name} already exists; stop it or pick another journey name"
            ),
            Self::Tmux { action, detail } => write!(f, "tmux {action} failed: {detail}"),
            Self::Timeout { text, screen } => {
                write!(f, "timed out waiting for {text:?}; last screen:\n{screen}")
            }
            Self::Exited {
                text,
                status,
                screen,
            } => write!(
                f,
                "program exited with status {status} before {text:?} appeared; last screen:\n{screen}"
            ),
        }
    }
}

impl std::error::Error for DriverError {}

/// `grapt-tui-<seat>-<journey>`, refusing parts tmux or a person would
/// trip over. Seats may be hyphenated agent ids.
pub fn session_name(seat: &str, journey: &str) -> Result<String, DriverError> {
    for (label, part) in [("seat", seat), ("journey", journey)] {
        if !name_part(part) {
            return Err(DriverError::Name(format!(
                "{label} {part:?} must be 1-{NAME_PART_MAX} of a-z, 0-9 or '-', not starting with '-'"
            )));
        }
    }
    Ok(format!("{SESSION_PREFIX}{seat}-{journey}"))
}

fn name_part(part: &str) -> bool {
    !part.is_empty()
        && part.len() <= NAME_PART_MAX
        && !part.starts_with('-')
        && part
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

/// Admit exactly the names [`session_name`] can produce: some split of the
/// remainder at a '-' gives a valid seat and journey.
fn check_name(name: &str) -> Result<(), DriverError> {
    let admitted = name.strip_prefix(SESSION_PREFIX).is_some_and(|rest| {
        rest.match_indices('-')
            .any(|(index, _)| name_part(&rest[..index]) && name_part(&rest[index + 1..]))
    });
    if admitted {
        Ok(())
    } else {
        Err(DriverError::Name(format!(
            "{name:?} is not {SESSION_PREFIX}<seat>-<journey>; the driver only touches its own sessions"
        )))
    }
}

/// What to start and how big the screen is.
#[derive(Clone, Debug)]
pub struct Launch {
    pub width: u16,
    pub height: u16,
    pub directory: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
    pub command: Vec<String>,
}

impl Launch {
    pub fn new(command: Vec<String>) -> Self {
        Self {
            width: 120,
            height: 40,
            directory: None,
            environment: Vec::new(),
            command,
        }
    }
}

/// How to reach tmux: the program, the bound on each call, and, when set,
/// a private socket reached with -L.
#[derive(Clone, Debug)]
pub struct Tmux {
    program: PathBuf,
    call_bound: Duration,
    socket: Option<String>,
}

impl Default for Tmux {
    fn default() -> Self {
        Self {
            program: PathBuf::from("tmux"),
            call_bound: CALL_BOUND,
            socket: None,
        }
    }
}

impl Tmux {
    /// Use another tmux executable, for example a test double.
    pub fn with_program(program: impl Into<PathBuf>) -> Self {
        Self {
            program: program.into(),
            ..Self::default()
        }
    }

    /// Bound each call by `bound` instead of [`CALL_BOUND`].
    pub fn call_bound(mut self, bound: Duration) -> Self {
        self.call_bound = bound;
        self
    }

    /// Reach tmux through its own socket (`-L <socket>`) instead of the
    /// per-user default. A tmux server exits when its last session dies, so
    /// callers that start and kill sessions must not share one: a sibling's
    /// in-flight `new-session` loses the server and fails with "server
    /// exited unexpectedly". The name is one file in tmux's per-user
    /// directory, so it must be non-empty and free of '/'.
    pub fn with_socket(mut self, socket: impl Into<String>) -> Result<Self, DriverError> {
        let socket = socket.into();
        if socket.is_empty() || socket.contains('/') {
            return Err(DriverError::Name(format!(
                "socket {socket:?} must be non-empty and without '/'"
            )));
        }
        self.socket = Some(socket);
        Ok(self)
    }

    /// Whether a session with exactly this name is running.
    pub fn exists(&self, name: &str) -> Result<bool, DriverError> {
        check_name(name)?;
        let target = format!("={name}");
        Ok(self
            .run("has-session", &["has-session", "-t", &target], None)?
            .success)
    }

    /// Start `launch.command` detached in a new session. The window keeps its
    /// size when someone attaches, and the pane stays after the program exits
    /// so its last screen and exit status can still be read.
    pub fn start(&self, name: &str, launch: &Launch) -> Result<Session, DriverError> {
        check_name(name)?;
        if launch.command.is_empty() {
            return Err(DriverError::Tmux {
                action: "start",
                detail: "no command to run".to_owned(),
            });
        }
        if self.exists(name)? {
            return Err(DriverError::Exists(name.to_owned()));
        }
        // tmux parses only the launcher's path; the command, its directory
        // and its environment live inside the script.
        let launcher = write_launcher(name, launch)?;
        let mut args: Vec<String> = vec![
            "new-session".into(),
            "-d".into(),
            "-s".into(),
            name.into(),
            "-x".into(),
            launch.width.to_string(),
            "-y".into(),
            launch.height.to_string(),
            "/bin/sh".into(),
            launcher.to_string_lossy().into_owned(),
        ];
        // The new session's only window, by exact session name.
        let window = format!("={name}:");
        let options: [&[&str]; 2] = [
            &["set-option", "-w", "-t", &window, "remain-on-exit", "on"],
            &["set-option", "-w", "-t", &window, "window-size", "manual"],
        ];
        // Chained into the same tmux call so the options are in place before
        // a fast program can exit.
        for option in options {
            args.push(";".into());
            args.extend(option.iter().map(|part| (*part).to_owned()));
        }
        if let Err(error) = self.checked("start", &args, None) {
            let _ = std::fs::remove_file(&launcher);
            return Err(error);
        }
        Ok(Session {
            name: name.to_owned(),
            tmux: self.clone(),
            launcher: Some(launcher),
        })
    }

    /// An existing driver session, for the CLI's later commands.
    pub fn existing(&self, name: &str) -> Result<Session, DriverError> {
        if !self.exists(name)? {
            return Err(DriverError::Tmux {
                action: "find",
                detail: format!("no tmux session named {name}"),
            });
        }
        Ok(Session {
            name: name.to_owned(),
            tmux: self.clone(),
            launcher: None,
        })
    }

    fn checked<S: AsRef<std::ffi::OsStr>>(
        &self,
        action: &'static str,
        args: &[S],
        deadline: Option<Instant>,
    ) -> Result<String, DriverError> {
        let output = self.run(action, args, deadline)?;
        if !output.success {
            return Err(DriverError::Tmux {
                action,
                detail: output.stderr.trim().to_owned(),
            });
        }
        Ok(output.stdout)
    }

    /// Run one tmux command, killing it if it outlives the call bound (or
    /// `deadline`, if sooner) or prints more than [`OUTPUT_CAP`].
    fn run<S: AsRef<std::ffi::OsStr>>(
        &self,
        action: &'static str,
        args: &[S],
        deadline: Option<Instant>,
    ) -> Result<Output, DriverError> {
        let failed = |detail: String| DriverError::Tmux { action, detail };
        let bound = monotonic_now() + self.call_bound;
        let deadline = deadline.map_or(bound, |deadline| deadline.min(bound));
        let mut command = Command::new(&self.program);
        if let Some(socket) = &self.socket {
            command.args(["-L", socket]);
        }
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        // Its own process group, so a failed call can kill whatever tmux
        // started that still holds its pipes.
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        let mut child = command
            .spawn()
            .map_err(|error| failed(format!("could not run tmux: {error}")))?;
        let overflow = Arc::new(AtomicBool::new(false));
        // One slot per stream, so a reader never blocks on its send.
        let (sender, outputs) = mpsc::sync_channel(2);
        let mut readers = 0;
        let pipes = [
            child
                .stdout
                .take()
                .map(|pipe| drain(Stream::Stdout, pipe, &overflow, sender.clone())),
            child
                .stderr
                .take()
                .map(|pipe| drain(Stream::Stderr, pipe, &overflow, sender)),
        ];
        for started in pipes.into_iter().flatten() {
            match started {
                Ok(()) => readers += 1,
                Err(error) => {
                    abandon(&mut child, &outputs, readers);
                    return Err(failed(format!("could not read its output: {error}")));
                }
            }
        }
        let status = loop {
            if overflow.load(Ordering::Relaxed) {
                abandon(&mut child, &outputs, readers);
                return Err(failed(format!("printed more than {OUTPUT_CAP} bytes")));
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if monotonic_now() >= deadline => {
                    abandon(&mut child, &outputs, readers);
                    return Err(failed(format!(
                        "did not finish within {} ms",
                        self.call_bound.as_millis()
                    )));
                }
                Ok(None) => thread::sleep(CHILD_POLL),
                Err(error) => {
                    abandon(&mut child, &outputs, readers);
                    return Err(failed(format!("could not wait for tmux: {error}")));
                }
            }
        };
        // tmux has exited, but a process it started may still hold its pipes;
        // the readers get only what is left of the same deadline, and then
        // that process's group is killed so they finish too.
        let (mut stdout, mut stderr) = (Ok(Vec::new()), Ok(Vec::new()));
        for received in 0..readers {
            let remaining = deadline.saturating_duration_since(monotonic_now());
            let Ok((stream, read)) = outputs.recv_timeout(remaining) else {
                abandon(&mut child, &outputs, readers - received);
                return Err(failed(
                    "exited but its output stayed open past its bound".to_owned(),
                ));
            };
            match stream {
                Stream::Stdout => stdout = read,
                Stream::Stderr => stderr = read,
            }
        }
        let read = |result: std::io::Result<Vec<u8>>| {
            result.map_err(|error| failed(format!("output read failed: {error}")))
        };
        let (stdout, stderr) = (read(stdout)?, read(stderr)?);
        if overflow.load(Ordering::Relaxed) {
            return Err(failed(format!("printed more than {OUTPUT_CAP} bytes")));
        }
        Ok(Output {
            success: status.success(),
            stdout: String::from_utf8(stdout)
                .map_err(|error| failed(format!("output was not UTF-8: {error}")))?,
            stderr: String::from_utf8_lossy(&stderr).into_owned(),
        })
    }
}

struct Output {
    success: bool,
    stdout: String,
    stderr: String,
}

#[derive(Clone, Copy)]
enum Stream {
    Stdout,
    Stderr,
}

/// Read a pipe on its own thread and send what it read, raising `overflow`
/// once it passes [`OUTPUT_CAP`]. The thread ends when every write end of
/// the pipe closes, which [`abandon`] forces when a call fails.
fn drain(
    stream: Stream,
    pipe: impl Read + Send + 'static,
    overflow: &Arc<AtomicBool>,
    sender: mpsc::SyncSender<(Stream, std::io::Result<Vec<u8>>)>,
) -> std::io::Result<()> {
    let overflow = Arc::clone(overflow);
    thread::Builder::new()
        .name(READER_THREAD.to_owned())
        .spawn(move || {
            let mut bytes = Vec::new();
            let limit = u64::try_from(OUTPUT_CAP)
                .unwrap_or(u64::MAX)
                .saturating_add(1);
            let read = pipe.take(limit).read_to_end(&mut bytes).map(|_| bytes);
            if read.as_ref().is_ok_and(|bytes| bytes.len() > OUTPUT_CAP) {
                overflow.store(true, Ordering::Relaxed);
            }
            // The receiver is gone once its call gave up; nothing is owed then.
            let _ = sender.send((stream, read));
        })
        .map(drop)
}

/// End a failed call: kill tmux's whole process group, which closes the
/// pipe write ends that tmux and anything it started hold, reap tmux, then
/// wait up to [`CLEANUP_BOUND`] for the `pending` readers to finish. A
/// process that left the group can still hold a pipe; its reader then
/// outlives the call.
fn abandon(
    child: &mut std::process::Child,
    outputs: &mpsc::Receiver<(Stream, std::io::Result<Vec<u8>>)>,
    pending: usize,
) {
    let deadline = monotonic_now() + CLEANUP_BOUND;
    kill_group(child.id(), deadline);
    let _ = child.kill();
    reap_by(child, deadline);
    for _ in 0..pending {
        let remaining = deadline.saturating_duration_since(monotonic_now());
        if outputs.recv_timeout(remaining).is_err() {
            break;
        }
    }
}

/// Send SIGKILL to the process group `leader` heads. The group outlives a
/// reaped leader while any member remains, so its id cannot be reused.
#[cfg(unix)]
fn kill_group(leader: u32, deadline: Instant) {
    let killed = Command::new("kill")
        .args(["-s", "KILL", "--", &format!("-{leader}")])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut kill) = killed {
        reap_by(&mut kill, deadline);
    }
}

#[cfg(not(unix))]
fn kill_group(_leader: u32, _deadline: Instant) {}

/// Wait for `child` to exit, giving up at `deadline` after killing it.
fn reap_by(child: &mut std::process::Child, deadline: Instant) {
    while let Ok(None) = child.try_wait() {
        if monotonic_now() >= deadline {
            let _ = child.kill();
            return;
        }
        thread::sleep(CHILD_POLL);
    }
}

static LAUNCHERS: AtomicU64 = AtomicU64::new(0);

/// Write the script tmux runs for `launch`. It removes itself, changes to
/// the requested directory, exports the environment and execs the command,
/// every value single-quoted, so tmux only ever parses the script's path.
fn write_launcher(name: &str, launch: &Launch) -> Result<PathBuf, DriverError> {
    let refused = |what: String| Err(DriverError::Name(what));
    let mut script = String::from("rm -f -- \"$0\"\n");
    if let Some(directory) = &launch.directory {
        let Some(text) = directory.to_str() else {
            return refused(format!("cwd {directory:?} is not UTF-8"));
        };
        if !directory.is_absolute() || !directory.is_dir() || text.contains('\0') {
            return refused(format!(
                "cwd {text:?} must be an existing absolute directory"
            ));
        }
        script.push_str(&format!("cd -- {} || exit 127\n", shell_quote(text)));
    }
    for (key, value) in &launch.environment {
        if !environment_key(key) || value.contains('\0') {
            return refused(format!(
                "environment {key:?} must be KEY=VALUE with KEY of letters, digits or '_', not starting with a digit"
            ));
        }
        script.push_str(&format!("export {key}={}\n", shell_quote(value)));
    }
    if launch
        .command
        .iter()
        .any(|argument| argument.contains('\0'))
    {
        return refused("a command argument contains a NUL byte".to_owned());
    }
    let command: Vec<String> = launch
        .command
        .iter()
        .map(|argument| shell_quote(argument))
        .collect();
    script.push_str(&format!("exec {}\n", command.join(" ")));
    let path = std::env::temp_dir().join(format!(
        "{name}-{}-{}.sh",
        std::process::id(),
        LAUNCHERS.fetch_add(1, Ordering::Relaxed)
    ));
    let plain = path
        .to_str()
        .is_some_and(|text| !text.chars().any(|c| c.is_control() || c == ';'));
    if !plain {
        return refused(format!(
            "launcher path {path:?} has a ';' or control character; set TMPDIR to a plain directory"
        ));
    }
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o700);
    options
        .open(&path)
        .and_then(|mut file| std::io::Write::write_all(&mut file, script.as_bytes()))
        .map_err(|error| DriverError::Tmux {
            action: "start",
            detail: format!("launcher {} could not be written: {error}", path.display()),
        })?;
    Ok(path)
}

/// A shell environment name: `[A-Za-z_][A-Za-z0-9_]*`.
fn environment_key(key: &str) -> bool {
    key.starts_with(|first: char| first.is_ascii_alphabetic() || first == '_')
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

/// A running driver session.
#[derive(Debug)]
pub struct Session {
    name: String,
    tmux: Tmux,
    /// The launcher this driver wrote, until the program removes it.
    launcher: Option<PathBuf>,
}

impl Session {
    /// [`Tmux::start`] on the default tmux.
    pub fn start(name: &str, launch: &Launch) -> Result<Self, DriverError> {
        Tmux::default().start(name, launch)
    }

    /// [`Tmux::existing`] on the default tmux.
    pub fn existing(name: &str) -> Result<Self, DriverError> {
        Tmux::default().existing(name)
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The exact-name target of this session's active pane.
    fn pane(&self) -> String {
        format!("={}:", self.name)
    }

    /// Keys by tmux name (`Enter`, `C-c`, `Up`, `Escape`, `F1`, ...) or as a
    /// single character, which is sent as its bytes.
    pub fn send_keys(&self, keys: &[&str]) -> Result<(), DriverError> {
        let pane = self.pane();
        for key in keys {
            let mut chars = key.chars();
            if let (Some(_), None) = (chars.next(), chars.next()) {
                self.send_bytes(key.as_bytes())?;
            } else if key_name(key) {
                self.tmux
                    .checked("send-keys", &["send-keys", "-t", &pane, "--", key], None)?;
            } else {
                return Err(DriverError::Name(format!(
                    "{key:?} is not a tmux key name; send text with send_text"
                )));
            }
        }
        Ok(())
    }

    /// Literal text, typed byte for byte.
    pub fn send_text(&self, text: &str) -> Result<(), DriverError> {
        self.send_bytes(text.as_bytes())
    }

    fn send_bytes(&self, bytes: &[u8]) -> Result<(), DriverError> {
        let pane = self.pane();
        // A tmux command line is bounded; send long text in pieces.
        for chunk in bytes.chunks(512) {
            let mut args = vec![
                "send-keys".to_owned(),
                "-t".into(),
                pane.clone(),
                "-H".into(),
            ];
            args.extend(chunk.iter().map(|byte| format!("{byte:02x}")));
            self.tmux.checked("send-keys", &args, None)?;
        }
        Ok(())
    }

    /// The exact visible screen text, one line per row.
    pub fn capture(&self) -> Result<String, DriverError> {
        self.capture_by(None)
    }

    fn capture_by(&self, deadline: Option<Instant>) -> Result<String, DriverError> {
        self.tmux.checked(
            "capture-pane",
            &["capture-pane", "-p", "-t", &self.pane()],
            deadline,
        )
    }

    /// The scrollback history followed by the visible screen. When the
    /// program exits, tmux moves its final screen into history and shows a
    /// "Pane is dead" line, so this is where that screen is found.
    pub fn capture_with_history(&self) -> Result<String, DriverError> {
        self.capture_with_history_by(None)
    }

    fn capture_with_history_by(&self, deadline: Option<Instant>) -> Result<String, DriverError> {
        self.tmux.checked(
            "capture-pane",
            &["capture-pane", "-p", "-S", "-", "-t", &self.pane()],
            deadline,
        )
    }

    /// The program's exit status once it has exited; `None` while it runs.
    pub fn exit_status(&self) -> Result<Option<i32>, DriverError> {
        self.exit_status_by(None)
    }

    fn exit_status_by(&self, deadline: Option<Instant>) -> Result<Option<i32>, DriverError> {
        let state = self.tmux.checked(
            "display-message",
            &[
                "display-message",
                "-p",
                "-t",
                &self.pane(),
                "#{pane_dead} #{pane_dead_status}",
            ],
            deadline,
        )?;
        let mut fields = state.split_whitespace();
        if fields.next() != Some("1") {
            return Ok(None);
        }
        let status = fields
            .next()
            .and_then(|status| status.parse().ok())
            .unwrap_or(-1);
        Ok(Some(status))
    }

    /// Poll the screen until it contains `text` and return that screen.
    /// Fails with the last screen on timeout or when the program exits first.
    /// No tmux call runs past the wait's own deadline.
    pub fn wait_for(&self, text: &str, timeout: Duration) -> Result<String, DriverError> {
        let deadline = monotonic_now() + timeout;
        let mut screen = String::new();
        // A tmux call cut short by the wait's own deadline is a timeout.
        let timed_out = |error: DriverError, screen: &str| {
            if monotonic_now() >= deadline {
                DriverError::Timeout {
                    text: text.to_owned(),
                    screen: screen.to_owned(),
                }
            } else {
                error
            }
        };
        loop {
            screen = self
                .capture_by(Some(deadline))
                .map_err(|error| timed_out(error, &screen))?;
            if screen.contains(text) {
                return Ok(screen);
            }
            let exited = self
                .exit_status_by(Some(deadline))
                .map_err(|error| timed_out(error, &screen))?;
            if let Some(status) = exited {
                let screen = self
                    .capture_with_history_by(Some(deadline))
                    .map_err(|error| timed_out(error, &screen))?;
                if screen.contains(text) {
                    return Ok(screen);
                }
                return Err(DriverError::Exited {
                    text: text.to_owned(),
                    status,
                    screen,
                });
            }
            if monotonic_now() >= deadline {
                return Err(DriverError::Timeout {
                    text: text.to_owned(),
                    screen,
                });
            }
            thread::sleep(POLL_INTERVAL);
        }
    }

    /// Kill the session and its program, and remove the launcher if the
    /// program was stopped before it could remove it.
    pub fn stop(self) -> Result<(), DriverError> {
        let target = format!("={}", self.name);
        let stopped = self
            .tmux
            .checked("kill-session", &["kill-session", "-t", &target], None)
            .map(drop);
        if let Some(launcher) = &self.launcher {
            let _ = std::fs::remove_file(launcher);
        }
        stopped
    }
}

/// tmux key-name spelling: `Enter`, `C-c`, `M-Left`, `F12`, `BSpace`, ...
/// Nothing that could read as an option or end in a command separator.
fn key_name(key: &str) -> bool {
    key.len() <= 32
        && key.starts_with(|first: char| first.is_ascii_alphabetic())
        && key
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '-')
}

/// One POSIX single-quoted word.
fn shell_quote(argument: &str) -> String {
    format!("'{}'", argument.replace('\'', r"'\''"))
}

/// The driver's one monotonic clock read, ratified for this dev-only crate.
/// It bounds a test tool's waits and is never recorded anywhere.
#[allow(clippy::disallowed_methods)]
fn monotonic_now() -> Instant {
    Instant::now()
}

#[cfg(test)]
mod tests;
