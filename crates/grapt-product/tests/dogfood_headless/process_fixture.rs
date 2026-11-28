use super::*;

#[path = "process_fixture/scenario_lifetime.rs"]
pub(super) mod scenario_lifetime;

#[cfg(target_os = "linux")]
#[path = "process_fixture/cmdline_readiness.rs"]
mod cmdline_readiness;

#[cfg(target_os = "linux")]
pub(super) const CHILD_LIFETIME_SECS: u64 = 5;

/// A spawned-child witness needs exec-visible argv, unlike an immediate dump.
#[cfg(target_os = "linux")]
pub(super) fn observe_spawned_child(child: &Child) -> Result<ChildrenObservation, String> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(CHILD_LIFETIME_SECS);
    cmdline_readiness::observe_ready(&LiveProc, std::process::id(), child.id(), deadline)
}

pub(super) fn cleanup(root: &Path) {
    // Retention is owned by the developer keep flag: a retained root must
    // survive teardown so the residue judge can observe it.
    if std::env::var_os("GRAPT_KEEP_TMP").is_some() {
        return;
    }
    force_cleanup(root)
}

/// Unconditional removal, for the tests that verify deletion itself.
pub(super) fn force_cleanup(root: &Path) {
    // A reaped Windows process can leave directory deletion pending briefly.
    // Retry only that sharing violation; every other failure stays immediate.
    for attempt in 0..100 {
        match fs::remove_dir_all(root) {
            Ok(()) => {
                if attempt > 0 {
                    eprintln!(
                        "fixture cleanup completed after {} sharing-violation retries",
                        attempt
                    );
                }
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) if cfg!(windows) && error.raw_os_error() == Some(32) && attempt < 99 => {
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => panic!(
                "{} cleanup failed after {} attempts: {error}",
                root.display(),
                attempt + 1
            ),
        }
    }
    panic!("bounded fixture cleanup exhausted without a result");
}

pub(super) fn spawn(root: &Path, provider: SocketAddr) -> ManagedChild {
    spawn_with(root, provider, &[])
}

/// End through terminal control EOF after the witness. Terminal control has no
/// serve-window fallback; scenarios needing a watchdog use `Scenario`.
/// Child::wait closes its private stdin before collecting output.
pub(super) fn spawn_controlled(root: &Path, provider: SocketAddr) -> ManagedChild {
    spawn_controlled_with(root, provider, &[])
}

/// The same private control lifetime with configuration scoped to this child.
pub(super) fn spawn_controlled_with(
    root: &Path,
    provider: SocketAddr,
    environment: &[(&str, &str)],
) -> ManagedChild {
    spawn_service(
        root,
        provider,
        environment,
        "serve-terminal",
        Stdio::piped(),
    )
}

/// Keep a fixture hold pipe separate from the bounded service lifetime.
#[cfg(all(unix, feature = "handover-crash-fixture"))]
pub(super) fn spawn_held_with(
    root: &Path,
    provider: SocketAddr,
    environment: &[(&str, &str)],
) -> ManagedChild {
    spawn_service(root, provider, environment, "serve", Stdio::piped())
}

/// Spawn the product with extra environment, for a witness that needs the
/// composition configured differently. The variables are set on THIS child
/// rather than on the test process, so tests running in parallel do not inherit
/// each other's configuration. Unless the witness configures them itself, the
/// catalog environment roots are pinned to per-fixture directories: the
/// product folds skill sources from the global config directory and from the
/// home's `.claude` and `.agents` directories, so a journey asserting a
/// request shape must not observe the machine's own catalogs.
pub(super) fn spawn_with(
    root: &Path,
    provider: SocketAddr,
    environment: &[(&str, &str)],
) -> ManagedChild {
    spawn_service(root, provider, environment, "serve", Stdio::inherit())
}

/// Owned private single-link copy (`nlink == 1`) of Cargo's hard-linked test
/// binary, dropped with its owning fixture so default-on Linux `bwrap`
/// containment accepts the executable without leaking temp copies on exit.
pub(super) struct InstalledProductBinary {
    _dir: tempfile::TempDir,
    path: PathBuf,
}

impl InstalledProductBinary {
    pub(super) fn path(&self) -> &Path {
        &self.path
    }
}

pub(super) fn installed_product_binary() -> InstalledProductBinary {
    let source = shipped_cli::product_binary();
    let dir = tempfile::Builder::new()
        .prefix("grapt-installed-bin-")
        .tempdir()
        .unwrap_or_else(|error| panic!("installed binary tempdir failed: {error}"));
    let path = dir.path().join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    fs::copy(&source, &path)
        .unwrap_or_else(|error| panic!("installed binary copy failed: {error}"));
    InstalledProductBinary { _dir: dir, path }
}

fn spawn_service(
    root: &Path,
    provider: SocketAddr,
    environment: &[(&str, &str)],
    operation: &str,
    stdin: Stdio,
) -> ManagedChild {
    let mut environment: Vec<(&str, &str)> = environment.to_vec();
    let home = root.join("home");
    let config = root.join("global-config");
    if !environment
        .iter()
        .any(|(name, _)| *name == "GRAPT_TEST_HOME")
    {
        fs::create_dir_all(&home)
            .unwrap_or_else(|error| panic!("fixture home creation failed: {error}"));
        environment.push((
            "GRAPT_TEST_HOME",
            home.to_str()
                .unwrap_or_else(|| panic!("fixture home path is not UTF-8")),
        ));
    }
    if !environment
        .iter()
        .any(|(name, _)| *name == "GRAPT_CONFIG_DIR")
    {
        fs::create_dir_all(&config)
            .unwrap_or_else(|error| panic!("fixture config creation failed: {error}"));
        environment.push((
            "GRAPT_CONFIG_DIR",
            config
                .to_str()
                .unwrap_or_else(|| panic!("fixture config path is not UTF-8")),
        ));
    }
    let installed_binary = installed_product_binary();
    let mut command = grapt_service::launcher_environment::command(installed_binary.path());
    command.env("GRAPT_TEST_CATALOG_ROOT", root);
    for (name, value) in &environment {
        command.env(name, value);
    }
    command.stdin(stdin);
    command
        .arg(operation)
        .arg("--service")
        .arg(root)
        .arg("--port")
        .arg("0")
        .env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{provider}"));
    // Valid service launches scrub after fixture overrides, just as the launcher does.
    grapt_service::launcher_environment::scrub(&mut command);
    let mut managed = spawn_captured(root, &mut command);
    managed._installed_binary = Some(installed_binary);
    managed
}

pub(super) fn spawn_captured(root: &Path, command: &mut Command) -> ManagedChild {
    let output = capture_output(root, command);
    ManagedChild {
        output,
        diagnosing: false,
        child: Some(
            command
                .spawn()
                .unwrap_or_else(|error| panic!("product spawn failed: {error}")),
        ),
        _installed_binary: None,
    }
}

struct CapturedOutput {
    stdout: PathBuf,
    stderr: PathBuf,
}

fn capture_output(root: &Path, command: &mut Command) -> CapturedOutput {
    // Restarts and an overlapping rejected launch retain separate diagnostics.
    let (generation, stdout, stdout_file) = (0..64)
        .find_map(|generation| {
            let path = root.join(format!("product-stdout-{generation}.log"));
            match fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => Some((generation, path, file)),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => None,
                Err(error) => panic!("{} capture failed: {error}", path.display()),
            }
        })
        .unwrap_or_else(|| panic!("fixture exceeded 64 captured launches"));
    let stderr = root.join(format!("product-stderr-{generation}.log"));
    // Regular files cannot fill an undrained pipe and stall the service.
    let open = |path: &Path| {
        fs::File::create(path)
            .unwrap_or_else(|error| panic!("{} capture failed: {error}", path.display()))
    };
    command
        .stdout(Stdio::from(stdout_file))
        .stderr(Stdio::from(open(&stderr)));
    CapturedOutput { stdout, stderr }
}

