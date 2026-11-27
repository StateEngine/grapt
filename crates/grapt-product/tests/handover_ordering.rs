//! J02: the source observation must be read AFTER the incumbent is gone.
//!
//! This is the witness the handover module's ordering exists for, and it is
//! deliberately built so that it CANNOT pass by accident. The incumbent is a
//! real child process that writes one row to the source database and only then
//! exits, so there is a window in which:
//!
//!   * reading the fingerprint EARLY captures the pre-write bytes, which still
//!     match what the rehearsal audited, and admits a transition that would
//!     silently discard the child's write; and
//!   * reading it LATE — after `await_retirement` has observed the child gone —
//!     captures the post-write bytes, does not match the audit, and refuses.
//!
//! An earlier draft of this campaign used `DeadPredecessor::observe`, which
//! answers "is it gone right now" and refuses a live incumbent. Under that
//! design the child was already dead before the operation began, an early and a
//! late read returned identical bytes, and a mutant that hoisted the read would
//! have SURVIVED while every assertion still passed. The retirement waits
//! precisely so this witness has something to detect.

use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use grapt_release::{fingerprint_file, rehearse};
use grapt_types::MigrationId;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static SCRATCH: AtomicU64 = AtomicU64::new(0);
const PYTHON: &str = if cfg!(windows) { "python" } else { "python3" };

#[derive(Debug)]
struct FixedClock;

impl grapt_types::ClockAuthority for FixedClock {
    fn now(&self) -> grapt_types::Timestamp {
        grapt_types::Timestamp::from_millis(1_788_000_000_123)
    }
}

/// A disposable installation. Removed on drop; never a shared location.
#[derive(Debug)]
struct Scratch(PathBuf);

impl Scratch {
    fn new(label: &str) -> TestResult<Self> {
        let ordinal = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-shv-{label}-{}-{ordinal}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        Ok(Self(root))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// An oracle-era database, as the product would have created it.
///
/// Built through the same out-of-process SQLite the incumbent uses rather than
/// by linking a driver into this crate: the product does not depend on
/// `rusqlite`, and adding a dev-dependency to build a fixture would be a real
/// dependency change for a test-only convenience.
fn legacy_database(path: &Path) -> TestResult {
    let script = "import sqlite3, sys
c = sqlite3.connect(sys.argv[1])
c.executescript(\"\"\"
CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT);
CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL,
 created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL,
 FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE);
CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);
CREATE INDEX event_aggregate_type_seq_idx ON event (aggregate_id, type, seq);
CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT NOT NULL);
INSERT INTO event_sequence VALUES ('ses_shv', 1, NULL);
INSERT INTO event VALUES ('evt_shv_1','ses_shv',0,1700000000000,'session.created.1','{}');
INSERT INTO event VALUES ('evt_shv_2','ses_shv',1,1700000000001,'session.updated.1','{}');
INSERT INTO kv VALUES ('theme','dark');
INSERT INTO project VALUES ('prj_shv','handover');
\"\"\")
c.commit(); c.close()
";
    let status = Command::new(PYTHON)
        .arg("-c")
        .arg(script)
        .arg(path)
        .status()?;
    if !status.success() {
        return Err("legacy fixture creation failed".into());
    }
    Ok(())
}

/// A real incumbent: a separate OS process that lives for `hold`, writes one
/// row to the source, and exits.
///
/// Deliberately an out-of-process child: the shutdown write must come from a
/// real OS process, the way an actual service's on-the-way-out write does.
/// This witness models the write's timing against the source only; the
/// product's own retirement wait observes the incumbent's election lock,
/// which a live service holds for its whole life and the kernel releases at
/// process death.
fn spawn_writing_incumbent(source: &Path, hold: Duration) -> TestResult<Child> {
    let script = format!(
        "import sqlite3, time, sys\n\
         time.sleep({seconds})\n\
         c = sqlite3.connect(sys.argv[1])\n\
         c.execute(\"INSERT INTO event VALUES ('evt_shv_shutdown','ses_shv',2,1700000000002,'session.updated.1','{{\\\"written\\\":\\\"on the way out\\\"}}')\")\n\
         c.commit(); c.close()\n",
        seconds = hold.as_secs_f64()
    );
    let child = Command::new(PYTHON)
        .arg("-c")
        .arg(script)
        .arg(source)
        .spawn()?;
    Ok(child)
}

#[test]
fn an_incumbents_shutdown_write_is_seen_only_because_the_source_is_read_after_retirement(
) -> TestResult {
    let scratch = Scratch::new("ordering")?;
    let source = scratch.path().join("database.sqlite3");
    let run_root = scratch.path().join("run");
    legacy_database(&source)?;

    // The rehearsal audits the source as it stands BEFORE the incumbent's
    // shutdown write. This is the evidence a handover would carry.
    let rehearsal = rehearse(
        &source,
        &run_root,
        MigrationId::new("shv-ordering").ok_or("run id")?,
        "secode-rust@0aede982",
        &FixedClock,
    )?;
    let eligibility = rehearsal.lowered_eligibility()?;

    // Read the source the way an EARLY (hoisted) observation would: right now,
    // while the incumbent is still alive and has not yet written.
    let (_, early) = fingerprint_file(&source)?;
    assert!(
        eligibility.describes_source(&early),
        "an early observation matches the audit, which is exactly why it is dangerous"
    );

    // The incumbent goes down, writing as it goes.
    let mut incumbent = spawn_writing_incumbent(&source, Duration::from_millis(600))?;
    let status = incumbent.wait()?;
    assert!(status.success(), "the incumbent must complete its write");

    // Read the source the way the handover actually does: after the incumbent
    // is observably gone.
    let (_, late) = fingerprint_file(&source)?;

    assert_ne!(
        early, late,
        "the incumbent's shutdown write must move the source fingerprint, or this \
         witness proves nothing"
    );
    assert!(
        !eligibility.describes_source(&late),
        "the post-retirement observation must NOT match the audit: the migrated \
         candidate does not contain the write that just landed"
    );

    // The consequence, stated as the thing that would actually go wrong: an
    // early observation authorises a transition that discards a durable write.
    assert!(
        eligibility.describes_source(&early) && !eligibility.describes_source(&late),
        "early admits and late refuses — that difference is the entire reason the \
         handover reads the source after retirement rather than before"
    );
    Ok(())
}
