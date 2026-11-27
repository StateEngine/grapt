//! Own the service, held provider request and durable reopen for restart tests.
use super::*;
use grapt_session::execution::{InterruptionReason, Outcome};

pub(super) enum Lifetime {
    #[cfg(unix)]
    Service,
    Window,
    Terminal,
}

pub(super) struct Journey {
    child: process_fixture::ManagedChild,
    provider: ProviderFixture,
    address: SocketAddr,
    session: SessionId,
    execution: Option<ExecutionId>,
    entered: Arc<terminal_coordination::Phase>,
    release: Arc<terminal_coordination::Phase>,
    disconnect: Arc<AtomicBool>,
    #[cfg(unix)]
    followup_entered: Arc<terminal_coordination::Phase>,
    followup_release: Arc<terminal_coordination::Phase>,
    root: PathBuf,
    _cleanup: terminal_coordination::RootCleanup,
}

const ANSWER: &str = "The interrupted task has now finished.";

impl Journey {
    pub(super) fn start(case: &str, lifetime: Lifetime) -> Self {
        Self::new(case, lifetime, false)
    }

    #[cfg(unix)]
    pub(super) fn start_with_held_followup(case: &str) -> Self {
        Self::new(case, Lifetime::Service, true)
    }

    fn new(case: &str, lifetime: Lifetime, hold_followup: bool) -> Self {
        let root = root(case);
        prepare(&root, 10_000);
        let entered = Arc::new(terminal_coordination::Phase::new());
        let release = Arc::new(terminal_coordination::Phase::new());
        let disconnect = Arc::new(AtomicBool::new(false));
        let followup_entered = Arc::new(terminal_coordination::Phase::new());
        let followup_release = Arc::new(terminal_coordination::Phase::new());
        let provider = ProviderFixture::start(vec![
            Script::ControlledTerminalOutput {
                bytes: 100,
                entered: Arc::clone(&entered),
                release: Arc::clone(&release),
                disconnect: Arc::clone(&disconnect),
            },
            if hold_followup {
                Script::ControlledTerminalOutput {
                    bytes: 100,
                    entered: Arc::clone(&followup_entered),
                    release: Arc::clone(&followup_release),
                    disconnect: Arc::clone(&disconnect),
                }
            } else {
                Script::SuccessText(ANSWER)
            },
        ]);
        let mut child = match lifetime {
            Lifetime::Window => spawn(&root, provider.address),
            Lifetime::Terminal => process_fixture::spawn_controlled(&root, provider.address),
            #[cfg(unix)]
            Lifetime::Service => {
                let mut command =
                    grapt_service::launcher_environment::command(shipped_cli::product_binary());
                command
                    .args(["serve-service", "--service"])
                    .arg(&root)
                    .args(["--port", "0"])
                    .env(
                        "GRAPT_LIVE_HEADLESS_LOOPBACK",
                        format!("http://{}", provider.address),
                    )
                    .env("GRAPT_TEST_CATALOG_ROOT", &root)
                    .env("GRAPT_TEST_HOME", root.join("home"))
                    .env("GRAPT_CONFIG_DIR", root.join("global-config"));
                for name in ["home", "global-config"] {
                    fs::create_dir_all(root.join(name))
                        .unwrap_or_else(|error| panic!("fixture catalog: {error}"));
                }
                process_fixture::spawn_captured(&root, &mut command)
            }
        };
        let address = await_ready(&root, &mut child);
        let session = stateengine_binding::create_native(address, &root)
            .parse()
            .unwrap_or_else(|error| panic!("fixture Session identity: {error}"));
        let journey = Self {
            child,
            provider,
            address,
            session,
            execution: None,
            entered,
            release,
            disconnect,
            #[cfg(unix)]
            followup_entered,
            followup_release,
            root: root.clone(),
            _cleanup: terminal_coordination::RootCleanup(root),
        };
        bound_session_tools::bind_config(journey.address, &journey.root, journey.session.as_str());
        journey
    }

