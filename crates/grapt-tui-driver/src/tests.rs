use super::*;

#[test]
fn names_are_prefixed_and_restricted() {
    assert_eq!(
        session_name("builder", "j01-install").ok().as_deref(),
        Some("grapt-tui-builder-j01-install")
    );
    for (seat, journey) in [
        ("", "j01"),
        ("builder", ""),
        ("Builder", "j01"),
        ("builder", "j01.x"),
        ("builder", "j01:x"),
        ("builder", "-j01"),
        ("builder", "a b"),
        ("builder", "a;"),
    ] {
        assert!(
            session_name(seat, journey).is_err(),
            "{seat:?}/{journey:?} admitted"
        );
    }
    assert!(session_name("builder", &"x".repeat(NAME_PART_MAX + 1)).is_err());
}

#[test]
fn every_generated_name_is_admitted_back() {
    let longest = "j".repeat(NAME_PART_MAX);
    let hyphenated_longest = format!("{}-z", "a".repeat(NAME_PART_MAX - 2));
    for (seat, journey) in [
        ("agt-demo", "j01-install"),
        ("builder", longest.as_str()),
        (longest.as_str(), longest.as_str()),
        (hyphenated_longest.as_str(), hyphenated_longest.as_str()),
        ("b", "j-"),
    ] {
        let name = session_name(seat, journey)
            .unwrap_or_else(|error| panic!("{seat:?}/{journey:?} refused: {error}"));
        assert!(check_name(&name).is_ok(), "{name:?} not admitted back");
    }
}

#[test]
fn foreign_sessions_are_refused() {
    for name in [
        "main",
        "grapt-tui-",
        "grapt-tui-builder",
        "grapt-tui--j01",
        "other-builder-j01",
        "grapt-tui-builder-j01;",
    ] {
        assert!(check_name(name).is_err(), "{name:?} admitted");
    }
    assert!(check_name(&format!("grapt-tui-{}-j", "a".repeat(NAME_PART_MAX + 1))).is_err());
    assert!(check_name("grapt-tui-builder-j01").is_ok());
}

#[test]
fn keys_are_names_or_single_characters() {
    for key in ["Enter", "C-c", "M-Left", "F12", "BSpace"] {
        assert!(key_name(key), "{key:?} refused");
    }
    for key in [
        "-t",
        ";",
        "a;",
        "Enter;",
        "kill-session -t x",
        "",
        "\u{e9}t\u{e9}",
    ] {
        assert!(!key_name(key), "{key:?} admitted as a key name");
    }
}

#[test]
fn program_arguments_are_quoted_whole() {
    assert_eq!(shell_quote("plain"), "'plain'");
    assert_eq!(shell_quote("it's;"), r"'it'\''s;'");
    assert_eq!(shell_quote(""), "''");
}

/// Stub tests run one at a time: a stub written while another test thread
/// forks can be briefly held open by that child, and executing it then
/// fails with "Text file busy".
#[cfg(unix)]
static STUBS: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// A stub tmux program in its own directory, removed on drop.
#[cfg(unix)]
struct Stub {
    directory: PathBuf,
    _serial: std::sync::MutexGuard<'static, ()>,
}

#[cfg(unix)]
impl Stub {
    fn new(case: &str, script: &str) -> Self {
        use std::os::unix::fs::PermissionsExt;
        let serial = STUBS
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let directory =
            std::env::temp_dir().join(format!("grapt-tui-driver-{}-{case}", std::process::id()));
        let _ = std::fs::remove_dir_all(&directory);
        let program = directory.join("tmux");
        std::fs::create_dir(&directory)
            .and_then(|()| std::fs::write(&program, format!("#!/bin/sh\n{script}\n")))
            .and_then(|()| {
                std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o755))
            })
            .unwrap_or_else(|error| panic!("stub tmux: {error}"));
        Self {
            directory,
            _serial: serial,
        }
    }

    fn tmux(&self, call_bound: Duration) -> Tmux {
        Tmux::with_program(self.directory.join("tmux")).call_bound(call_bound)
    }
}