fn output_prefix(path: &Path, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    fs::File::open(path)?.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

pub(super) fn await_ready(root: &Path, child: &mut ManagedChild) -> SocketAddr {
    // Startup scheduling and cold filesystem work are separate from every
    // provider/operation deadline exercised below. Two governed carriers can
    // consume the old five-second fixture allowance before readiness exists.
    // This remains a finite startup allowance, not a production latency claim.
    let wait = PollDeadline::within(Duration::from_secs(30));
    if let Some(address) = wait.poll(Duration::from_millis(10), || {
        if let Ok(bytes) = fs::read(root.join("serve-ready.json")) {
            if let Ok(value) = serde_json::from_slice::<serde_json::Value>(&bytes) {
                if let Some(address) = value
                    .get("observed_endpoint")
                    .and_then(serde_json::Value::as_str)
                    .and_then(|raw| raw.strip_prefix("http://"))
                    .and_then(|raw| raw.parse().ok())
                {
                    return Some(address);
                }
            }
        }
        match child.try_wait() {
            Ok(None) => {}
            Ok(Some(status)) => panic!("product exited before readiness: {status}"),
            Err(error) => panic!("product status failed: {error}"),
        }
        None
    }) {
        return address;
    }
    #[cfg(target_os = "linux")]
    diagnose_threads(child);
    child
        .kill()
        .unwrap_or_else(|error| panic!("unready product kill failed: {error}"));
    child
        .wait()
        .unwrap_or_else(|error| panic!("unready product reap failed: {error}"));
    let stdout = child
        .stdout_prefix(65536)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let stderr = output_prefix(&child.output.stderr, 65536)
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    panic!("product published no bounded readiness receipt ({wait}); stdout={stdout:?}; stderr={stderr:?}")
}

/// Own the child even on an assertion panic. Bounded serve has its own lease;
/// terminal-controlled children need an explicit scenario watchdog. This Drop
/// deliberately exercises process loss rather than graceful completion.
pub(super) struct ManagedChild {
    child: Option<Child>,
    output: CapturedOutput,
    diagnosing: bool,
    _installed_binary: Option<InstalledProductBinary>,
}

impl std::ops::Deref for ManagedChild {
    type Target = Child;
    fn deref(&self) -> &Child {
        match &self.child {
            Some(child) => child,
            None => panic!("child already consumed"),
        }
    }
}
impl std::ops::DerefMut for ManagedChild {
    fn deref_mut(&mut self) -> &mut Child {
        match &mut self.child {
            Some(child) => child,
            None => panic!("child already consumed"),
        }
    }
}
impl ManagedChild {
    /// Preserve the existing panic capture when a worker owns the process.
    pub(super) fn preserve_failure_diagnostics(&mut self) {
        self.diagnosing = true;
    }

    pub(super) fn stop_after_witness(&mut self, root: &Path) {
        assert!(
            self.try_wait().is_ok_and(|status| status.is_none()),
            "service expired before the journey completed"
        );
        self.kill()
            .unwrap_or_else(|error| panic!("witness stop failed: {error}"));
        self.wait()
            .unwrap_or_else(|error| panic!("witness reap failed: {error}"));
        // Direct binary tests perform only the supervisor's stale-registration
        // cleanup. The durable database is never changed by fixture cleanup.
        for name in [
            "registration",
            "registration.elected",
            "registration.lock",
            "serve-ready.json",
        ] {
            let path = root.join(name);
            match fs::remove_file(&path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => panic!("stale {name} cleanup failed: {error}"),
            }
        }
    }
    pub(super) fn stdout_prefix(&self, limit: u64) -> std::io::Result<Vec<u8>> {
        output_prefix(&self.output.stdout, limit)
    }

    pub(super) fn wait_with_output(mut self) -> std::io::Result<std::process::Output> {
        match self.child.take() {
            Some(mut child) => Ok(std::process::Output {
                status: child.wait()?,
                stdout: self.stdout_prefix(65536)?,
                stderr: output_prefix(&self.output.stderr, 65536)?,
            }),
            None => panic!("child already consumed"),
        }
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Some(child) = &mut self.child {
            let diagnosing = self.diagnosing || std::thread::panicking();
            let prior_status = child.try_wait();
            #[cfg(target_os = "linux")]
            if diagnosing {
                diagnose_threads(child);
            }
            let _ = child.kill();
            let reaped = child.wait();
            if diagnosing {
                eprintln!("owned product failure: before fixture cleanup={prior_status:?}; reaped={reaped:?}");
                if let Some(root) = self.output.stdout.parent() {
                    eprintln!("{}", catalog_boundary::diagnostics(root));
                }
                for (name, path) in [
                    ("stdout", &self.output.stdout),
                    ("stderr", &self.output.stderr),
                ] {
                    let result = output_prefix(path, 4096);
                    let read = result.as_ref().map(Vec::len);
                    let bytes = result.as_deref().unwrap_or_default();
                    eprintln!(
                        "owned product {name}: {read:?}: {}",
                        String::from_utf8_lossy(bytes)
                    );
                }
            }
        }
    }
}

/// Collection closes the private control pipe without waiting for the numeric
/// window, which only governs bounded serve, not terminal control.
#[test]
fn controlled_collection_drains_on_eof_without_waiting_for_the_window() {
    let _journey = process_journey_custody();
    let root = root("controlled-fixture-drain");
    prepare(&root, 120_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut child);
    assert_eq!(
        json(address, "GET", "/api/session/active", "")["data"],
        serde_json::json!({})
    );
    let ready: serde_json::Value = serde_json::from_slice(
        &fs::read(root.join("serve-ready.json"))
            .unwrap_or_else(|error| panic!("controlled readiness missing: {error}")),
    )
    .unwrap_or_else(|error| panic!("controlled readiness invalid: {error}"));
    assert_eq!(ready["serve_window_ms"], 120_000);
    let started = tokio::time::Instant::now();
    let output = child
        .wait_with_output()
        .unwrap_or_else(|error| panic!("controlled output failed: {error}"));
    assert!(
        started.elapsed() < Duration::from_secs(30),
        "control EOF waited for the bounded-serve window"
    );
    assert!(
        output.status.success(),
        "controlled service refused: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)
        .unwrap_or_else(|error| panic!("controlled exit receipt invalid: {error}"));
    assert_eq!(receipt["registration_released"], true);
    assert!(!root.join("registration").exists());
    assert!(!root.join("serve-ready.json").exists());
    cleanup(&root);
}

/// The same capture path must survive more than a pipe's capacity before an
/// HTTP reply. The subprocess is this test binary, not a model or product seam.
#[test]
fn logs_beyond_pipe_capacity_do_not_block_active_response() {
    const PROBE_ROOT: &str = "GRAPT_FIXTURE_OUTPUT_PROBE_ROOT";
    if let Some(root) = std::env::var_os(PROBE_ROOT) {
        logging_probe(&PathBuf::from(root));
        return;
    }
    let root = root("fixture-output");
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("probe root failed: {error}"));
    let binary =
        std::env::current_exe().unwrap_or_else(|error| panic!("test executable absent: {error}"));
    let mut command = Command::new(binary);
    command
        .arg("--exact")
        .arg("process_fixture::logs_beyond_pipe_capacity_do_not_block_active_response")
        .arg("--nocapture")
        .env(PROBE_ROOT, &root);
    let mut child = spawn_captured(&root, &mut command);
    drop(command);
    let address = await_ready(&root, &mut child);
    assert_eq!(
        json(address, "GET", "/api/session/active", "")["data"],
        serde_json::json!({})
    );
    let mut exited = false;
    let wait = PollDeadline::new();
    while wait.pending() {
        match child.try_wait() {
            Ok(Some(status)) => {
                assert!(status.success(), "log probe failed: {status}");
                exited = true;
                break;
            }
            Ok(None) => thread::sleep(Duration::from_millis(10)),
            Err(error) => panic!("log probe status failed: {error}"),
        }
    }
    assert!(
        exited,
        "log probe did not exit before its deadline ({wait})"
    );
    for path in [&child.output.stdout, &child.output.stderr] {
        let size = fs::metadata(path)
            .unwrap_or_else(|error| panic!("probe log absent: {error}"))
            .len();
        assert!(
            size >= 128 * 1024,
            "probe did not exceed pipe capacity: {size}"
        );
        assert_eq!(
            output_prefix(path, 4096)
                .unwrap_or_else(|error| panic!("bounded diagnostic read failed: {error}"))
                .len(),
            4096
        );
    }
    drop(child);
    cleanup(&root);
}

fn logging_probe(root: &Path) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .unwrap_or_else(|error| panic!("log probe listen failed: {error}"));
    let address = listener
        .local_addr()
        .unwrap_or_else(|error| panic!("probe address absent: {error}"));
    fs::write(
        root.join("serve-ready.json"),
        format!(r#"{{"observed_endpoint":"http://{address}"}}"#),
    )
    .unwrap_or_else(|error| panic!("log probe ready write failed: {error}"));
    let noise = [b'L'; 4096];
    for _ in 0..32 {
        std::io::stdout()
            .write_all(&noise)
            .unwrap_or_else(|error| panic!("probe stdout failed: {error}"));
        std::io::stderr()
            .write_all(&noise)
            .unwrap_or_else(|error| panic!("probe stderr failed: {error}"));
    }
    listener
        .set_nonblocking(true)
        .unwrap_or_else(|error| panic!("probe listener bound failed: {error}"));
    let wait = PollDeadline::new();
    while wait.pending() {
        match listener.accept() {
            Ok((mut stream, _)) => {
                stream
                    .set_read_timeout(Some(Duration::from_secs(2)))
                    .unwrap_or_else(|error| panic!("probe read bound failed: {error}"));
                stream
                    .set_write_timeout(Some(Duration::from_secs(2)))
                    .unwrap_or_else(|error| panic!("probe write bound failed: {error}"));
                let mut request = [0u8; 4096];
                let count = stream
                    .read(&mut request)
                    .unwrap_or_else(|error| panic!("probe request failed: {error}"));
                assert!(String::from_utf8_lossy(&request[..count])
                    .starts_with("GET /api/session/active HTTP/1.1"));
                stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\n{\"data\":{}}")
                    .unwrap_or_else(|error| panic!("probe response failed: {error}"));
                return;
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(10))
            }
            Err(error) => panic!("probe accept failed: {error}"),
        }
    }
    panic!("log probe received no request before its deadline ({wait})");
}