    pub(super) fn bind(&self) {
        let (status, reply) = request(
            self.address,
            "PUT",
            &format!("/api/session/{}/stateengine/capability", self.session),
            &serde_json::json!({
                "capability":"NATIVE_FIXTURE_CAPABILITY",
                "configPath":self.root.join("private/session.json")
            })
            .to_string(),
        );
        assert_eq!(
            status,
            204,
            "native rebind refused: {}",
            String::from_utf8_lossy(&reply)
        );
    }

    pub(super) fn begin(&mut self) {
        self.prompt("msg_restart_original", "Finish this interrupted task.");
        self.entered
            .wait("original provider request", &self.provider.stop)
            .unwrap_or_else(|error| panic!("{error}"));
        self.observe_active();
    }

    fn observe_active(&mut self) {
        let active = json(self.address, "GET", "/api/session/active", "");
        self.execution = Some(
            ExecutionId::new(
                active["data"][self.session.as_str()]["executionID"]
                    .as_str()
                    .unwrap_or_else(|| panic!("held execution is not active: {active}")),
            )
            .unwrap_or_else(|| panic!("held execution identity is empty")),
        );
    }

    fn prompt(&self, id: &str, text: &str) {
        json(
            self.address,
            "POST",
            &format!("/api/session/{}/prompt", self.session),
            &serde_json::json!({"id":id,"text":text,"delivery":"queue","resume":true}).to_string(),
        );
    }

    pub(super) fn follow_up(&self) {
        self.prompt("msg_restart_followup", "Finish the saved work explicitly.");
    }

    #[cfg(unix)]
    pub(super) fn await_held_followup(&mut self) {
        self.followup_entered
            .wait("followup provider request", &self.provider.stop)
            .unwrap_or_else(|error| panic!("{error}"));
        self.observe_active();
    }

    #[cfg(unix)]
    pub(super) fn stop_service(&mut self) {
        let pid = i32::try_from(self.child.id())
            .unwrap_or_else(|error| panic!("owned service PID: {error}"));
        nix::sys::signal::kill(
            nix::unistd::Pid::from_raw(pid),
            nix::sys::signal::Signal::SIGTERM,
        )
        .unwrap_or_else(|error| panic!("owned service signal: {error}"));
        self.await_exit();
    }

    pub(super) fn close(&mut self) {
        drop(
            self.child
                .stdin
                .take()
                .unwrap_or_else(|| panic!("terminal control pipe is absent")),
        );
        self.await_exit();
    }

    pub(super) fn await_exit(&mut self) {
        let wait = PollDeadline::within(Duration::from_secs(30));
        let status = wait
            .poll(Duration::from_millis(10), || {
                self.child
                    .try_wait()
                    .unwrap_or_else(|error| panic!("service wait: {error}"))
            })
            .unwrap_or_else(|| panic!("owned service did not drain: {wait}"));
        assert!(status.success(), "service stop was not graceful: {status}");
        self.disconnect.store(true, Ordering::SeqCst);
        self.release.release();
        let connection = self.read_connection();
        let execution = self
            .execution
            .as_ref()
            .unwrap_or_else(|| panic!("execution missing"));
        let observations = grapt_session::execution::read(&connection, &self.session, execution)
            .unwrap_or_else(|error| panic!("durable execution terminal: {error}"));
        assert!(
            observations
                .iter()
                .any(|row| row.outcome == Some(Outcome::Interrupted)),
            "service exit did not preserve the original interruption"
        );
    }