#[cfg(unix)]
impl Drop for Stub {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

#[cfg(unix)]
fn finishes_within<T>(limit: Duration, run: impl FnOnce() -> T) -> T {
    let started = monotonic_now();
    let result = run();
    let taken = monotonic_now() - started;
    assert!(taken < limit, "took {taken:?}, limit {limit:?}");
    result
}

#[cfg(unix)]
fn tmux_detail<T: fmt::Debug>(result: Result<T, DriverError>) -> String {
    match result {
        Err(DriverError::Tmux { detail, .. }) => detail,
        other => panic!("expected a tmux refusal, got {other:?}"),
    }
}

#[cfg(unix)]
#[test]
fn a_stalled_tmux_call_is_cut_at_its_bound() {
    let stub = Stub::new("stall", "exec sleep 30");
    let tmux = stub.tmux(Duration::from_millis(300));
    let detail = finishes_within(Duration::from_secs(3), || {
        tmux_detail(tmux.exists("grapt-tui-test-stall"))
    });
    assert!(detail.contains("did not finish"), "{detail}");
}

#[cfg(unix)]
#[test]
fn a_flooding_tmux_call_is_cut_at_its_output_cap() {
    let stub = Stub::new("flood", "exec yes flood");
    let tmux = stub.tmux(Duration::from_secs(10));
    let detail = finishes_within(Duration::from_secs(5), || {
        tmux_detail(tmux.exists("grapt-tui-test-flood"))
    });
    assert!(detail.contains("printed more than"), "{detail}");
}

#[cfg(unix)]
#[test]
fn a_wait_ends_by_its_own_deadline_even_when_tmux_stalls() {
    // has-session answers; every capture stalls well past the wait.
    let stub = Stub::new(
        "wait",
        r#"case "$1" in has-session) exit 0 ;; *) exec sleep 30 ;; esac"#,
    );
    let session = stub
        .tmux(Duration::from_secs(10))
        .existing("grapt-tui-test-wait")
        .unwrap_or_else(|error| panic!("{error}"));
    let result = finishes_within(Duration::from_secs(3), || {
        session.wait_for("anything", Duration::from_millis(300))
    });
    assert!(
        matches!(result, Err(DriverError::Timeout { .. })),
        "{result:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_tmux_that_exits_with_its_output_held_open_is_cut_at_its_bound() {
    // tmux exits at once, but a process it started keeps stdout open.
    let stub = Stub::new("held", "sleep 5 &\nexit 0");
    let tmux = stub.tmux(Duration::from_millis(300));
    let detail = finishes_within(Duration::from_secs(3), || {
        tmux_detail(tmux.exists("grapt-tui-test-held"))
    });
    assert!(detail.contains("stayed open"), "{detail}");
}

/// Live pipe-reader threads in this process.
#[cfg(target_os = "linux")]
fn reader_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap_or_else(|error| panic!("thread list: {error}"))
        .filter_map(Result::ok)
        .filter(|task| {
            std::fs::read_to_string(task.path().join("comm"))
                .is_ok_and(|name| name.trim_end() == READER_THREAD)
        })
        .count()
}

#[cfg(target_os = "linux")]
#[test]
fn repeated_held_output_timeouts_leave_no_reader_behind() {
    // Each call's tmux exits at once while a process it started keeps
    // both pipes open far past the test.
    let stub = Stub::new("held-repeat", "sleep 30 &\nexit 0");
    let tmux = stub.tmux(Duration::from_millis(100));
    let baseline = reader_threads();
    for _ in 0..6 {
        let detail = tmux_detail(tmux.exists("grapt-tui-test-held-repeat"));
        assert!(detail.contains("stayed open"), "{detail}");
    }
    // A reader that has sent its output may take a moment to exit.
    let settle = monotonic_now() + CLEANUP_BOUND;
    while reader_threads() > baseline && monotonic_now() < settle {
        thread::sleep(CHILD_POLL);
    }
    assert_eq!(reader_threads(), baseline);
}

#[cfg(unix)]
#[test]
fn a_dead_pane_whose_history_stalls_ends_the_wait_with_its_last_screen() {
    let stub = Stub::new(
        "history",
        r#"case "$*" in
  has-session*) exit 0 ;;
  *"-S -"*) exec sleep 30 ;;
  capture-pane*) echo last-visible-line ; exit 0 ;;
  display-message*) echo "1 3" ; exit 0 ;;
esac
exit 1"#,
    );
    let session = stub
        .tmux(Duration::from_secs(10))
        .existing("grapt-tui-test-history")
        .unwrap_or_else(|error| panic!("{error}"));
    let result = finishes_within(Duration::from_secs(3), || {
        session.wait_for("absent", Duration::from_millis(300))
    });
    match result {
        Err(DriverError::Timeout { screen, .. }) => {
            assert!(screen.contains("last-visible-line"), "{screen}")
        }
        other => panic!("expected a timeout with the last screen, got {other:?}"),
    }
}

#[test]
fn environment_keys_are_shell_names() {
    for key in ["PATH", "_X", "grapt_mode", "A1"] {
        assert!(environment_key(key), "{key:?} refused");
    }
    for key in ["", "1A", "A-B", "A;B", "A B", "A=B"] {
        assert!(!environment_key(key), "{key:?} admitted");
    }
}

#[test]
fn a_launch_with_unusable_cwd_or_environment_is_refused_before_tmux() {
    let missing =
        std::env::temp_dir().join(format!("grapt-tui-driver-missing-{}", std::process::id()));
    let mut bad = Vec::new();
    for directory in [PathBuf::from("relative/dir"), missing] {
        let mut launch = Launch::new(vec!["true".into()]);
        launch.directory = Some(directory);
        bad.push(launch);
    }
    let mut launch = Launch::new(vec!["true".into()]);
    launch.environment.push(("A;B".into(), "x".into()));
    bad.push(launch);
    let mut launch = Launch::new(vec!["true".into()]);
    launch.environment.push(("A".into(), "x\0y".into()));
    bad.push(launch);
    bad.push(Launch::new(vec!["tr\0ue".into()]));
    for launch in bad {
        assert!(
            matches!(
                write_launcher("grapt-tui-test-refused", &launch),
                Err(DriverError::Name(_))
            ),
            "{launch:?} admitted"
        );
    }
}

#[cfg(unix)]
#[test]
fn a_failed_start_leaves_no_launcher_behind() {
    let stub = Stub::new(
        "nostart",
        r#"case "$1" in has-session) exit 1 ;; *) echo refused >&2 ; exit 1 ;; esac"#,
    );
    let name = format!("grapt-tui-test-nostart{}", std::process::id());
    let result = stub
        .tmux(Duration::from_secs(5))
        .start(&name, &Launch::new(vec!["true".into()]));
    assert!(
        matches!(result, Err(DriverError::Tmux { .. })),
        "{result:?}"
    );
    let left: Vec<_> = std::fs::read_dir(std::env::temp_dir())
        .unwrap_or_else(|error| panic!("temp dir: {error}"))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(&name))
        .collect();
    assert!(left.is_empty(), "launcher left behind: {left:?}");
}
