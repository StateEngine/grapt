//! Bounded lifecycle helpers for the handover journeys that coordinate with a
//! live product: the harness observation budget, a bounded pipe watch, and a
//! held child whose release, failure and teardown are each bounded.
//!
//! These serve the handover journeys only; they are not a process supervisor.
//! Every wait here is a HARNESS choice. Expiry leaves the event unobserved and
//! RED; it is never evidence that the product broke a deadline, because the
//! product states no readiness deadline. Each failed observation is returned
//! as a string that starts with its outcome name.

use std::io::Read;
use std::process::{Child, ChildStdin, ExitStatus, Stdio};
use std::sync::{mpsc, Arc, Mutex, OnceLock, PoisonError};
use std::time::Duration;

use tokio::time::Instant;

use super::*;

/// Ceiling on observing ONE child event: readiness, an announcement or an
/// exit. A correct startup was still running at 5.5 s under measured I/O
/// pressure (trace 20260924192435-4327654641e44b51); this value is a
/// diagnostic choice pending measured distributions, not a startup SLO.
pub(super) const OBSERVATION_BUDGET: Duration = Duration::from_secs(120);

/// Ceiling on the OBSERVATION waits of one test binary, measured from its
/// first wait. It bounds only waiting for child events; teardown, snapshots
/// and product work outside a wait are bounded separately (CONTRACT.md).
/// Once spent, a wait returns `invocation-budget-exhausted` without observing.
const INVOCATION_BUDGET: Duration = Duration::from_secs(900);

/// A failed or abandoned child is killed and then reaped within this bound.
/// SIGKILL cannot bound a process in uninterruptible I/O; a child still
/// unreaped here is reported and left to the governed process-group cleanup.
const REAP_BUDGET: Duration = Duration::from_secs(5);

/// How long a finished child's pipes may take to reach EOF before the
/// retained output is reported as not fully drained.
const PIPE_SETTLE: Duration = Duration::from_secs(2);

const POLL: Duration = Duration::from_millis(20);
const RETAINED_BYTES: usize = 64 * 1_024;
const LINE_BYTES: usize = 512;
const CHUNK_BYTES: usize = 4_096;

pub(super) const ANNOUNCE: &str = "secode-fixture boundary=before-retirement incumbent_pid=";

static INVOCATION: OnceLock<Instant> = OnceLock::new();

/// What is left of `budget` once `spent` of the invocation ceiling is gone;
/// `None` once the ceiling is spent.
fn clamp(budget: Duration, spent: Duration) -> Option<Duration> {
    let remaining = INVOCATION_BUDGET.saturating_sub(spent);
    (!remaining.is_zero()).then(|| budget.min(remaining))
}

fn window(budget: Duration) -> Option<Duration> {
    clamp(budget, INVOCATION.get_or_init(Instant::now).elapsed())
}

fn exhausted() -> String {
    format!(
        "invocation-budget-exhausted: the {}s observation ceiling is spent; not observed",
        INVOCATION_BUDGET.as_secs()
    )
}

#[cfg(feature = "handover-crash-fixture")]
/// Readiness bytes and how long the observation took, which is recorded on
/// success as well as failure so a later revision can derive the budget.
pub(super) struct Ready {
    pub(super) bytes: Vec<u8>,
    pub(super) elapsed: Duration,
}

#[cfg(feature = "handover-crash-fixture")]
/// Await `serve-ready.json` within `budget`.
pub(super) fn await_ready(
    path: &Path,
    budget: Duration,
    exited: impl FnMut() -> Result<bool, String>,
) -> Result<Ready, String> {
    let started = Instant::now();
    observe_ready(path, window(budget), exited).map(|bytes| Ready {
        bytes,
        elapsed: started.elapsed(),
    })
}

