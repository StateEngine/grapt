//! J01: the handover as one real operation, driven through the shipped binary.
//!
//! Everything here goes through `grapt-server handover` against a disposable
//! installation with a real registration on disk. Nothing calls the halves
//! directly — a helper driving `validate` and `elect` in sequence would prove
//! that the two functions exist, not that the product joins them.
//!
//! What each test asserts is the OUTCOME an operator would see: the process's
//! own launch receipt, and the state of the slot afterwards.

#[path = "support/provider_profile.rs"]
mod provider_profile;
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_service::RegistrationRecord;
use grapt_types::authority::Timestamp;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};

static SCRATCH: AtomicU64 = AtomicU64::new(0);
#[cfg(feature = "handover-crash-fixture")]
#[path = "handover_journey/crashes.rs"]
mod crashes;
#[path = "plugin_compatibility/evidence.rs"]
mod evidence;
#[path = "handover_journey/lifecycle.rs"]
mod lifecycle;
#[cfg(feature = "handover-crash-fixture")]
use lifecycle::await_ready;
#[cfg(feature = "handover-crash-fixture")]
use lifecycle::{fixture_storage, product_installation};
use lifecycle::{Held, OBSERVATION_BUDGET};
const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };
// Start hostile writes from the durable completed rehearsal, not process-launch
// timing. The short grace lets the caller enter its bounded retirement wait.
const AFTER_REHEARSAL: &str = "import pathlib,sys,time\np=pathlib.Path(sys.argv[1])\nmarker=pathlib.Path(sys.argv[2])\nfor _ in range(1000):\n if marker.is_file(): break\n time.sleep(0.02)\nelse: raise RuntimeError('rehearsal checkpoint absent')\ntime.sleep(0.3)\n";

/// A portable election-lock prelude for the python fixtures: a real kernel
/// lock on the elected-registration file — an msvcrt byte-range lock
/// (LockFileEx) on Windows, flock on Unix — the same kernel lock the
/// production election holds on every platform. `{path}` is replaced with
/// the Python expression for the lock path.
const ELECTION_LOCK_PRELUDE: &str =
    "import os,sys\n_lock_path={path}\ntry:\n import msvcrt\n _lf=open(_lock_path,'a+b')\n _lf.seek(0)\n msvcrt.locking(_lf.fileno(),msvcrt.LK_NBLCK,1)\nexcept ModuleNotFoundError:\n import fcntl\n _lf=open(_lock_path,'a+b')\n fcntl.flock(_lf,fcntl.LOCK_EX)\n";

/// A pid that is certainly not running.
///
/// Chosen high and then confirmed absent rather than assumed: a handover whose
/// incumbent is accidentally alive would exercise a different path.
const ABSENT_PID: u32 = 0x7FFF_0000;

fn scratch(label: &str) -> PathBuf {
    let ordinal = SCRATCH.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!(
        "grapt-shv-journey-{label}-{}-{ordinal}",
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&root);
    fs::create_dir_all(&root).unwrap_or_else(|error| panic!("scratch: {error}"));
    root
}