// Observe only the fixture-owned child before cleanup destroys its wait state.
// Kernel wait names and syscall numbers carry no request bodies or credentials.
#[cfg(target_os = "linux")]
fn diagnose_threads(child: &Child) {
    let mut pressure = Vec::new();
    let observed = fs::File::open("/proc/pressure/io")
        .and_then(|file| file.take(512).read_to_end(&mut pressure));
    eprintln!(
        "owned fixture IO pressure: {observed:?}: {}",
        String::from_utf8_lossy(&pressure)
    );
    let descriptors = PathBuf::from(format!("/proc/{}/fd", child.id()));
    match fs::read_dir(descriptors) {
        Ok(entries) => {
            let mut sockets = 0usize;
            let mut unreadable = 0usize;
            let mut observed = 0usize;
            for entry in entries.take(256) {
                observed += 1;
                match entry.and_then(|entry| fs::read_link(entry.path())) {
                    Ok(target) if target.to_string_lossy().starts_with("socket:[") => sockets += 1,
                    Ok(_) => {}
                    Err(_) => unreadable += 1,
                }
            }
            eprintln!("owned product descriptor observation: entries={observed} sockets={sockets} unreadable={unreadable} limit=256");
        }
        Err(error) => eprintln!("owned product descriptors unavailable: {error}"),
    }
    let root = PathBuf::from(format!("/proc/{}/task", child.id()));
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) => {
            eprintln!("owned product thread observation unavailable: {error}");
            return;
        }
    };
    let mut observations = std::collections::BTreeMap::<String, usize>::new();
    for entry in entries.take(64) {
        let observation = match entry {
            Ok(entry) => {
                let read = |name: &str| -> String {
                    let mut bytes = Vec::new();
                    match fs::File::open(entry.path().join(name))
                        .and_then(|file| file.take(256).read_to_end(&mut bytes))
                    {
                        Ok(_) => String::from_utf8_lossy(&bytes).trim().to_owned(),
                        Err(error) => format!("unavailable:{error}"),
                    }
                };
                let syscall = read("syscall");
                format!(
                    "wait={} syscall={}",
                    read("wchan"),
                    syscall.split_whitespace().next().unwrap_or("absent")
                )
            }
            Err(error) => format!("thread entry unavailable:{error}"),
        };
        *observations.entry(observation).or_default() += 1;
    }
    eprintln!("owned product thread observations (at most 64): {observations:?}");
    eprintln!(
        "owned product children: {}",
        format_children(&observe_children(child.id()))
    );
}