/// Outcomes: the bytes; `exited-unready`, a product outcome; `alive-unobserved`
/// on expiry, where late readiness is retained, not excluded; `unreadable-ready`
/// for a receipt that exists but cannot be read, which is never absence;
/// `exit-observation-error`, which is never an exit; and
/// `invocation-budget-exhausted`, which observes nothing.
fn observe_ready(
    path: &Path,
    limit: Option<Duration>,
    mut exited: impl FnMut() -> Result<bool, String>,
) -> Result<Vec<u8>, String> {
    let limit = limit.ok_or_else(exhausted)?;
    let started = Instant::now();
    loop {
        let gone = exited().map_err(|error| format!("exit-observation-error: {error}"))?;
        match fs::read(path) {
            Ok(bytes) => return Ok(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("unreadable-ready: {error}")),
        }
        let elapsed_ms = started.elapsed().as_millis();
        if gone {
            return Err(format!("exited-unready after {elapsed_ms}ms"));
        }
        if started.elapsed() >= limit {
            return Err(format!(
                "alive-unobserved after {elapsed_ms}ms of a {}ms budget; late readiness retained",
                limit.as_millis()
            ));
        }
        std::thread::sleep(POLL);
    }
}

#[derive(Default)]
struct Retained {
    bytes: Vec<u8>,
    dropped: u64,
}

/// A reader thread over one child pipe. It keeps at most `RETAINED_BYTES`,
/// scans lines of at most `LINE_BYTES` without `read_line`, and forwards only
/// the first line starting with its marker through a one-slot channel. It is
/// never joined: a pipe held open by some other process leaves it behind.
pub(super) struct Pipe {
    retained: Arc<Mutex<Retained>>,
    marker: mpsc::Receiver<String>,
    ended: mpsc::Receiver<String>,
    end: Option<String>,
}

impl Pipe {
    pub(super) fn watch(
        pipe: Option<impl Read + Send + 'static>,
        marker: Option<&'static str>,
    ) -> Self {
        let retained = Arc::new(Mutex::new(Retained::default()));
        let (found, marker_rx) = mpsc::sync_channel(1);
        let (ended, ended_rx) = mpsc::sync_channel(1);
        match pipe {
            Some(pipe) => {
                let shared = Arc::clone(&retained);
                std::thread::spawn(move || {
                    let end = scan(pipe, &shared, marker, &found);
                    let _ = ended.try_send(end);
                });
            }
            None => {
                let _ = ended.try_send("absent".to_owned());
            }
        }
        Self {
            retained,
            marker: marker_rx,
            ended: ended_rx,
            end: None,
        }
    }

    /// The first marker line, or why it was not observed: `ended-unannounced`
    /// when the pipe closed without one, `alive-unobserved` on expiry.
    pub(super) fn await_marker(&self, budget: Duration) -> Result<(String, Duration), String> {
        let limit = window(budget).ok_or_else(exhausted)?;
        let started = Instant::now();
        match self.marker.recv_timeout(limit) {
            Ok(line) => Ok((line, started.elapsed())),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(format!(
                "ended-unannounced after {}ms",
                started.elapsed().as_millis()
            )),
            Err(mpsc::RecvTimeoutError::Timeout) => Err(format!(
                "alive-unobserved after {}ms",
                started.elapsed().as_millis()
            )),
        }
    }

    /// The retained text once the pipe has ended, waiting at most
    /// `PIPE_SETTLE`; a pipe still open is named rather than waited on.
    pub(super) fn settle(&mut self) -> (String, String) {
        if self.end.is_none() {
            self.end = Some(
                self.ended
                    .recv_timeout(PIPE_SETTLE)
                    .unwrap_or_else(|_| format!("open after {}ms", PIPE_SETTLE.as_millis())),
            );
        }
        let retained = self.retained.lock().unwrap_or_else(PoisonError::into_inner);
        let text = String::from_utf8_lossy(&retained.bytes).into_owned();
        let summary = format!(
            "{}B retained, {}B dropped, end={}",
            retained.bytes.len(),
            retained.dropped,
            self.end.as_deref().unwrap_or("unknown")
        );
        (text, summary)
    }

    pub(super) fn render(&mut self) -> String {
        let (text, summary) = self.settle();
        format!("[{summary}] {text}")
    }
}

