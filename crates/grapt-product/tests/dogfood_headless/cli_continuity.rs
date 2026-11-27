//! SESSION-CONTINUITY-01 J02: a durable Session survives process loss and is
//! driven again through the shipped `grapt` CLI. Identity is preserved, the
//! lost attempt settles once as process loss, and each named mismatch refuses
//! with an actionable diagnostic and without mutating durable state.

use super::*;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};

pub(super) use super::shipped_cli::cli_binary;

pub(super) struct CliRun {
    pub(super) code: Option<i32>,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

impl CliRun {
    fn records(&self) -> Vec<serde_json::Value> {
        self.stdout
            .lines()
            .map(|line| {
                serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("CLI JSONL record is invalid ({error}): {line}"))
            })
            .collect()
    }

    pub(super) fn receipt(&self) -> serde_json::Value {
        let records = self.records();
        let receipt = records
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("CLI JSONL stream has no records: {}", self.stderr));
        assert_eq!(
            receipt["schema"], "grapt.cli.run-receipt.v1",
            "the JSONL stream did not end with its run receipt: {}",
            self.stdout
        );
        receipt
    }
}

/// Runs the CLI in the fixture project. New runs belong to the caller cwd,
/// independently of the service profile. `output()` drains stdout and stderr
/// while the CLI runs, so no journey depends on an OS pipe's capacity.
pub(super) fn cli(root: &Path, arguments: &[&str]) -> CliRun {
    let output = Command::new(cli_binary())
        .current_dir(root)
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("CLI launch failed: {error}"));
    CliRun {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    }
}

/// `grapt_cli::SERVER_LOSS_EXIT_BOUND`: one terminal-poll interval plus one
/// request allowance (docs/cli-server-loss/CONTRACT.md). The product and CLI
/// crates do not depend on each other, so the value is restated here.
const SERVER_LOSS_EXIT_BOUND: Duration = Duration::from_secs(31);

fn digest(path: &Path) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("{} digest read failed: {error}", path.display()));
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

/// Launches the product on a store that must refuse, and returns its combined
/// diagnostic output. The refusal must precede service authority.
fn refused_launch(root: &Path, provider: SocketAddr) -> String {
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .arg("serve")
        .arg("--service")
        .arg(root)
        .arg("--port")
        .arg("0")
        .env("GRAPT_LIVE_HEADLESS_LOOPBACK", format!("http://{provider}"))
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("refusal launch failed: {error}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "the product served a store it must refuse: {text}"
    );
    assert!(
        !root.join("serve-ready.json").exists(),
        "a refused store still published service readiness"
    );
    text
}

fn refused_profile_check(root: &Path) -> String {
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["check-profile", "--service"])
        .arg(root)
        .stdin(Stdio::null())
        .output()
        .unwrap_or_else(|error| panic!("profile check failed: {error}"));
    let text = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        !output.status.success(),
        "profile check accepted an incompatible store: {text}"
    );
    text
}

fn recovery_follows_cause(message: &str, cause: &str) {
    let cause_at = message
        .find(cause)
        .unwrap_or_else(|| panic!("missing store cause: {message}"));
    let recovery_at = message
        .find("grapt-migrate disposition")
        .unwrap_or_else(|| panic!("missing recovery guidance: {message}"));
    assert!(
        cause_at < recovery_at,
        "recovery replaced the store cause: {message}"
    );
    assert!(
        message.contains("grapt-migrate restore"),
        "missing restore guidance: {message}"
    );
}