/// One live child of the product, as /proc shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ChildObservation {
    pub pid: u32,
    /// The command name, from /proc/<pid>/stat, or why it could not be read.
    pub comm: Result<String, String>,
    /// The process state letter (R, S, D, Z, ...).
    pub state: String,
    pub wchan: String,
    /// The first bytes of its command line.
    pub cmdline: String,
    /// Each task's kernel stack, or why it could not be read.
    pub stacks: Vec<String>,
}

/// What one look at the product's children saw, and how much of it: a
/// listing that could not be read, or was cut short, says so rather than
/// reading as "no children" (review SB-H1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct ChildrenObservation {
    pub children: Vec<ChildObservation>,
    /// Why the product's task list could not be read at all.
    pub unavailable: Option<String>,
    /// Each task entry or child list that could not be read.
    pub skipped: Vec<String>,
    /// The bound that cut the listing short.
    pub truncated: Option<&'static str>,
}

/// Where an observation reads /proc: the live filesystem, or a test's
/// stand-in that fails where a real one can.
pub(super) trait ProcSource {
    /// At most `limit + 1` of a directory's entries, each a path or why it
    /// could not be read: the one past the bound only says there are more.
    fn entries(&self, dir: &Path, limit: usize) -> Result<Vec<Result<PathBuf, String>>, String>;
    /// At most `limit` bytes of a file, trimmed.
    fn read(&self, path: &Path, limit: u64) -> Result<String, String>;
}