fn scan(
    mut pipe: impl Read,
    retained: &Mutex<Retained>,
    marker: Option<&'static str>,
    found: &mpsc::SyncSender<String>,
) -> String {
    let mut chunk = [0_u8; CHUNK_BYTES];
    let mut line = Vec::with_capacity(LINE_BYTES);
    let mut overlong = false;
    let mut announced = marker.is_none();
    loop {
        let read = match pipe.read(&mut chunk) {
            Ok(0) => return "eof".to_owned(),
            Ok(read) => read,
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(error) => return format!("read error: {error}"),
        };
        let bytes = &chunk[..read];
        {
            let mut kept = retained.lock().unwrap_or_else(PoisonError::into_inner);
            let room = RETAINED_BYTES.saturating_sub(kept.bytes.len()).min(read);
            kept.bytes.extend_from_slice(&bytes[..room]);
            kept.dropped += (read - room) as u64;
        }
        for &byte in bytes {
            if byte != b'\n' {
                if line.len() < LINE_BYTES {
                    line.push(byte);
                } else {
                    overlong = true;
                }
                continue;
            }
            if !announced && !overlong {
                let text = String::from_utf8_lossy(&line);
                if marker.is_some_and(|marker| text.starts_with(marker)) {
                    announced = true;
                    let _ = found.try_send(text.into_owned());
                }
            }
            line.clear();
            overlong = false;
        }
    }
}

/// A child whose stdin the test holds. Dropping the stdin is EOF, which
/// releases a fixture hold; every other way out of the test goes through
/// `Drop`, which kills the child and observes its exit within `REAP_BUDGET`.
pub(super) struct Held {
    child: Child,
    stdin: Option<ChildStdin>,
    label: &'static str,
    spawned: Instant,
    settled: bool,
}

impl Held {
    pub(super) fn spawn(label: &'static str, command: &mut Command) -> Self {
        let spawned = Instant::now();
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .unwrap_or_else(|error| panic!("{label} spawn: {error}"));
        let stdin = child.stdin.take();
        Self {
            child,
            stdin,
            label,
            spawned,
            settled: false,
        }
    }

    pub(super) fn pid(&self) -> u32 {
        self.child.id()
    }

    pub(super) fn stdout(&mut self) -> Option<std::process::ChildStdout> {
        self.child.stdout.take()
    }

    pub(super) fn stderr(&mut self) -> Option<std::process::ChildStderr> {
        self.child.stderr.take()
    }

    #[cfg(feature = "handover-crash-fixture")]
    /// Hand the stdin to another owner, whose drop is then the release.
    pub(super) fn take_stdin(&mut self) -> Option<ChildStdin> {
        self.stdin.take()
    }

    /// EOF on the child's stdin. This alone proves nothing about its exit.
    pub(super) fn release(&mut self) {
        self.stdin = None;
    }

    /// Whether the child has exited. Reaps it, so Linux process observation
    /// never mistakes a zombie for a live writer. An observation error is
    /// returned as such, never read as an exit.
    pub(super) fn exited(&mut self) -> Result<bool, String> {
        match self.child.try_wait() {
            Ok(Some(_)) => {
                self.settled = true;
                Ok(true)
            }
            Ok(None) => Ok(false),
            Err(error) => Err(format!("{}: {error}", self.label)),
        }
    }

