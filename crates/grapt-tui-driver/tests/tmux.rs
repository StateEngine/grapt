//! Real tmux sessions driving small shell programs. These need tmux on PATH
//! and fail, rather than skip, when it is absent.
#![cfg(unix)]

use std::process::Command;
use std::time::Duration;

use grapt_tui_driver::{session_name, DriverError, Launch, Session, Tmux};
// The monotonic scheduling clock the workspace admits; never a durable fact.
use tokio::time::Instant;

const BOUND: Duration = Duration::from_secs(10);

/// A per-process session name on a private per-test socket, both killed on
/// drop so a failed test leaves nothing running. One socket per test,
/// shared by every session of that test: the injection scenarios need the
/// victim and the attacker on one server, while different tests stay
/// isolated on their own sockets.
struct Named {
    name: String,
    socket: String,
}

impl Named {
    fn new(case: &str) -> Self {
        let process = std::process::id();
        let journey = format!("t{process}-{case}");
        let name = match session_name("test", &journey) {
            Ok(name) => name,
            Err(error) => panic!("test session name refused: {error}"),
        };
        // The private socket keeps this test's tmux server to itself. On the
        // shared per-user server, one test killing the last session exits the
        // server under a sibling's in-flight new-session, which fails with
        // "server exited unexpectedly".
        Self {
            name,
            socket: format!("t{process}-{case}"),
        }
    }

    /// Another session of the same test, on the test's socket.
    fn join(case: &str, socket: &str) -> Self {
        let process = std::process::id();
        let journey = format!("t{process}-{case}");
        let name = match session_name("test", &journey) {
            Ok(name) => name,
            Err(error) => panic!("test session name refused: {error}"),
        };
        Self {
            name,
            socket: socket.to_owned(),
        }
    }

    /// This test's tmux: the default program and bound, its own socket.
    fn tmux(&self) -> Tmux {
        Tmux::default()
            .with_socket(&self.socket)
            .unwrap_or_else(|error| panic!("test socket refused: {error}"))
    }
}

impl Drop for Named {
    fn drop(&mut self) {
        let _ = Command::new("tmux")
            .args([
                "-L",
                self.socket.as_str(),
                "kill-session",
                "-t",
                &format!("={}", self.name),
            ])
            .output();
        // A session killed before its shell ran leaves its launcher behind.
        if let Ok(entries) = std::fs::read_dir(std::env::temp_dir()) {
            for entry in entries.flatten() {
                let file = entry.file_name().to_string_lossy().into_owned();
                if file.starts_with(&format!("{}-", self.name)) && file.ends_with(".sh") {
                    let _ = std::fs::remove_file(entry.path());
                }
            }
        }
    }
}

fn shell(script: &str) -> Launch {
    Launch::new(vec!["sh".into(), "-c".into(), script.into()])
}

fn started(name: &Named, launch: &Launch) -> Session {
    name.tmux()
        .start(&name.name, launch)
        .unwrap_or_else(|error| panic!("start failed: {error}"))
}

fn alive(name: &Named) -> bool {
    name.tmux()
        .exists(&name.name)
        .unwrap_or_else(|error| panic!("has-session failed: {error}"))
}

/// The temp-dir launchers whose names start with this prefix.
fn launchers(prefix: &str) -> Vec<std::fs::DirEntry> {
    std::fs::read_dir(std::env::temp_dir())
        .unwrap_or_else(|error| panic!("temp dir: {error}"))
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().starts_with(prefix))
        .collect()
}

#[test]
fn types_keys_and_reads_the_screen_back() {
    let name = Named::new("echo");
    let mut launch = shell(
        r#"printf 'ready %s\n' "$GREETING"; read line; printf 'got:%s\n' "$line"; exec sleep 60"#,
    );
    launch.width = 60;
    launch.height = 10;
    launch
        .environment
        .push(("GREETING".into(), "from-env".into()));
    let session = started(&name, &launch);
    let screen = session
        .wait_for("ready from-env", BOUND)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(screen.lines().count(), 10, "capture is the 10-row screen");
    session
        .send_text("hello world")
        .and_then(|()| session.send_keys(&["Enter"]))
        .unwrap_or_else(|error| panic!("send failed: {error}"));
    session
        .wait_for("got:hello world", BOUND)
        .unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(
        session.exit_status().ok(),
        Some(None),
        "program still running"
    );
    session
        .stop()
        .unwrap_or_else(|error| panic!("stop failed: {error}"));
    assert!(!alive(&name), "session gone after stop");
}