#[cfg(target_os = "linux")]
struct LiveProc;

#[cfg(target_os = "linux")]
impl ProcSource for LiveProc {
    fn entries(&self, dir: &Path, limit: usize) -> Result<Vec<Result<PathBuf, String>>, String> {
        let entries = fs::read_dir(dir).map_err(|error| error.to_string())?;
        Ok(entries
            .take(limit + 1)
            .map(|entry| {
                entry
                    .map(|entry| entry.path())
                    .map_err(|error| error.to_string())
            })
            .collect())
    }

    fn read(&self, path: &Path, limit: u64) -> Result<String, String> {
        let mut bytes = Vec::new();
        fs::File::open(path)
            .and_then(|file| file.take(limit).read_to_end(&mut bytes))
            .map(|_| String::from_utf8_lossy(&bytes).trim().to_owned())
            .map_err(|error| error.to_string())
    }
}

#[cfg(target_os = "linux")]
fn observe_children(pid: u32) -> ChildrenObservation {
    observe_children_in(&LiveProc, pid)
}

/// The product's direct children: whether a tool's shell is among them, and
/// where each one waits. Bounded: 64 tasks, 16
/// children, 8 tasks each, 64 bytes of command line, 512 bytes of each stack.
pub(super) fn observe_children_in(proc: &dyn ProcSource, pid: u32) -> ChildrenObservation {
    let unavailable = |error: String| format!("unavailable:{error}");
    let mut seen = ChildrenObservation::default();
    let tasks = match proc.entries(&PathBuf::from(format!("/proc/{pid}/task")), 64) {
        Ok(tasks) => tasks,
        Err(error) => {
            seen.unavailable = Some(error);
            return seen;
        }
    };
    if tasks.len() > 64 {
        seen.truncated = Some("64 tasks");
    }
    for task in tasks.into_iter().take(64) {
        let list = match task.and_then(|task| proc.read(&task.join("children"), 4096)) {
            Ok(list) => list,
            Err(error) => {
                seen.skipped.push(error);
                continue;
            }
        };
        for child in list
            .split_whitespace()
            .filter_map(|id| id.parse::<u32>().ok())
        {
            if seen.children.len() >= 16 {
                seen.truncated = Some("16 children");
                return seen;
            }
            let base = PathBuf::from(format!("/proc/{child}"));
            let stat = proc.read(&base.join("stat"), 512);
            // "pid (comm) state ...": comm may hold spaces, so split at ')'.
            let parsed = stat.and_then(|stat| {
                stat.split_once(" (")
                    .and_then(|(_, rest)| rest.rsplit_once(") "))
                    .map(|(comm, rest)| (comm.to_owned(), rest.to_owned()))
                    .ok_or_else(|| format!("unparsed stat {stat:?}"))
            });
            let stacks = match proc.entries(&base.join("task"), 8) {
                Ok(tasks) => {
                    let more = tasks.len() > 8;
                    let mut stacks: Vec<String> = tasks
                        .into_iter()
                        .take(8)
                        .map(|task| {
                            task.and_then(|task| proc.read(&task.join("stack"), 512))
                                .unwrap_or_else(unavailable)
                        })
                        .collect();
                    if more {
                        stacks.push("truncated at 8 tasks".to_owned());
                    }
                    stacks
                }
                Err(error) => vec![unavailable(error)],
            };
            seen.children.push(ChildObservation {
                pid: child,
                state: parsed
                    .as_ref()
                    .ok()
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .unwrap_or("?")
                    .to_owned(),
                comm: parsed.map(|(comm, _)| comm),
                wchan: proc
                    .read(&base.join("wchan"), 128)
                    .unwrap_or_else(unavailable),
                cmdline: proc
                    .read(&base.join("cmdline"), 64)
                    .map(|line| line.replace('\0', " "))
                    .unwrap_or_else(unavailable),
                stacks,
            });
        }
    }
    seen
}