    /// Await the child's own exit within `budget`.
    pub(super) fn await_exit(
        &mut self,
        budget: Duration,
    ) -> Result<(ExitStatus, Duration), String> {
        let limit = window(budget).ok_or_else(exhausted)?;
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => {
                    self.settled = true;
                    return Ok((status, started.elapsed()));
                }
                Ok(None) if started.elapsed() >= limit => {
                    return Err(format!(
                        "alive-unobserved: {} did not exit within {}ms",
                        self.label,
                        limit.as_millis()
                    ))
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(error) => {
                    return Err(format!("exit-observation-error: {}: {error}", self.label))
                }
            }
        }
    }

    /// The crash cut: kill, then observe the exit within `REAP_BUDGET`.
    pub(super) fn kill(&mut self) -> Result<ExitStatus, String> {
        let _ = self.child.kill();
        let reaped = self.reap();
        self.settled = reaped.is_ok();
        reaped
    }

    /// Evidence for a failed observation. The snapshot comes first, because
    /// `try_wait` reaps an exited child and its `/proc` entry goes with it;
    /// then the child is killed and reaped within bounds.
    pub(super) fn failure(&mut self, outcome: &str) -> String {
        let snapshot = evidence::product_snapshot(self.pid(), None);
        let before = self.child.try_wait();
        let reaped = self.kill();
        format!(
            "[{} pid={}] {outcome}; since_spawn_ms={}; status_before_kill={before:?}; \
             {snapshot}; reaped={reaped:?}",
            self.label,
            self.pid(),
            self.spawned.elapsed().as_millis()
        )
    }

    fn reap(&mut self) -> Result<ExitStatus, String> {
        let started = Instant::now();
        loop {
            match self.child.try_wait() {
                Ok(Some(status)) => return Ok(status),
                Ok(None) if started.elapsed() >= REAP_BUDGET => {
                    return Err(format!(
                        "unreaped after {}ms; left to the governed process-group cleanup",
                        REAP_BUDGET.as_millis()
                    ))
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(error) => return Err(format!("reap observation: {error}")),
            }
        }
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        self.stdin = None;
        if let Err(error) = self.kill() {
            eprintln!("{} pid={}: {error}", self.label, self.pid());
        }
    }
}

#[cfg(feature = "handover-crash-fixture")]
/// A store the product created on its first launch, serving for `window_ms`.
///
/// The first launch creates a current store; a second launch opens that Ready
/// store as it is, without creating it again.
pub(super) fn product_installation(label: &str, build: &str, window_ms: u32) -> PathBuf {
    let root = scratch(label);
    write_inputs(&root, build);
    let serve = || {
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["serve", "--service"])
            .arg(&root)
            .args(["--port", "0"])
            .output()
            .unwrap_or_else(|error| panic!("serve: {error}"))
    };
    for launch in ["first", "second"] {
        let served = serve();
        assert!(
            served.status.success(),
            "{launch} product launch: {}",
            String::from_utf8_lossy(&served.stdout)
        );
        assert!(matches!(
            grapt_store::preflight_read_only(&root.join("database.sqlite3")),
            grapt_store::StoreReadiness::Ready
        ));
    }
    set_window(&root, window_ms);
    root
}

#[cfg(feature = "handover-crash-fixture")]
pub(super) fn set_window(root: &Path, window_ms: u32) {
    let path = root.join("service.conf");
    let service = fs::read_to_string(&path).unwrap_or_else(|error| panic!("service: {error}"));
    fs::write(
        &path,
        service.replace(
            "serve_window_ms=250",
            &format!("serve_window_ms={window_ms}"),
        ),
    )
    .unwrap_or_else(|error| panic!("service window: {error}"));
}

/// A disposable migrating installation with a legacy store.
pub(super) fn installation(label: &str) -> PathBuf {
    let root = scratch(label);
    write_inputs(&root, "secode-rust@lifecycle");
    legacy_database(&root.join("database.sqlite3"));
    root
}

#[cfg(feature = "handover-crash-fixture")]
pub(super) fn held_serve(root: &Path, hold_at: &str) -> Held {
    Held::spawn(
        "held-serve",
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["serve", "--service"])
            .arg(root)
            .args(["--port", "0"])
            .env("GRAPT_FIXTURE_HOLD_AT", hold_at)
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null()),
    )
}

/// A handover whose stdout and stderr the test watches.
pub(super) fn watched_handover(
    root: &Path,
    run_id: &str,
    build: &str,
    env: &[(&str, &str)],
) -> Held {
    let mut command =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
    command
        .args(["handover", "--service"])
        .arg(root)
        .arg("--run-root")
        .arg(root.join(format!("run-{run_id}")))
        .args(["--run-id", run_id, "--engine-version", build])
        .envs(env.iter().copied())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    Held::spawn("handover", &mut command)
}