/// A disposable installation, configured to MIGRATE its store.
///
/// `store.authority` is `migrate` because that is the situation a handover
/// exists for: an oracle-era database that must cross versions. The
/// `product.build` is the attested identity, which is what gets stamped
/// durably into the database.
fn write_inputs(root: &Path, build: &str) {
    let initialized = Command::new("git")
        .args(["init", "--quiet"])
        .arg(root)
        .output()
        .unwrap_or_else(|error| panic!("service repository initialization: {error}"));
    assert!(initialized.status.success(), "git init failed");
    fs::write(
        root.join("service.conf"),
        "slot=product\ninstance=instance-a\nenvironment=internal-alpha\nversion=0.1.0\n\
         serve_window_ms=250\nprovider_mode=production\nprovider_connect_ms=10000\n\
         provider_response_head_ms=60000\nprovider_active_attempts=4\n",
    )
    .unwrap_or_else(|error| panic!("config: {error}"));
    fs::write(root.join("basic.credential"), "alpha:zephyr")
        .unwrap_or_else(|error| panic!("credential: {error}"));
    let platform = format!(
        r#"{{
      "store": {{ "authority": "migrate" }},
      "product": {{ "build": "{build}" }},
      "provider": {{
        "id": "openai", "model": "gpt-5.6", "base_model": "gpt-5.6", "family": "gpt-5",
        "route": "responses", "account": "account-a", "credential_version": "credential-v1",
        "policy_version": "df0-policy-v1", "variants": []
      }}
    }}"#
    );
    fs::write(root.join("grapt.jsonc"), platform)
        .unwrap_or_else(|error| panic!("platform config: {error}"));
    fs::write(root.join("provider.credential"), "provider-zephyr")
        .unwrap_or_else(|error| panic!("provider credential: {error}"));
    if let Err(error) = provider_profile::configure(root, SocketAddr::from(([127, 0, 0, 1], 1))) {
        panic!("loopback fixture profile failed: {error}");
    }
}

/// An oracle-era database, built out of process so this crate needs no
/// SQLite driver of its own.
fn legacy_database(path: &Path) {
    let script = "import sqlite3, sys\n\
c = sqlite3.connect(sys.argv[1])\n\
c.executescript(\"\"\"\n\
CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT);\n\
CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,\n\
 created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL,\n\
 FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE);\n\
CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);\n\
CREATE INDEX event_aggregate_type_seq_idx ON event (aggregate_id, type, seq);\n\
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);\n\
CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT NOT NULL);\n\
INSERT INTO event_sequence VALUES ('ses_shv', 1, NULL);\n\
INSERT INTO event VALUES ('evt_shv_1','ses_shv',0,1700000000000,'session.created.1','{}');\n\
INSERT INTO event VALUES ('evt_shv_2','ses_shv',1,1700000000001,'session.updated.1','{}');\n\
INSERT INTO kv VALUES ('theme','dark');\n\
INSERT INTO project VALUES ('prj_shv','handover');\n\
\"\"\")\n\
c.commit(); c.close()\n";
    let status = Command::new(PYTHON)
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()
        .unwrap_or_else(|error| panic!("legacy fixture: {error}"));
    assert!(status.success(), "legacy fixture creation failed");
}

/// Put a registration on disk naming `pid` as the incumbent.
fn register_incumbent(root: &Path, pid: u32) {
    let owner = OwnerReceipt::new(
        ServiceSlotId::new("product").unwrap_or_else(|| panic!("slot")),
        ServiceInstanceId::new("predecessor").unwrap_or_else(|| panic!("instance")),
        EnvironmentReceipt::new("internal-alpha").unwrap_or_else(|| panic!("environment")),
        ServiceGeneration::new(7),
        ProcessId::new(pid).unwrap_or_else(|| panic!("pid")),
        Endpoint::new("http://127.0.0.1:49151").unwrap_or_else(|| panic!("endpoint")),
    );
    let bytes = RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new("0.1.0").unwrap_or_else(|| panic!("version")),
        Timestamp::from_millis(0),
    )
    .unwrap_or_else(|error| panic!("registration: {error}"))
    .encode();
    fs::write(root.join("registration"), &bytes).unwrap_or_else(|error| panic!("write: {error}"));
}

fn handover(root: &Path, run_root: &Path, run_id: &str, engine_version: &str) -> Output {
    grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["handover", "--service"])
        .arg(root)
        .arg("--run-root")
        .arg(run_root)
        .args(["--run-id", run_id, "--engine-version", engine_version])
        .output()
        .unwrap_or_else(|error| panic!("handover failed to launch: {error}"))
}

fn receipt(output: &Output) -> serde_json::Value {
    receipt_line(&String::from_utf8_lossy(&output.stdout))
}

fn receipt_line(text: &str) -> serde_json::Value {
    let line = text
        .lines()
        .last()
        .unwrap_or_else(|| panic!("no receipt on stdout: {text}"));
    serde_json::from_str(line).unwrap_or_else(|error| panic!("receipt parse {error}: {line}"))
}