/// How complete the look was, then whether a shell was observed, then one
/// line per child. A shell is "no" only when every child was seen in full.
pub(super) fn format_children(seen: &ChildrenObservation) -> String {
    if let Some(error) = &seen.unavailable {
        return format!("children unavailable ({error}); shell observed: unknown");
    }
    let mut status = Vec::new();
    if !seen.skipped.is_empty() {
        status.push(format!(
            "partial: {} task list(s) unreadable, first {}",
            seen.skipped.len(),
            seen.skipped[0]
        ));
    }
    if let Some(bound) = seen.truncated {
        status.push(format!("truncated at {bound}"));
    }
    let shell = seen
        .children
        .iter()
        .any(|child| matches!(child.comm.as_deref(), Ok("sh" | "bash" | "dash" | "zsh")));
    let whole = status.is_empty() && seen.children.iter().all(|child| child.comm.is_ok());
    let mut text = format!(
        "{} live ({}); shell observed: {}",
        seen.children.len(),
        if status.is_empty() {
            "complete".to_owned()
        } else {
            status.join("; ")
        },
        match (shell, whole) {
            (true, _) => "yes",
            (false, true) => "no",
            (false, false) => "unknown",
        }
    );
    for child in &seen.children {
        let comm = match &child.comm {
            Ok(comm) => comm.clone(),
            Err(error) => format!("unavailable:{error}"),
        };
        text.push_str(&format!(
            "\n  pid={} comm={comm} state={} wchan={} cmdline={:?} stacks={:?}",
            child.pid, child.state, child.wchan, child.cmdline, child.stacks
        ));
    }
    text
}