    fn read_connection(&self) -> rusqlite::Connection {
        rusqlite::Connection::open_with_flags(
            self.root.join("database.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap_or_else(|error| panic!("fixture history open: {error}"))
    }

    pub(super) fn assert_shutdown_terminal_without_resume_notice(&self) {
        let connection = self.read_connection();
        let execution = self
            .execution
            .as_ref()
            .unwrap_or_else(|| panic!("execution missing"));
        let rows = grapt_session::execution::read(&connection, &self.session, execution)
            .unwrap_or_else(|error| panic!("legacy execution read: {error}"));
        assert!(rows
            .iter()
            .any(|row| row.interruption == Some(InterruptionReason::Shutdown)));
        let resumable: i64 = connection.query_row(
            "SELECT count(*) FROM event WHERE aggregate_family='session' AND aggregate_id=?1
             AND type='session.notice.recorded.1' AND json_extract(data,'$.kind')='interrupted-turn'",
            [self.session.as_str()], |row| row.get(0),
        ).unwrap_or_else(|error| panic!("legacy notice read: {error}"));
        assert_eq!(
            resumable, 0,
            "legacy-shaped history gained resume authority"
        );
    }

    pub(super) fn reopen(&mut self) {
        self.child = process_fixture::spawn_controlled(&self.root, self.provider.address);
        self.address = await_ready(&self.root, &mut self.child);
    }

    pub(super) fn assert_calls_stay(&self, wanted: usize) {
        let wait = PollDeadline::within(Duration::from_secs(2));
        while wait.pending() {
            assert_eq!(
                self.provider.calls.load(Ordering::SeqCst),
                wanted,
                "rebinding revived work without authority for another turn"
            );
            thread::sleep(Duration::from_millis(10));
        }
        permission_residuals::wait_session_idle(self.address, self.session.as_str());
        assert_eq!(self.provider.calls.load(Ordering::SeqCst), wanted);
    }

    pub(super) fn await_answer(&self, calls: usize) {
        wait_calls(&self.provider.calls, calls);
        permission_residuals::wait_session_idle(self.address, self.session.as_str());
        let history = json(
            self.address,
            "GET",
            &format!("/api/session/{}/message", self.session),
            "",
        );
        let rows = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("history rows: {history}"));
        assert!(
            rows.iter().any(|row| row["type"] == "assistant"
                && row["content"].as_array().is_some_and(|parts| parts
                    .iter()
                    .any(|part| part["type"] == "text" && part["text"] == ANSWER))),
            "the resumed turn did not finish: {history}"
        );
        assert_eq!(
            rows.iter()
                .filter(|row| row["type"] == "user" && row["id"] == "msg_restart_original")
                .count(),
            1,
            "continuation duplicated the original user input"
        );
    }

    pub(super) fn assert_stop_notice(&self) {
        let history = json(
            self.address,
            "GET",
            &format!("/api/session/{}/message", self.session),
            "",
        );
        let notices: Vec<_> = history["data"]
            .as_array()
            .unwrap_or_else(|| panic!("history rows: {history}"))
            .iter()
            .filter(|row| row["metadata"]["interruptedTurn"] == true)
            .collect();
        assert_eq!(
            notices.len(),
            1,
            "the stop must appear exactly once: {history}"
        );
        let notice = notices[0];
        assert_eq!(notice["type"], "system");
        let text = notice["text"]
            .as_str()
            .unwrap_or_else(|| panic!("stop notice text is absent"));
        assert!(
            !text.trim().is_empty() && !text.contains(['\n', '\r']),
            "a service stop needs one readable line: {notice}"
        );
        let execution = self
            .execution
            .as_ref()
            .unwrap_or_else(|| panic!("execution missing"));
        assert_eq!(notice["metadata"]["executionID"], execution.as_str());
        assert_eq!(notice["metadata"]["messageID"], "msg_restart_original");
    }
}

impl Drop for Journey {
    fn drop(&mut self) {
        self.disconnect.store(true, Ordering::SeqCst);
        self.release.release();
        self.followup_release.release();
        if let Err(error) = self.provider.settle_terminal_workers() {
            if thread::panicking() {
                eprintln!("restart provider cleanup: {error}");
            } else {
                panic!("restart provider cleanup: {error}");
            }
        }
    }
}