#[test]
fn the_handover_verb_retires_validates_and_commits_through_the_real_binary() {
    let root = scratch("committed");
    write_inputs(&root, "secode-rust@0aede982-handover");
    legacy_database(&root.join("database.sqlite3"));
    register_incumbent(&root, ABSENT_PID);
    let run_root = root.join("run");

    let output = handover(
        &root,
        &run_root,
        "shv-journey",
        "secode-rust@0aede982-handover",
    );
    let receipt = receipt(&output);
    assert!(
        output.status.success(),
        "handover refused: {receipt}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    // The launch receipt reports it was a handover, and reports the fresh
    // observation it validated against rather than leaving it implicit.
    assert_eq!(receipt["outcome"], "served");
    let evidence = &receipt["handover"];
    assert_eq!(evidence["schema"], "grapt.product.handover.v1");
    assert_eq!(
        evidence["retired_incumbent"], true,
        "the registered incumbent must have been retired: {receipt}"
    );
    assert!(
        evidence["observed_source"]
            .as_str()
            .is_some_and(|f| !f.is_empty()),
        "the receipt must carry the post-retirement source observation: {receipt}"
    );
    assert!(
        evidence["serving"].as_str().is_some_and(|f| !f.is_empty()),
        "the receipt must name the candidate it was entitled to serve: {receipt}"
    );

    // The registration actually changed hands through the controller's own CAS,
    // and the controller's own classification says so rather than this test
    // inferring it.
    assert_eq!(
        evidence["switch_outcome"], "Replaced",
        "the incumbent's registration must have been replaced: {receipt}"
    );

    // And then released on clean shutdown, which is ordinary serve behaviour
    // the handover does not change. The registration is therefore ABSENT at the
    // end — its absence here is the released state, not a handover that never
    // committed, which is why the switch outcome above is the real evidence.
    assert_eq!(receipt["registration_released"], true, "{receipt}");
    assert!(
        !root.join("registration").exists(),
        "a cleanly released slot leaves no registration behind"
    );

    // The rehearsal's own artifacts exist: this ran a real migration rehearsal,
    // not a stubbed one.
    assert!(
        run_root.join("checkpoints").is_dir(),
        "the run root must hold the rehearsal journal"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn ordinary_serve_is_unchanged_and_publishes_no_handover_evidence() {
    let root = scratch("ordinary");
    write_inputs(&root, "secode-rust@0aede982-ordinary");
    legacy_database(&root.join("database.sqlite3"));

    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["serve", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .output()
        .unwrap_or_else(|error| panic!("serve failed to launch: {error}"));
    let receipt = receipt(&output);
    assert!(
        output.status.success(),
        "ordinary serve refused: {receipt}\nstderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(receipt["outcome"], "served");
    assert!(
        receipt.get("handover").is_none(),
        "ordinary startup must publish no handover evidence: {receipt}"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_live_incumbent_that_will_not_retire_refuses_the_handover_by_name() {
    let root = scratch("live-incumbent");
    write_inputs(&root, "secode-rust@0aede982-live");
    legacy_database(&root.join("database.sqlite3"));

    // A real process that outlives the retirement bound. The retirement law
    // is the service's own election on every OS: the incumbent holds the
    // elected-registration lock for its whole serve life, and the kernel
    // releases it at death — no pid table is consulted. This fixture holds
    // that lock exactly as a real serving process does. It lives until its
    // stdin closes, never on a clock: a fixed 30 s life expired first when
    // rehearsal took over 63 s (trace 20260924192435-4327654641e44b51).
    let incumbent_script = format!(
        "{}sys.stdin.buffer.read()\n",
        ELECTION_LOCK_PRELUDE.replace("{path}", "sys.argv[1]")
    );
    let mut incumbent = Held::spawn(
        "python-incumbent",
        Command::new(PYTHON)
            .args(["-c", incumbent_script.as_str()])
            .arg(root.join("registration.elected")),
    );
    register_incumbent(&root, incumbent.pid());

    let registration = fs::read(root.join("registration"))
        .unwrap_or_else(|error| panic!("registration before refusal: {error}"));

    let output = handover(
        &root,
        &root.join("run"),
        "shv-live",
        "secode-rust@0aede982-live",
    );
    let receipt = receipt(&output);
    assert!(
        incumbent.exited() == Ok(false),
        "a live PID alone must never authorize terminating another process"
    );
    // TD2: the test-owned stdin is the incumbent's whole lifetime.
    incumbent.release();
    let (status, _) = incumbent
        .await_exit(OBSERVATION_BUDGET)
        .unwrap_or_else(|state| {
            panic!(
                "TD2 released incumbent did not exit: {}",
                incumbent.failure(&state)
            )
        });
    assert!(status.success(), "TD2 released incumbent exit {status:?}");

    assert!(
        !output.status.success(),
        "TD3 a live incumbent yielded a handover: {receipt}"
    );
    assert_eq!(
        receipt["outcome"], "refused",
        "TD3 a live incumbent's handover was not refused: {receipt}"
    );
    let reason = receipt["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("transition-incumbent-may-still-be-writing"),
        "TD3 the refusal must name the incumbent law rather than fail generically: {reason}"
    );
    assert_eq!(
        fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration after refusal: {error}")),
        registration,
        "the incumbent's exact registration must survive a refused handover"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_source_written_after_the_rehearsal_refuses_before_the_store_is_migrated() {
    let root = scratch("stale-source");
    write_inputs(&root, "secode-rust@0aede982-stale");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let run_root = root.join("run");

    // Establish the evidence at the source's CURRENT state, before the late
    // write. The handover would otherwise rehearse the moved source itself and
    // there would be no staleness to detect.
    grapt_release::rehearse(
        &source,
        &run_root,
        grapt_types::MigrationId::new("shv-stale").unwrap_or_else(|| panic!("run id")),
        "secode-rust@0aede982-stale",
        &grapt_store::SystemClock,
    )
    .unwrap_or_else(|error| panic!("rehearsal: {error}"));

    // Then the source moves, exactly as an un-quiesced incumbent would move it.
    let written = Command::new(PYTHON)
        .args([
            "-c",
            "import sqlite3, sys\nc = sqlite3.connect(sys.argv[1])\nc.execute(\"INSERT INTO event VALUES ('evt_late','ses_shv',2,1700000000002,'session.updated.1','{}')\")\nc.commit(); c.close()\n",
        ])
        .arg(&source)
        .status()
        .unwrap_or_else(|error| panic!("late write: {error}"));
    assert!(written.success(), "the late write must land");

    let before = fs::read(&source).unwrap_or_else(|error| panic!("{error}"));
    let output = handover(&root, &run_root, "shv-stale", "secode-rust@0aede982-stale");
    let receipt = receipt(&output);

    assert!(
        !output.status.success(),
        "a source that moved after its audit must not yield a handover: {receipt}"
    );
    // And the refusal happened BEFORE the platform migrated anything: the
    // database is still byte-identical to what it was going in, so the gate
    // genuinely gates rather than reporting after the fact.
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("{error}")),
        before,
        "a refused handover must leave the database untouched"
    );
    let _ = fs::remove_dir_all(&root);
}

#[test]
fn a_write_during_the_retirement_wait_is_caught_only_by_the_post_retirement_read() {
    let root = scratch("race");
    write_inputs(&root, "secode-rust@0aede982-race");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let run_root = root.join("run");

    // This is the race the whole ordering exists for, and it is NOT the same as
    // a source that had already moved before the operation began. There, the
    // evidence owner's own revalidation refuses while minting eligibility. Here
    // the source is pristine when eligibility is minted and moves DURING the
    // retirement wait, so only a read taken after that wait can see it. The
    // fixture is the incumbent: on linux it holds the election lock across the
    // late write, exactly as a real serving process holds it while alive, so
    // the retirement wait spans the write: the fixture holds the election
    // lock on every platform, exactly as a real serving process does.
    let incumbent_lock = ELECTION_LOCK_PRELUDE.replace(
        "{path}",
        "os.path.join(os.path.dirname(sys.argv[1]),'registration.elected')",
    );
    let mut incumbent = Command::new(PYTHON)
        .arg("-c")
        .arg(format!("{incumbent_lock}{AFTER_REHEARSAL}import sqlite3\nc = sqlite3.connect(p)\nc.execute(\"INSERT INTO event VALUES ('evt_during','ses_shv',2,1700000000002,'session.updated.1','{{}}')\")\nc.commit(); c.close()\n"))
        .arg(&source)
        .arg(run_root.join("checkpoints/08-ready-for-handoff.json"))
        .spawn()
        .unwrap_or_else(|error| panic!("incumbent spawn: {error}"));
    register_incumbent(&root, incumbent.id());

    let reaper = std::thread::spawn(move || incumbent.wait());
    let output = handover(&root, &run_root, "shv-race", "secode-rust@0aede982-race");
    let receipt = receipt(&output);
    // Reaped rather than abandoned: a test that leaves a child behind is the
    // orphan this campaign's endurance journey exists to rule out.
    assert!(reaper
        .join()
        .unwrap_or_else(|_| panic!("incumbent reaper panicked"))
        .unwrap_or_else(|error| panic!("incumbent reap: {error}"))
        .success());

    assert!(
        !output.status.success(),
        "a write landing during the retirement wait must refuse the handover: {receipt}"
    );
    let reason = receipt["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("transition-source-moved-since-audit"),
        "the refusal must be the source-moved law, not the incumbent law or a \
         generic failure: {reason}"
    );

    // The assertion that pins WHERE the gate lives. The election carries this
    // same law, so refusing proves only that SOMETHING refused. What the gate's
    // position buys is that the refusal happened before `PlatformOwner::prepare`
    // migrated the store: the database is still oracle-era,
    // `PRAGMA user_version = 0`, with no ownership stamped. Move the check to
    // the election and THIS assertion is the one that fails, because the store
    // is migrated by then and there is no reverse path.
    assert_eq!(
        user_version(&source),
        0,
        "a refused handover must leave the store unmigrated and unowned"
    );
    let _ = fs::remove_dir_all(&root);
}

/// `PRAGMA user_version`, read out of process. 0 is oracle-era; 2 is
/// Rust-owned and there is no way back.
fn user_version(database: &Path) -> i64 {
    let output = Command::new(PYTHON)
        .args([
            "-c",
            "import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
print(c.execute('PRAGMA user_version').fetchone()[0])
c.close()
",
        ])
        .arg(database)
        .output()
        .unwrap_or_else(|error| panic!("user_version read: {error}"));
    String::from_utf8_lossy(&output.stdout)
        .trim()
        .parse()
        .unwrap_or_else(|error| panic!("user_version parse: {error}"))
}

#[test]
fn concurrent_election_refuses_before_migrating_the_source() {
    let root = scratch("election-held");
    write_inputs(&root, "secode-rust@0aede982-held");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
    let policy = grapt_service::LockAttemptPolicy::new(1).unwrap_or_else(|| panic!("lock policy"));
    let authority =
        grapt_service::ElectedControllerAuthority::acquire(root.join("registration"), policy)
            .unwrap_or_else(|error| panic!("incumbent election: {error}"));
    let output = handover(
        &root,
        &root.join("run"),
        "shv-held",
        "secode-rust@0aede982-held",
    );
    let result = receipt(&output);
    assert!(
        !output.status.success(),
        "a held election admitted handover: {result}"
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        before
    );
    assert!(!root.join("registration").exists());
    let ordinary = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["serve", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .output()
        .unwrap_or_else(|error| panic!("ordinary contender: {error}"));
    assert!(
        !ordinary.status.success(),
        "ordinary startup bypassed election custody"
    );
    assert_eq!(
        fs::read(&source)
            .unwrap_or_else(|error| panic!("source after ordinary contender: {error}")),
        before
    );
    drop(authority);
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn empty_registration_is_corruption_and_does_not_authorize_migration() {
    let root = scratch("empty-registration");
    write_inputs(&root, "secode-rust@0aede982-empty");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    fs::write(root.join("registration"), b"")
        .unwrap_or_else(|error| panic!("registration: {error}"));
    let before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
    let output = handover(
        &root,
        &root.join("run"),
        "shv-empty",
        "secode-rust@0aede982-empty",
    );
    let result = receipt(&output);
    assert!(
        !output.status.success(),
        "empty registration admitted handover: {result}"
    );
    assert!(
        result["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("registration is unreadable")),
        "{result}"
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        before
    );
    assert_eq!(
        fs::metadata(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration after: {error}"))
            .len(),
        0
    );
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn wrong_build_refuses_before_rehearsal_or_store_mutation() {
    let root = scratch("wrong-build");
    write_inputs(&root, "secode-rust@configured");
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
    let output = handover(&root, &root.join("run"), "shv-build", "secode-rust@wrong");
    let result = receipt(&output);
    assert!(!output.status.success(), "wrong build admitted: {result}");
    assert!(
        result["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("transition-build-mismatch")),
        "{result}"
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        before
    );
    assert!(!root.join("run").exists());
    assert!(!root.join("registration").exists());
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[cfg(feature = "handover-crash-fixture")]
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn live_product_handover_preserves_a_product_created_database() {
    let build = "secode-rust@live-product";
    let root = product_installation("live-product", build, 2000);
    // Four cycles exercise repeated OS process retirement and lock reuse on
    // one store. Each incumbent serves its full 2000 ms window and is then held
    // until the handover announces the incumbent it decoded, so retirement
    // lands inside the product's own 3 s bound (E3). Every cycle must preserve
    // domain state and release both receipts.
    for cycle in 0..4 {
        let mut incumbent = Held::spawn(
            "incumbent",
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
                .args(["serve", "--service"])
                .arg(&root)
                .args(["--port", "0"])
                .env("GRAPT_FIXTURE_HOLD_AT", "serve-window-end")
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null()),
        );
        let child_pid = incumbent.pid();
        let ready_path = root.join("serve-ready.json");
        let ready = await_ready(&ready_path, OBSERVATION_BUDGET, || incumbent.exited())
            .unwrap_or_else(|state| {
                panic!(
                    "cycle={cycle}: incumbent never became ready: {}",
                    incumbent.failure(&state)
                )
            });
        let ready_ms = ready.elapsed.as_millis();
        let ready: serde_json::Value = serde_json::from_slice(&ready.bytes)
            .unwrap_or_else(|error| panic!("ready receipt: {error}"));
        assert_eq!(ready["pid"], child_pid);
        let before = grapt_release::domain_fingerprint(&root.join("database.sqlite3"))
            .unwrap_or_else(|error| panic!("domain before: {error}"));
        let mut successor = Held::spawn(
            "handover",
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
                .args(["handover", "--service"])
                .arg(&root)
                .arg("--run-root")
                .arg(root.join(format!("run-{cycle}")))
                .args(["--run-id", &format!("shv-live-product-{cycle}")])
                .args(["--engine-version", build])
                .env("GRAPT_FIXTURE_ANNOUNCE_AT", "before-retirement")
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped()),
        );
        let mut stdout = lifecycle::Pipe::watch(successor.stdout(), None);
        let mut stderr = lifecycle::Pipe::watch(successor.stderr(), Some(lifecycle::ANNOUNCE));
        let (announce, announce_elapsed) =
            stderr
                .await_marker(OBSERVATION_BUDGET)
                .unwrap_or_else(|state| {
                    panic!(
                        "cycle={cycle}: {}; {}; stderr={}",
                        successor.failure(&state),
                        incumbent.failure("released by teardown"),
                        stderr.render()
                    )
                });
        // RT1: the handover read the registration naming THIS incumbent. Only
        // then is the incumbent released, so no release precedes that read.
        assert_eq!(
            announce.strip_prefix(lifecycle::ANNOUNCE),
            Some(child_pid.to_string().as_str()),
            "RT1 announced a different incumbent: cycle={cycle}"
        );
        assert!(ready_path.is_file() && root.join("registration").is_file());
        incumbent.release();
        let (status, retired) = incumbent
            .await_exit(OBSERVATION_BUDGET)
            .unwrap_or_else(|state| panic!("cycle={cycle}: {}", incumbent.failure(&state)));
        assert!(status.success(), "cycle={cycle}: incumbent exit {status:?}");
        let (handover_status, _) =
            successor
                .await_exit(OBSERVATION_BUDGET)
                .unwrap_or_else(|state| {
                    panic!(
                        "cycle={cycle}: {}; stdout={}",
                        successor.failure(&state),
                        stdout.render()
                    )
                });
        let (text, drained) = stdout.settle();
        let result = receipt_line(&text);
        assert!(
            handover_status.success(),
            "cycle={cycle}: live product handover {handover_status:?}: {result}; stdout {drained}; \
             stderr={}",
            stderr.render()
        );
        assert_eq!(
            result["handover"]["retired_incumbent"],
            true,
            "cycle={cycle}: incumbent {child_pid} not retired; exited {}ms after release; \
             result={result}",
            retired.as_millis()
        );
        assert_ne!(result["pid"], child_pid);
        assert_ne!(
            result["service_instance"], ready["service_instance"],
            "a successor must have a fresh owner identity even after clean registration release"
        );
        assert_eq!(result["registration_released"], true);
        assert_eq!(
            grapt_release::domain_fingerprint(&root.join("database.sqlite3"))
                .unwrap_or_else(|error| panic!("domain after: {error}")),
            before
        );
        assert!(!ready_path.exists());
        assert!(!root.join("registration").exists());
        let (entries, bytes) = fixture_storage(&root);
        println!(
            "SHV_CYCLE {}",
            serde_json::json!({"cycle": cycle, "incumbent": ready,
            "readiness_elapsed_ms": ready_ms,
            "announce_elapsed_ms": announce_elapsed.as_millis(),
            "incumbent_exit_after_release_ms": retired.as_millis(),
            "successor": result, "domain": before.as_str(), "entries": entries, "bytes": bytes})
        );
    }
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn replaced_registration_during_retirement_refuses_before_migration() {
    let root = scratch("replaced-registration");
    let build = "secode-rust@registration-race";
    write_inputs(&root, build);
    let source = root.join("database.sqlite3");
    legacy_database(&source);
    let before = fs::read(&source).unwrap_or_else(|error| panic!("source: {error}"));
    register_incumbent(&root, ABSENT_PID);
    let replacement =
        fs::read(root.join("registration")).unwrap_or_else(|error| panic!("replacement: {error}"));
    fs::write(root.join("replacement"), &replacement)
        .unwrap_or_else(|error| panic!("replacement input: {error}"));
    // The fixture holds the election lock until the replacement has landed
    // (a real serving process keeps the lock until it is gone), on every
    // platform, so the retirement wait spans the replacement.
    let incumbent_lock =
        ELECTION_LOCK_PRELUDE.replace("{path}", "os.path.join(sys.argv[1],'registration.elected')");
    let mut incumbent = Command::new(PYTHON)
        .arg("-c")
        .arg(format!(
            "{incumbent_lock}{AFTER_REHEARSAL}(p/'registration').write_bytes((p/'replacement').read_bytes())"
        ))
        .arg(&root)
        .arg(root.join("run/checkpoints/08-ready-for-handoff.json"))
        .spawn()
        .unwrap_or_else(|error| panic!("incumbent: {error}"));
    register_incumbent(&root, incumbent.id());
    let reaper = std::thread::spawn(move || incumbent.wait());
    let output = handover(&root, &root.join("run"), "shv-registration-race", build);
    assert!(reaper
        .join()
        .unwrap_or_else(|_| panic!("reaper panicked"))
        .unwrap_or_else(|error| panic!("reap: {error}"))
        .success());
    let result = receipt(&output);
    assert!(
        !output.status.success(),
        "replaced registration admitted: {result}"
    );
    assert!(
        result["reason"]
            .as_str()
            .is_some_and(|reason| reason.contains("registration changed during retirement")),
        "{result}"
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        before
    );
    assert_eq!(
        fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration after: {error}")),
        replacement
    );
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[test]
fn failed_preparation_releases_election_and_preserves_registration_for_recovery() {
    let absent = scratch("absent-inputs");
    fs::remove_dir(&absent).unwrap_or_else(|error| panic!("remove empty fixture: {error}"));
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["serve", "--service"])
        .arg(&absent)
        .args(["--port", "0"])
        .output()
        .unwrap_or_else(|error| panic!("absent installation: {error}"));
    let refusal = receipt(&output);
    assert!(!output.status.success());
    assert!(
        refusal["reason"]
            .as_str()
            .is_some_and(|reason| reason.starts_with("platform bootstrap refused before bind:")),
        "election displaced the existing configuration refusal: {refusal}"
    );
    assert!(!absent.exists());
    let root = scratch("preparation-refusal");
    write_inputs(&root, "secode-rust@preparation");
    register_incumbent(&root, ABSENT_PID);
    let original =
        fs::read(root.join("registration")).unwrap_or_else(|error| panic!("registration: {error}"));
    let source = root.join("database.sqlite3");
    fs::write(&source, b"corrupt database")
        .unwrap_or_else(|error| panic!("corrupt fixture: {error}"));
    let failed = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["serve", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .output()
        .unwrap_or_else(|error| panic!("failed preparation: {error}"));
    assert!(!failed.status.success());
    assert_eq!(
        fs::read(root.join("registration"))
            .unwrap_or_else(|error| panic!("registration after: {error}")),
        original
    );
    assert_eq!(
        fs::read(&source).unwrap_or_else(|error| panic!("source after: {error}")),
        b"corrupt database"
    );
    let policy = grapt_service::LockAttemptPolicy::new(1).unwrap_or_else(|| panic!("lock policy"));
    let election =
        grapt_service::ElectedControllerAuthority::acquire(root.join("registration"), policy)
            .unwrap_or_else(|error| panic!("failed preparation leaked election: {error}"));
    drop(election);
    fs::remove_file(&source)
        .unwrap_or_else(|error| panic!("replace corrupt disposable fixture: {error}"));
    legacy_database(&source);
    let recovered =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["serve", "--service"])
            .arg(&root)
            .args(["--port", "0"])
            .output()
            .unwrap_or_else(|error| panic!("recovery: {error}"));
    assert!(
        recovered.status.success(),
        "{}",
        String::from_utf8_lossy(&recovered.stdout)
    );
    assert!(!root.join("registration").exists());
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}

#[cfg(not(feature = "handover-crash-fixture"))]
#[test]
fn ordinary_binary_ignores_the_fixture_crash_selector() {
    let root = scratch("inactive-selector");
    write_inputs(&root, "secode-rust@selector");
    legacy_database(&root.join("database.sqlite3"));
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["handover", "--service"])
        .arg(&root)
        .arg("--run-root")
        .arg(root.join("run"))
        .args([
            "--run-id",
            "shv-selector",
            "--engine-version",
            "secode-rust@selector",
        ])
        .env("GRAPT_HANDOVER_CRASH_AT", "before-retirement")
        .output()
        .unwrap_or_else(|error| panic!("ordinary binary: {error}"));
    assert!(
        output.status.success(),
        "ordinary build enabled fixture exit: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    assert_eq!(receipt(&output)["outcome"], "served");
    fs::remove_dir_all(root).unwrap_or_else(|error| panic!("cleanup: {error}"));
}