fn process_loss_terminals(root: &Path, session: &str) -> (i64, i64) {
    let store = EventStore::open(&root.join("database.sqlite3"), "cli-continuity-evidence")
        .unwrap_or_else(|error| panic!("continuity evidence store is unreadable: {error}"));
    let count = |sql: &str| -> i64 {
        store
            .connection()
            .query_row(sql, [session], |row| row.get(0))
            .unwrap_or_else(|error| panic!("continuity evidence query failed: {error}"))
    };
    (
        count(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type LIKE 'session.step.failed.%' AND data LIKE '%process%loss%'",
        ),
        count(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type LIKE 'session.step.ended.%'",
        ),
    )
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_crashed_session_resumes_through_the_shipped_cli_and_refuses_named_mismatches() {
    let _journey = process_journey_custody();
    let root = root("j02-cli-continuity");
    prepare(&root, 120_000);
    prepare_model_inventory(&root, 128_000);
    let service = root.to_string_lossy().into_owned();
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("first answer through the CLI"),
        Script::HeldOutput {
            bytes: 32,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::SuccessText("answer after restart"),
    ]);

    let mut child = spawn(&root, provider.address);
    await_ready(&root, &mut child);
    let created = cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--new",
            "--prompt",
            "first task",
            "--jsonl",
        ],
    );
    assert_eq!(
        created.code,
        Some(0),
        "fresh CLI run failed: {}",
        created.stderr
    );
    let session = created.receipt()["session"]
        .as_str()
        .unwrap_or_else(|| panic!("the run receipt named no Session: {}", created.stdout))
        .to_owned();
    assert_eq!(created.receipt()["prompted"], true);

    // Crash the product while the second prompt's Physical Attempt is live.
    let crashing = {
        let root = root.clone();
        let service = service.clone();
        let session = session.clone();
        thread::spawn(move || {
            cli(
                &root,
                &[
                    "run",
                    "--service",
                    &service,
                    "--session",
                    &session,
                    "--prompt",
                    "second task",
                    "--jsonl",
                ],
            )
        })
    };
    let _ = entered.wait();
    child.stop_after_witness(&root);
    let lost = tokio::time::Instant::now();
    drop(child);
    let _ = release.wait();
    let crashed = crashing
        .join()
        .unwrap_or_else(|_| panic!("crashed CLI run thread panicked"));
    let exited_after = lost.elapsed();
    assert_eq!(
        crashed.code,
        Some(12),
        "a CLI run whose product was killed did not exit Remote: {}",
        crashed.stderr
    );
    assert!(
        exited_after <= SERVER_LOSS_EXIT_BOUND,
        "the CLI took {exited_after:?} to leave its lost server, beyond {SERVER_LOSS_EXIT_BOUND:?}"
    );

    let mut restarted = spawn(&root, provider.address);
    let address_after = await_ready(&root, &mut restarted);
    let attached = cli(
        &root,
        &["attach", "--service", &service, "--resume", "--jsonl"],
    );
    assert_eq!(
        attached.code,
        Some(0),
        "CLI resume failed: {}",
        attached.stderr
    );
    assert_eq!(
        attached.receipt()["session"],
        session.as_str(),
        "`attach --resume` did not select the crashed Session"
    );
    let (lost, ended) = process_loss_terminals(&root, &session);
    assert_eq!(
        lost, 1,
        "the lost attempt did not settle exactly once as process loss"
    );
    assert_eq!(
        ended, 1,
        "the lost attempt was resurrected as a successful Step"
    );

    let history_path = format!("/api/session/{session}/message");
    let before = json(address_after, "GET", &history_path, "");
    let resumed = cli(
        &root,
        &[
            "run",
            "--service",
            &service,
            "--session",
            &session,
            "--prompt",
            "third task",
            "--jsonl",
        ],
    );
    assert_eq!(
        resumed.code,
        Some(0),
        "CLI run after restart failed: {}",
        resumed.stderr
    );
    assert_eq!(resumed.receipt()["session"], session.as_str());
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    let (lost, ended) = process_loss_terminals(&root, &session);
    assert_eq!(
        (lost, ended),
        (1, 2),
        "resuming duplicated or resurrected a terminal"
    );
    let history = json(address_after, "GET", &history_path, "");
    assert!(history.to_string().contains("answer after restart"));
    assert_ne!(history, before, "the resumed run left no durable history");

    // Named mismatches refuse without mutation.
    let database = root.join("database.sqlite3");
    let stable = json(address_after, "GET", &history_path, "");
    let wrong_project = cli(
        &root,
        &[
            "attach",
            "--service",
            &service,
            "--session",
            &session,
            "--project",
            "prj_foreign",
            "--jsonl",
        ],
    );
    assert!(
        wrong_project.code != Some(0)
            && wrong_project.stderr.contains("belongs to project")
            && wrong_project.stderr.contains("prj_foreign"),
        "a wrong project was not refused actionably: exit {:?}: {}",
        wrong_project.code,
        wrong_project.stderr
    );
    let other_root = root.join("not-a-service");
    fs::create_dir_all(&other_root).unwrap_or_else(|error| panic!("foreign root failed: {error}"));
    let wrong_root = cli(
        &root,
        &[
            "attach",
            "--service",
            &other_root.to_string_lossy(),
            "--session",
            &session,
            "--jsonl",
        ],
    );
    assert_eq!(
        wrong_root.code,
        Some(10),
        "a wrong service root was not a discovery refusal: {}",
        wrong_root.stderr
    );
    assert!(
        wrong_root.stderr.starts_with("grapt: "),
        "wrong root diagnostic: {}",
        wrong_root.stderr
    );
    let foreign_cursor =
        URL_SAFE_NO_PAD.encode(br#"{"id":"msg_foreign","order":"desc","direction":"next"}"#);
    let (status, body) = request(
        address_after,
        "GET",
        &format!("{history_path}?cursor={foreign_cursor}"),
        "",
    );
    assert_eq!(
        status,
        400,
        "a stale cursor was answered: {}",
        String::from_utf8_lossy(&body)
    );
    assert!(
        String::from_utf8_lossy(&body)
            .contains("Cursor does not identify a message in this Session"),
        "a stale cursor was not refused actionably: {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(json(address_after, "GET", &history_path, ""), stable);
    restarted.stop_after_witness(&root);
    drop(restarted);

    // The real crash/resume journey above uses the live WAL policy. The
    // following corruption fixtures replace main-file bytes, so settle the
    // WAL and explicitly select rollback journaling before taking that image.
    {
        let connection = rusqlite::Connection::open(&database)
            .unwrap_or_else(|error| panic!("corruption fixture open failed: {error}"));
        connection
            .execute_batch("PRAGMA journal_mode = DELETE;")
            .unwrap_or_else(|error| panic!("corruption fixture journal mode failed: {error}"));
    }
    let good = fs::read(&database).unwrap_or_else(|error| panic!("database read failed: {error}"));
    {
        let connection = rusqlite::Connection::open(&database)
            .unwrap_or_else(|error| panic!("version fixture open failed: {error}"));
        connection
            .execute(
                "INSERT INTO rust_migration (id, time_completed) VALUES ('9999_from_a_newer_engine', 1)",
                [],
            )
            .unwrap_or_else(|error| panic!("version fixture insert failed: {error}"));
    }
    let newer = digest(&database);
    let refused = refused_launch(&root, provider.address);
    assert!(
        refused.contains("9999_from_a_newer_engine"),
        "an incompatible schema version was not named: {refused}"
    );
    recovery_follows_cause(&refused, "9999_from_a_newer_engine");
    let checked = refused_profile_check(&root);
    recovery_follows_cause(&checked, "9999_from_a_newer_engine");
    assert_eq!(
        digest(&database),
        newer,
        "the version refusal mutated the store"
    );

    let mut corrupt = good.clone();
    corrupt[..16].copy_from_slice(b"not a sqlite db\0");
    fs::write(&database, &corrupt)
        .unwrap_or_else(|error| panic!("corrupt fixture failed: {error}"));
    let refused = refused_launch(&root, provider.address);
    assert!(
        refused.to_ascii_lowercase().contains("corrupt") || refused.contains("not a database"),
        "corrupt durable state was not refused actionably: {refused}"
    );
    recovery_follows_cause(
        &refused,
        if refused.contains("not a database") {
            "not a database"
        } else {
            "corrupt"
        },
    );
    let checked = refused_profile_check(&root);
    assert!(
        checked.to_ascii_lowercase().contains("corrupt") || checked.contains("not a database"),
        "profile check lost the corruption cause: {checked}"
    );
    recovery_follows_cause(
        &checked,
        if checked.contains("not a database") {
            "not a database"
        } else {
            "corrupt"
        },
    );
    assert_eq!(
        fs::read(&database).ok(),
        Some(corrupt),
        "the corruption refusal mutated the store"
    );

    fs::write(&database, &good).unwrap_or_else(|error| panic!("database restore failed: {error}"));
    let mut recovered = spawn(&root, provider.address);
    let address_recovered = await_ready(&root, &mut recovered);
    assert_eq!(
        json(address_recovered, "GET", &history_path, ""),
        stable,
        "refused launches changed the durable Session"
    );
    recovered.stop_after_witness(&root);
    drop(recovered);
    process_fixture::cleanup(&root);
}