#[test]
fn a_missing_text_times_out_with_the_last_screen() {
    let name = Named::new("timeout");
    let session = started(&name, &shell("echo still-here; exec sleep 60"));
    session
        .wait_for("still-here", BOUND)
        .unwrap_or_else(|error| panic!("{error}"));
    match session.wait_for("never-shown", Duration::from_millis(300)) {
        Err(DriverError::Timeout { text, screen }) => {
            assert_eq!(text, "never-shown");
            assert!(screen.contains("still-here"), "last screen kept: {screen}");
        }
        other => panic!("expected a timeout, got {other:?}"),
    }
}

#[test]
fn an_exited_program_reports_its_status_and_last_screen() {
    let name = Named::new("exit");
    let session = started(&name, &shell("echo bye; exit 3"));
    match session.wait_for("never-shown", BOUND) {
        Err(DriverError::Exited { status, screen, .. }) => {
            assert_eq!(status, 3);
            assert!(screen.contains("bye"), "last screen kept: {screen}");
        }
        other => panic!("expected an exit, got {other:?}"),
    }
}

#[test]
fn a_running_session_name_is_not_reused() {
    let name = Named::new("dup");
    let _first = started(&name, &shell("exec sleep 60"));
    match name.tmux().start(&name.name, &shell("exec sleep 60")) {
        Err(DriverError::Exists(existing)) => assert_eq!(existing, name.name),
        other => panic!("expected a refusal, got {other:?}"),
    }
}

#[test]
fn program_arguments_reach_the_program_literally() {
    let victim = Named::new("argv-victim");
    let _victim_session = started(&victim, &shell("exec sleep 60"));
    let name = Named::join("argv", &victim.socket);
    let exact_victim = format!("={}", victim.name);
    let launch = Launch::new(
        [
            "sh",
            "-c",
            r#"printf '%s|' "$@"; echo; exec sleep 60"#,
            "sh",
            "a;",
            ";",
            "kill-session",
            "-t",
            &exact_victim,
            "it's",
            "#{session_name}",
            "two words",
        ]
        .map(str::to_owned)
        .to_vec(),
    );
    let session = started(&name, &launch);
    let expected = format!("a;|;|kill-session|-t|{exact_victim}|it's|#{{session_name}}|two words|");
    session
        .wait_for(&expected, BOUND)
        .unwrap_or_else(|error| panic!("{error}"));
    assert!(alive(&victim), "program arguments ran as a tmux command");
}

#[test]
fn typed_text_and_keys_never_become_tmux_commands() {
    let victim = Named::new("keys-victim");
    let _victim_session = started(&victim, &shell("exec sleep 60"));
    let name = Named::join("keys", &victim.socket);
    let session = started(&name, &shell("exec cat"));
    let exact_victim = format!("={}", victim.name);
    session
        .send_text(&format!("x; kill-session -t {exact_victim};"))
        .and_then(|()| session.send_keys(&[";", "kill-session", "-", "Enter"]))
        .unwrap_or_else(|error| panic!("send failed: {error}"));
    session
        .wait_for(
            &format!("x; kill-session -t {exact_victim};;kill-session-"),
            BOUND,
        )
        .unwrap_or_else(|error| panic!("{error}"));
    for refused in ["-t", "a;", "Enter;", "kill-session -t x"] {
        assert!(
            matches!(session.send_keys(&[refused]), Err(DriverError::Name(_))),
            "{refused:?} admitted as a key"
        );
    }
    assert!(alive(&victim), "typed text or keys ran as a tmux command");
}

#[test]
fn a_session_is_targeted_by_its_exact_name() {
    let longer = Named::new("exact-longer");
    let _longer_session = started(&longer, &shell("exec cat"));
    let shorter = longer.name.trim_end_matches("-longer").to_owned();
    assert!(
        matches!(
            longer.tmux().existing(&shorter),
            Err(DriverError::Tmux { .. })
        ),
        "a name prefix matched another session"
    );
    let name = Named::join("exact", &longer.socket);
    let session = started(&name, &shell("exec cat"));
    let _ = Command::new("tmux")
        .args([
            "-L",
            name.socket.as_str(),
            "kill-session",
            "-t",
            &format!("={}", name.name),
        ])
        .output();
    assert!(
        session.send_text("stray").is_err(),
        "text went to some other session"
    );
}