#[cfg(feature = "handover-crash-fixture")]
pub(super) fn fixture_storage(root: &Path) -> (usize, u64) {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0;
    let mut bytes = 0_u64;
    while let Some(directory) = pending.pop() {
        for entry in
            fs::read_dir(directory).unwrap_or_else(|error| panic!("storage census: {error}"))
        {
            let entry = entry.unwrap_or_else(|error| panic!("storage entry: {error}"));
            entries += 1;
            assert!(entries <= 256, "fixture storage exceeds 256 entries");
            let kind = entry
                .file_type()
                .unwrap_or_else(|error| panic!("entry type: {error}"));
            assert!(!kind.is_symlink(), "fixture contains a symbolic link");
            if kind.is_dir() {
                pending.push(entry.path());
            } else {
                bytes += entry
                    .metadata()
                    .unwrap_or_else(|error| panic!("entry metadata: {error}"))
                    .len();
                assert!(bytes <= 64 * 1024 * 1024, "fixture storage exceeds 64 MiB");
            }
        }
    }
    (entries, bytes)
}

#[test]
fn the_invocation_ceiling_clamps_each_wait_and_then_observes_nothing() {
    let budget = Duration::from_secs(120);
    assert_eq!(clamp(budget, Duration::ZERO), Some(budget));
    assert_eq!(
        clamp(budget, Duration::from_secs(850)),
        Some(Duration::from_secs(50))
    );
    assert_eq!(clamp(budget, INVOCATION_BUDGET), None);
    assert_eq!(clamp(budget, Duration::from_secs(1_000)), None);

    // A spent ceiling refuses even a receipt that is already present.
    let root = scratch("ceiling-spent");
    let path = root.join("serve-ready.json");
    fs::write(&path, b"{}").unwrap_or_else(|error| panic!("receipt: {error}"));
    let spent = observe_ready(&path, None, || Ok(false));
    assert!(
        spent
            .as_ref()
            .is_err_and(|state| state.starts_with("invocation-budget-exhausted")),
        "{spent:?}"
    );
    assert_eq!(
        observe_ready(&path, Some(Duration::ZERO), || Ok(false)),
        Ok(b"{}".to_vec())
    );
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn readiness_outcomes_keep_absent_unreadable_exited_and_unobservable_apart() {
    let root = scratch("readiness-outcomes");
    let missing = root.join("serve-ready.json");
    // The outcome is the first word of a failed observation.
    let outcome = |path: &Path, exited: Result<bool, String>| {
        observe_ready(path, Some(Duration::ZERO), move || exited.clone()).map_err(|state| {
            state
                .split([':', ' '])
                .next()
                .unwrap_or_default()
                .to_owned()
        })
    };
    assert_eq!(
        outcome(&missing, Ok(true)),
        Err("exited-unready".to_owned())
    );
    assert_eq!(
        outcome(&missing, Ok(false)),
        Err("alive-unobserved".to_owned())
    );
    // A directory where the receipt belongs exists but cannot be read.
    assert_eq!(
        outcome(&root, Ok(false)),
        Err("unreadable-ready".to_owned())
    );
    assert_eq!(
        outcome(&missing, Err("waitpid failed".to_owned())),
        Err("exit-observation-error".to_owned())
    );
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn the_pipe_watch_bounds_lines_and_retention_and_forwards_one_marker() {
    let mut input = vec![b'x'; LINE_BYTES + 1];
    input.extend_from_slice(ANNOUNCE.as_bytes());
    input.extend_from_slice(b"7 hidden-by-overlong-line\n");
    input.extend_from_slice(ANNOUNCE.as_bytes());
    input.extend_from_slice(b"41\n");
    input.extend_from_slice(ANNOUNCE.as_bytes());
    input.extend_from_slice(b"42\n");
    input.resize(RETAINED_BYTES + 100, b'y');
    let mut pipe = Pipe::watch(Some(std::io::Cursor::new(input)), Some(ANNOUNCE));
    let (line, _) = pipe
        .await_marker(Duration::from_secs(5))
        .unwrap_or_else(|state| panic!("{state}"));
    assert_eq!(line, format!("{ANNOUNCE}41"));
    let (text, summary) = pipe.settle();
    assert_eq!(text.len(), RETAINED_BYTES);
    assert!(summary.contains("100B dropped, end=eof"), "{summary}");
    let marker = pipe.await_marker(Duration::from_secs(1));
    assert!(
        marker
            .as_ref()
            .is_err_and(|state| state.starts_with("ended-unannounced")),
        "a second marker was forwarded: {marker:?}"
    );
}

/// HB1-HB2, RT2+TD1, RT4, RT5 and OW1: the fixture seams and harness bounds, driven
/// through the real binary.
mod witnesses {
    use super::*;

    #[cfg(feature = "handover-crash-fixture")]
    #[test]
    fn late_readiness_inside_the_budget_is_observed_and_expiry_is_red_and_reaped() {
        // HB1: readiness held for 3 s past spawn, beyond the old 2 s fixture
        // limit, is observed inside the harness budget.
        let root = installation("hold-before-ready");
        let mut child = held_serve(&root, "serve-before-ready");
        let stdin = child.take_stdin();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(3));
            drop(stdin);
        });
        let ready_path = root.join("serve-ready.json");
        let ready = await_ready(&ready_path, OBSERVATION_BUDGET, || child.exited());
        release
            .join()
            .unwrap_or_else(|_| panic!("release thread panicked"));
        let ready = ready.unwrap_or_else(|state| {
            panic!("HB1 late readiness unobserved: {}", child.failure(&state))
        });
        assert!(
            ready.elapsed >= std::time::Duration::from_secs(3),
            "the hold did not hold"
        );
        let (status, _) = child
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("{}", child.failure(&state)));
        assert!(status.success(), "released serve: {status:?}");
        fs::remove_dir_all(&root).unwrap_or_else(|error| panic!("cleanup: {error}"));

        // HB2: the same hold, never released, observed with a 200 ms budget passed
        // into the same function, on a fresh installation (the first is migrated).
        // Expiry is named; the child is killed and its exit observed.
        let root = installation("hold-never-released");
        let mut child = held_serve(&root, "serve-before-ready");
        let expired = await_ready(
            &root.join("serve-ready.json"),
            std::time::Duration::from_millis(200),
            || child.exited(),
        );
        let state = match expired {
            Ok(_) => panic!("a held child published readiness"),
            Err(state) => state,
        };
        let failure = child.failure(&state);
        assert!(state.starts_with("alive-unobserved"), "{failure}");
        assert!(failure.contains("status_before_kill=Ok(None)"), "{failure}");
        assert!(
            failure.contains("reaped=Ok("),
            "HB2 held child left unreaped: {failure}"
        );
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    #[cfg(feature = "handover-crash-fixture")]
    #[test]
    fn a_held_window_end_serves_until_released_then_drains_and_releases() {
        // RT2: a 1 ms window has elapsed long before a 1 s probe, yet the held
        // process still owns its registration and ready receipt.
        let root = installation("hold-window-end");
        set_window(&root, 1);
        let mut child = held_serve(&root, "serve-window-end");
        let ready_path = root.join("serve-ready.json");
        await_ready(&ready_path, OBSERVATION_BUDGET, || child.exited()).unwrap_or_else(|state| {
            panic!(
                "RT2 held serve did not stay ready: {}",
                child.failure(&state)
            )
        });
        let probe = child.await_exit(std::time::Duration::from_secs(1));
        assert!(
            probe.is_err(),
            "RT2 the window-end hold did not hold: {probe:?}"
        );
        assert!(ready_path.is_file(), "RT2 ready receipt retired while held");
        assert!(
            root.join("registration").is_file(),
            "RT2 registration released while held"
        );

        // TD1: EOF releases the hold; the ordinary drain releases both receipts
        // and the process exits by itself inside the budget.
        child.release();
        let (status, _) = child
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| {
                panic!("TD1 released hold did not drain: {}", child.failure(&state))
            });
        assert!(status.success(), "TD1 released serve: {status:?}");
        assert!(!ready_path.exists());
        assert!(!root.join("registration").exists());
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    #[cfg(feature = "handover-crash-fixture")]
    #[test]
    fn an_unwritable_announcement_exits_by_name_rather_than_proceeding() {
        // RT5: stderr's reader is gone, so the announce write fails (EPIPE) and
        // the handover exits 87 before any retirement or migration.
        let root = installation("announce-unwritable");
        register_incumbent(&root, ABSENT_PID);
        let mut child = watched_handover(
            &root,
            "shv-announce",
            "secode-rust@lifecycle",
            &[("GRAPT_FIXTURE_ANNOUNCE_AT", "before-retirement")],
        );
        drop(child.stderr());
        let (status, _) = child
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("{}", child.failure(&state)));
        assert_eq!(
            status.code(),
            Some(87),
            "RT5 announce write failure was not an exit 87: {status:?}"
        );
        assert!(root.join("registration").is_file());
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    /// OW1: the announce is published only after the registration read. The
    /// handover is held right after announcing while the test retires the
    /// incumbent completely, registration included. Had the announce preceded the
    /// read, the handover would find no incumbent and retire none.
    #[cfg(feature = "handover-crash-fixture")]
    #[test]
    fn publication_after_the_registration_read_cannot_release_the_incumbent_early() {
        let build = "secode-rust@order-witness";
        let root = product_installation("order-witness", build, 1);
        let mut incumbent = held_serve(&root, "serve-window-end");
        await_ready(&root.join("serve-ready.json"), OBSERVATION_BUDGET, || {
            incumbent.exited()
        })
        .unwrap_or_else(|state| panic!("{}", incumbent.failure(&state)));
        let mut handover = watched_handover(
            &root,
            "shv-order",
            build,
            &[
                ("GRAPT_FIXTURE_ANNOUNCE_AT", "before-retirement"),
                ("GRAPT_FIXTURE_HOLD_AT", "before-retirement"),
            ],
        );
        let mut stdout = Pipe::watch(handover.stdout(), None);
        let mut stderr = Pipe::watch(handover.stderr(), Some(ANNOUNCE));
        let (announce, _) = stderr
            .await_marker(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("{}; {}", handover.failure(&state), stderr.render()));
        assert_eq!(
            announce.strip_prefix(ANNOUNCE),
            Some(incumbent.pid().to_string().as_str())
        );
        incumbent.release();
        let (status, _) = incumbent
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("{}", incumbent.failure(&state)));
        assert!(status.success(), "incumbent exit {status:?}");
        assert!(
            !root.join("registration").exists(),
            "the incumbent kept its registration"
        );
        handover.release();
        let (status, _) = handover
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("{}; {}", handover.failure(&state), stdout.render()));
        let (text, drained) = stdout.settle();
        let result = receipt_line(&text);
        assert!(
            status.success(),
            "{status:?}: {result} ({drained}); {}",
            stderr.render()
        );
        assert_eq!(
            result["handover"]["retired_incumbent"], true,
            "OW1 publication preceded the registration read: {result}"
        );
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }

    /// RT4: with its stdin held open, an ordinary handover neither holds at its
    /// window end nor announces a boundary; it serves and exits on its own.
    #[cfg(not(feature = "handover-crash-fixture"))]
    #[test]
    fn ordinary_binary_ignores_the_fixture_lifecycle_selectors() {
        let root = installation("inactive-lifecycle");
        register_incumbent(&root, ABSENT_PID);
        let mut child = watched_handover(
            &root,
            "shv-lifecycle",
            "secode-rust@lifecycle",
            &[
                ("GRAPT_FIXTURE_HOLD_AT", "serve-window-end"),
                ("GRAPT_FIXTURE_ANNOUNCE_AT", "before-retirement"),
            ],
        );
        let mut stdout = Pipe::watch(child.stdout(), None);
        let mut stderr = Pipe::watch(child.stderr(), Some(ANNOUNCE));
        let (status, _) = child
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("RT4 ordinary build held: {}", child.failure(&state)));
        let announce = stderr.await_marker(OBSERVATION_BUDGET);
        assert!(
            announce
                .as_ref()
                .is_err_and(|state| state.starts_with("ended-unannounced")),
            "RT4 ordinary build announced: {announce:?}; stderr={}",
            stderr.render()
        );
        let (text, drained) = stdout.settle();
        assert!(status.success(), "{status:?}: {text} ({drained})");
        assert_eq!(receipt_line(&text)["outcome"], "served");
        fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
    }
}