/// A /proc stand-in: the files and directories a test names, and the
/// failures it plants.
#[derive(Default)]
struct FakeProc {
    dirs: std::collections::BTreeMap<PathBuf, Result<Vec<Result<PathBuf, String>>, String>>,
    files: std::collections::BTreeMap<PathBuf, Result<String, String>>,
}

impl ProcSource for FakeProc {
    fn entries(&self, dir: &Path, limit: usize) -> Result<Vec<Result<PathBuf, String>>, String> {
        let entries = self
            .dirs
            .get(dir)
            .cloned()
            .unwrap_or_else(|| Err("No such file or directory".into()))?;
        Ok(entries.into_iter().take(limit + 1).collect())
    }

    fn read(&self, path: &Path, _limit: u64) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .unwrap_or_else(|| Err("No such file or directory".into()))
    }
}

impl FakeProc {
    /// Product 100 with `tasks`, and a child 4242 that is a waiting bash.
    fn product(tasks: Vec<Result<PathBuf, String>>) -> Self {
        let mut proc = Self::default();
        proc.dirs.insert("/proc/100/task".into(), Ok(tasks));
        proc.files
            .insert("/proc/100/task/100/children".into(), Ok("4242 ".into()));
        proc.files.insert(
            "/proc/4242/stat".into(),
            Ok("4242 (bash) S 100 4242".into()),
        );
        proc.files
            .insert("/proc/4242/wchan".into(), Ok("pipe_read".into()));
        proc.files.insert(
            "/proc/4242/cmdline".into(),
            Ok("bash\0-c\0echo preset-ok".into()),
        );
        proc.dirs.insert(
            "/proc/4242/task".into(),
            Ok(vec![Ok("/proc/4242/task/4242".into())]),
        );
        proc.files.insert(
            "/proc/4242/task/4242/stack".into(),
            Ok("[<0>] pipe_read+0x2f0/0x420".into()),
        );
        proc
    }
}

/// The settle dump names every child, where it waits,
/// and whether a shell was observed; a complete look with no children says
/// so as complete.
#[test]
fn the_children_dump_names_each_child_and_whether_a_shell_ran() {
    let proc = FakeProc::product(vec![Ok("/proc/100/task/100".into())]);
    let text = format_children(&observe_children_in(&proc, 100));
    assert!(
        text.starts_with("1 live (complete); shell observed: yes"),
        "{text}"
    );
    for field in [
        "pid=4242",
        "comm=bash",
        "state=S",
        "wchan=pipe_read",
        "bash -c echo preset-ok",
        "pipe_read+0x2f0",
    ] {
        assert!(text.contains(field), "{field} missing: {text}");
    }
    let mut idle = FakeProc::product(vec![Ok("/proc/100/task/100".into())]);
    idle.files
        .insert("/proc/100/task/100/children".into(), Ok(String::new()));
    let text = format_children(&observe_children_in(&idle, 100));
    assert_eq!(text, "0 live (complete); shell observed: no");
}

/// Review SB-H1: an unreadable task list is no observation, never "no
/// children, no shell".
#[test]
fn an_unreadable_task_list_reads_as_unavailable_not_as_no_children() {
    let mut proc = FakeProc::default();
    proc.dirs.insert(
        "/proc/100/task".into(),
        Err("Permission denied (os error 13)".into()),
    );
    let text = format_children(&observe_children_in(&proc, 100));
    assert_eq!(
        text,
        "children unavailable (Permission denied (os error 13)); shell observed: unknown"
    );
}