#[test]
fn directory_and_environment_reach_the_program_literally() {
    let victim = Named::new("cwd-victim");
    let _victim_session = started(&victim, &shell("exec sleep 60"));
    let directory = std::env::temp_dir().join(format!(
        "grapt-tui-driver-{}-work; it's here",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&directory);
    std::fs::create_dir(&directory).unwrap_or_else(|error| panic!("work dir: {error}"));
    let name = Named::join("cwd", &victim.socket);
    let exact_victim = format!("={}", victim.name);
    let value = format!("a; kill-session -t {exact_victim}; it's \"quoted\"");
    let mut launch = shell(r#"printf 'cwd=%s|env=%s|\n' "$PWD" "$CHECK"; exec sleep 60"#);
    launch.width = 250;
    launch.directory = Some(directory.clone());
    launch.environment.push(("CHECK".into(), value.clone()));
    let session = started(&name, &launch);
    let expected = format!("cwd={}|env={value}|", directory.display());
    let screen = session
        .wait_for("|env=", BOUND)
        .unwrap_or_else(|error| panic!("{error}"));
    let joined: String = screen.lines().map(str::trim_end).collect();
    assert!(
        joined.contains(&expected),
        "expected {expected:?} in {screen:?}"
    );
    assert!(
        alive(&victim),
        "directory or environment ran as a tmux command"
    );
    let deadline = Instant::now() + BOUND;
    let mut leftovers = launchers(&name.name);
    // The victim's launcher is removed by its own shell the moment that
    // shell runs, which on a loaded box can land after this check; a
    // genuinely leaked launcher never disappears, so wait with a bound.
    while !leftovers.is_empty() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
        leftovers = launchers(&name.name);
    }
    assert!(leftovers.is_empty(), "launcher left behind: {leftovers:?}");
    let _ = std::fs::remove_dir_all(&directory);
}

#[test]
fn the_cli_drives_a_session_end_to_end() {
    let name = Named::new("cli");
    let journey = name.name.trim_start_matches("grapt-tui-test-").to_owned();
    let driver = env!("CARGO_BIN_EXE_grapt-tui-driver");
    let run = |args: &[&str]| {
        let output = Command::new(driver)
            .args(args)
            .output()
            .unwrap_or_else(|error| panic!("driver did not run: {error}"));
        (
            output.status.code(),
            String::from_utf8_lossy(&output.stdout).into_owned(),
            String::from_utf8_lossy(&output.stderr).into_owned(),
        )
    };
    let (code, stdout, stderr) = run(&[
        "start",
        "--socket",
        &name.socket,
        "--seat",
        "test",
        "--journey",
        &journey,
        "--width",
        "50",
        "--height",
        "8",
        "--",
        "sh",
        "-c",
        "read line; echo \"line=$line\"; exec sleep 60",
    ]);
    assert_eq!(code, Some(0), "start: {stderr}");
    assert_eq!(stdout.trim(), name.name);
    let (code, _, stderr) = run(&[
        "send",
        "--socket",
        &name.socket,
        "--session",
        &name.name,
        "--text",
        "abc",
        "Enter",
    ]);
    assert_eq!(code, Some(0), "send: {stderr}");
    let (code, stdout, stderr) = run(&[
        "wait",
        "--socket",
        &name.socket,
        "--session",
        &name.name,
        "--text",
        "line=abc",
    ]);
    assert_eq!(code, Some(0), "wait: {stderr}");
    assert!(stdout.contains("line=abc"));
    let (code, _, stderr) = run(&[
        "wait",
        "--socket",
        &name.socket,
        "--session",
        &name.name,
        "--text",
        "absent",
        "--timeout-ms",
        "200",
    ]);
    assert_eq!(code, Some(1), "a missed wait exits 1");
    assert!(
        stderr.contains("line=abc"),
        "a missed wait prints the screen: {stderr}"
    );
    let (code, _, _) = run(&["capture", "--session", "main"]);
    assert_eq!(code, Some(2), "a foreign session is refused");
    let (code, _, stderr) = run(&["stop", "--socket", &name.socket, "--session", &name.name]);
    assert_eq!(code, Some(0), "stop: {stderr}");
    assert!(!alive(&name));
}