/// Review SB-H1: a listing that fails partway, or is cut at its bound,
/// says how much it missed, and a shell it did not see is unknown.
#[test]
fn a_partial_or_truncated_listing_says_so_and_leaves_the_shell_unknown() {
    // A task entry that failed mid-list, and a task whose child list is
    // unreadable: the child that was read still shows, and so does a shell.
    let mut proc = FakeProc::product(vec![
        Ok("/proc/100/task/100".into()),
        Err("Permission denied (os error 13)".into()),
        Ok("/proc/100/task/101".into()),
    ]);
    let text = format_children(&observe_children_in(&proc, 100));
    assert!(
        text.starts_with(
            "1 live (partial: 2 task list(s) unreadable, first Permission denied (os error 13)); shell observed: yes"
        ),
        "{text}"
    );
    // With no shell among what was read, a partial look cannot say "no".
    proc.files.insert(
        "/proc/4242/stat".into(),
        Ok("4242 (sleep) S 100 4242".into()),
    );
    let text = format_children(&observe_children_in(&proc, 100));
    assert!(text.contains("); shell observed: unknown"), "{text}");
    // Nor can a child whose command name was unreadable.
    let mut gone = FakeProc::product(vec![Ok("/proc/100/task/100".into())]);
    gone.files.remove(&PathBuf::from("/proc/4242/stat"));
    let text = format_children(&observe_children_in(&gone, 100));
    assert!(
        text.starts_with("1 live (complete); shell observed: unknown"),
        "{text}"
    );
    assert!(text.contains("comm=unavailable:No such file"), "{text}");
    // A child with more tasks than the stack bound says so.
    let mut threads = FakeProc::product(vec![Ok("/proc/100/task/100".into())]);
    let tasks: Vec<_> = (0..12)
        .map(|task| Ok(PathBuf::from(format!("/proc/4242/task/{task}"))))
        .collect();
    threads.dirs.insert("/proc/4242/task".into(), Ok(tasks));
    let seen = observe_children_in(&threads, 100);
    assert_eq!(seen.children[0].stacks.len(), 9, "{seen:?}");
    assert_eq!(
        seen.children[0].stacks.last().map(String::as_str),
        Some("truncated at 8 tasks")
    );
    // Past the child bound the listing stops and says where.
    let mut many = FakeProc::product(vec![Ok("/proc/100/task/100".into())]);
    let ids: Vec<String> = (5000..5020).map(|id| id.to_string()).collect();
    many.files
        .insert("/proc/100/task/100/children".into(), Ok(ids.join(" ")));
    let text = format_children(&observe_children_in(&many, 100));
    assert!(
        text.starts_with("16 live (truncated at 16 children); shell observed: unknown"),
        "{text}"
    );
}

/// Review SB-H2: the live adapter stops reading a directory one entry past
/// its bound, however large the directory, so a truncated listing is still
/// known to be truncated.
#[cfg(target_os = "linux")]
#[test]
fn the_live_listing_reads_at_most_one_entry_past_its_bound() {
    let dir = std::env::temp_dir().join(format!("grapt-proc-bound-{}", std::process::id()));
    let _ = fs::remove_dir_all(&dir);
    fs::create_dir_all(&dir).unwrap_or_else(|error| panic!("scratch failed: {error}"));
    for index in 0..128 {
        fs::write(dir.join(index.to_string()), b"")
            .unwrap_or_else(|error| panic!("scratch entry failed: {error}"));
    }
    let listed = LiveProc.entries(&dir, 64);
    let few = LiveProc.entries(&dir, 200);
    let _ = fs::remove_dir_all(&dir);
    let listed = listed.unwrap_or_else(|error| panic!("listing failed: {error}"));
    assert_eq!(listed.len(), 65, "{listed:?}");
    assert!(listed.iter().all(Result::is_ok), "{listed:?}");
    assert_eq!(few.map(|entries| entries.len()), Ok(128));
}

/// The observation reads real /proc data: a child this test spawned is found
/// with its command name, a state and its command line.
#[cfg(target_os = "linux")]
#[test]
fn observed_children_include_a_spawned_child() {
    let mut child = Command::new("sleep")
        .arg(CHILD_LIFETIME_SECS.to_string())
        .spawn()
        .unwrap_or_else(|error| panic!("sleep could not start: {error}"));
    let observed = observe_spawned_child(&child);
    let _ = child.kill();
    let _ = child.wait();
    let observed = observed.unwrap_or_else(|error| panic!("sleep readiness: {error}"));
    assert_eq!(observed.unavailable, None, "{observed:?}");
    let found = observed
        .children
        .iter()
        .find(|observation| observation.pid == child.id())
        .unwrap_or_else(|| panic!("the spawned child was not observed: {observed:?}"));
    assert_eq!(found.comm.as_deref(), Ok("sleep"));
    assert!(!found.state.is_empty() && found.state != "?", "{found:?}");
    assert!(found.cmdline.starts_with("sleep 5"), "{found:?}");
}
