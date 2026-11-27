use grapt_release::{
    fingerprint_file, inspect_status, rehearse, restore_backup_rehearsal, HandoffError,
    JournalError, RehearsalError, RehearsalPhase, SnapshotError,
};
use grapt_types::{ClockAuthority, MigrationId, Timestamp};
use rusqlite::Connection;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Plugin-state creation witnesses (credential ownership and oracle `kv`).
#[path = "r6_migration/plugin_state.rs"]
mod plugin_state;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
struct FixedClock;

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_788_000_000_123)
    }
}

fn scratch(label: &str) -> TestResult<PathBuf> {
    let root = std::env::temp_dir().join(format!("grapt-r6mr-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

fn migration_id(value: &str) -> TestResult<MigrationId> {
    MigrationId::new(value).ok_or_else(|| "migration id must be non-empty".into())
}

fn legacy_database(path: &Path) -> TestResult {
    let connection = Connection::open(path)?;
    connection.execute_batch(
        "CREATE TABLE event_sequence (
            aggregate_id TEXT PRIMARY KEY,
            seq INTEGER NOT NULL,
            owner_id TEXT
        );
        CREATE TABLE event (
            id TEXT PRIMARY KEY,
            aggregate_id TEXT NOT NULL,
            seq INTEGER NOT NULL,
            created INTEGER NOT NULL,
            type TEXT NOT NULL,
            data TEXT NOT NULL,
            FOREIGN KEY (aggregate_id) REFERENCES event_sequence(aggregate_id) ON DELETE CASCADE
        );
        CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_id, seq);
        CREATE INDEX event_aggregate_type_seq_idx ON event (aggregate_id, type, seq);
        CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL);
        CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT NOT NULL);
        INSERT INTO event_sequence VALUES ('ses_r6', 4, NULL);
        INSERT INTO event VALUES
            ('evt_r6_1', 'ses_r6', 0, 1700000000000, 'session.created.1', '{\"directory\":\"C:/work\"}'),
            ('evt_r6_2', 'ses_r6', 4, 1700000000001, 'session.updated.1', '{\"title\":\"kept\"}');
        INSERT INTO kv VALUES ('theme', 'dark');
        INSERT INTO project VALUES ('prj_r6', 'migration');",
    )?;
    Ok(())
}

#[test]
fn serving_attestation_measures_the_open_store_and_rejects_changed_domain() -> TestResult {
    let root = scratch("serving-attestation")?;
    let source = root.join("source.sqlite3");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &root.join("run"),
        migration_id("serving")?,
        "serving-build",
        &FixedClock,
    )?;
    let eligibility = rehearsal.lowered_eligibility()?;
    let serving = root.join("serving.sqlite3");
    std::fs::copy(rehearsal.candidate(), &serving)?;
    let store = grapt_store::EventStore::open(&serving, "serving-build")?;
    // Different physical bytes with the same domain are expected after an
    // independent owned open; measure them instead of substituting the copy.
    store
        .connection()
        .execute_batch("PRAGMA application_id = 19301; PRAGMA wal_checkpoint(TRUNCATE);")?;
    let (_, observed) = fingerprint_file(&serving)?;
    assert_ne!(&observed, eligibility.candidate().file());
    let (measured, actual) = grapt_release::attest_serving_store(&eligibility, &store)?;
    assert_eq!(actual, observed);
    assert_eq!(measured.candidate().file(), &observed);
    store
        .connection()
        .execute("UPDATE kv SET value = 'lost' WHERE key = 'theme'", [])?;
    assert!(matches!(
        grapt_release::attest_serving_store(&eligibility, &store),
        Err(HandoffError::ServingDomainMismatch)
    ));
    drop(store);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn whole_rehearsal_is_idempotent_and_never_mutates_its_source() -> TestResult {
    let root = scratch("whole")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let source_before = fingerprint_file(&source)?;
    let receipt = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-whole")?,
        "0.1.0",
        &FixedClock,
    )?;
    assert_eq!(fingerprint_file(&source)?, source_before);
    assert_eq!(
        receipt.applied_migrations().len(),
        grapt_store::ProductionMigrations::load()?.as_slice().len()
    );
    let canonical_run_root = std::fs::canonicalize(&run_root)?;
    assert!(receipt.candidate().starts_with(&canonical_run_root));
    assert!(receipt.rollback().path().starts_with(&canonical_run_root));
    let source_version: i64 =
        Connection::open_with_flags(&source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let candidate_version: i64 = Connection::open_with_flags(
        receipt.candidate(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    let rollback_version: i64 = Connection::open_with_flags(
        receipt.rollback().path(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?
    .query_row("PRAGMA user_version", [], |row| row.get(0))?;
    assert_eq!(source_version, 0);
    assert_eq!(candidate_version, 2);
    assert_eq!(rollback_version, 0);
    let status = inspect_status(&run_root)?;
    assert_eq!(status.phase, Some(RehearsalPhase::ReadyForHandoff));
    assert_eq!(status.completed_checkpoints, 8);
    assert!(status.ready_for_handoff);

    let second = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-whole")?,
        "0.1.0",
        &FixedClock,
    )?;
    assert_eq!(
        second.candidate_file_fingerprint(),
        receipt.candidate_file_fingerprint()
    );
    assert_eq!(inspect_status(&run_root)?.completed_checkpoints, 8);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn every_material_checkpoint_resumes_with_effects_already_ahead() -> TestResult {
    let root = scratch("restart-grid")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let run_id = migration_id("r6mr-restart-grid")?;
    let _ = rehearse(&source, &run_root, run_id.clone(), "0.1.0", &FixedClock)?;
    let names = [
        "01-source-audited.json",
        "02-clone-created.json",
        "03-backup-verified.json",
        "04-migrated.json",
        "05-migration-compared.json",
        "06-rollback-restored.json",
        "07-rollback-compared.json",
        "08-ready-for-handoff.json",
    ];
    for retained in 1..names.len() {
        for name in &names[retained..] {
            let path = run_root.join("checkpoints").join(name);
            if path.exists() {
                std::fs::remove_file(path)?;
            }
        }
        let _ = rehearse(&source, &run_root, run_id.clone(), "0.1.0", &FixedClock)?;
        assert_eq!(
            inspect_status(&run_root)?.phase,
            Some(RehearsalPhase::ReadyForHandoff),
            "resume from durable phase {retained} did not close the journey"
        );
    }
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn interrupted_pending_publications_are_rebuilt_without_becoming_truth() -> TestResult {
    let root = scratch("pending")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    std::fs::create_dir_all(run_root.join("checkpoints"))?;
    std::fs::write(
        run_root.join("checkpoints/01-source-audited.json.pending"),
        b"torn-checkpoint",
    )?;
    std::fs::write(
        run_root.join("migration-candidate.sqlite3.pending"),
        b"torn-snapshot",
    )?;
    let receipt = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-pending")?,
        "0.2.0",
        &FixedClock,
    )?;
    assert!(receipt.candidate().is_file());
    assert!(!run_root
        .join("checkpoints/01-source-audited.json.pending")
        .exists());
    assert!(!run_root
        .join("migration-candidate.sqlite3.pending")
        .exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn source_aliasing_a_fixed_candidate_name_is_refused_before_mutation() -> TestResult {
    let root = scratch("alias")?;
    let source = root.join("migration-candidate.sqlite3");
    legacy_database(&source)?;
    let before = fingerprint_file(&source)?;
    let result = rehearse(
        &source,
        &root,
        migration_id("r6mr-alias")?,
        "0.2.0",
        &FixedClock,
    );
    assert!(matches!(
        result,
        Err(RehearsalError::SourceArtifactAlias(_))
    ));
    assert_eq!(fingerprint_file(&source)?, before);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn candidate_drift_after_a_migrated_checkpoint_refuses_resurrection() -> TestResult {
    let root = scratch("candidate-drift")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let run_id = migration_id("r6mr-candidate-drift")?;
    let receipt = rehearse(&source, &run_root, run_id.clone(), "0.2.0", &FixedClock)?;
    Connection::open(receipt.candidate())?
        .execute("DELETE FROM event WHERE id = 'evt_r6_2'", [])?;
    let outcome = rehearse(&source, &run_root, run_id, "0.2.0", &FixedClock);
    assert!(
        matches!(outcome, Err(RehearsalError::CandidateChanged)),
        "candidate drift returned {outcome:?}"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn altered_checkpoint_bytes_break_the_hash_chain_even_when_json_remains_valid() -> TestResult {
    let root = scratch("checkpoint-chain")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let run_id = migration_id("r6mr-checkpoint-chain")?;
    let _ = rehearse(&source, &run_root, run_id.clone(), "0.2.0", &FixedClock)?;
    let first = run_root.join("checkpoints/01-source-audited.json");
    let mut bytes = std::fs::read(&first)?;
    bytes.push(b' ');
    std::fs::write(first, bytes)?;
    let outcome = rehearse(&source, &run_root, run_id, "0.2.0", &FixedClock);
    assert!(matches!(
        outcome,
        Err(RehearsalError::Journal(JournalError::BrokenChain { .. }))
    ));
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn absent_and_corrupt_backups_refuse_loudly() -> TestResult {
    let root = scratch("backup-refusals")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let run_id = migration_id("r6mr-backup-refusals")?;
    let _ = rehearse(&source, &run_root, run_id.clone(), "0.1.0", &FixedClock)?;
    let backup = run_root.join("pre-mutation-rollback.sqlite3");
    std::fs::remove_file(&backup)?;
    assert!(matches!(
        rehearse(&source, &run_root, run_id.clone(), "0.1.0", &FixedClock),
        Err(RehearsalError::Snapshot(SnapshotError::SourceAbsent(path)))
            if path.file_name() == backup.file_name()
    ));

    let corrupt_root = root.join("corrupt-run");
    let _ = rehearse(
        &source,
        &corrupt_root,
        migration_id("r6mr-corrupt-backup")?,
        "0.1.0",
        &FixedClock,
    )?;
    let corrupt = corrupt_root.join("pre-mutation-rollback.sqlite3");
    std::fs::write(&corrupt, b"not a sqlite backup")?;
    assert!(rehearse(
        &source,
        &corrupt_root,
        migration_id("r6mr-corrupt-backup")?,
        "0.1.0",
        &FixedClock,
    )
    .is_err());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn rollback_recovers_a_new_database_after_partial_candidate_damage() -> TestResult {
    let root = scratch("partial-rollback")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let receipt = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-partial-rollback")?,
        "0.1.0",
        &FixedClock,
    )?;
    let partial = root.join("partially-mutated.sqlite3");
    std::fs::copy(receipt.candidate(), &partial)?;
    Connection::open(&partial)?.execute("DELETE FROM event WHERE id = 'evt_r6_2'", [])?;
    let restored = root.join("restored-after-partial.sqlite3");
    let rollback = restore_backup_rehearsal(
        &source,
        &run_root.join("pre-mutation-rollback.sqlite3"),
        &restored,
    )?;
    assert_eq!(
        rollback.domain_fingerprint(),
        receipt.source_domain_fingerprint()
    );
    let partial_events: i64 =
        Connection::open_with_flags(partial, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    let restored_events: i64 =
        Connection::open_with_flags(restored, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?
            .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(partial_events, 1);
    assert_eq!(restored_events, 2);
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn eligibility_handoff_contains_only_database_evidence() -> TestResult {
    let root = scratch("handoff")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-handoff")?,
        "0.1.0",
        &FixedClock,
    )?;
    let receipt = rehearsal.eligibility_receipt()?;
    assert_eq!(receipt.outcome, "eligible-for-unified-cutover-review");
    let encoded = serde_json::to_value(receipt)?;
    let object = encoded
        .as_object()
        .ok_or("handoff must serialize as an object")?;
    for forbidden in [
        "retirement",
        "service_owner",
        "cutover",
        "committed",
        "election",
        "registration",
    ] {
        assert!(
            !object.contains_key(forbidden),
            "migration handoff leaked cutover authority field {forbidden:?}"
        );
    }
    assert_eq!(
        inspect_status(&run_root)?.phase,
        Some(RehearsalPhase::ReadyForHandoff)
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn former_cutover_cli_is_refused_before_fabricated_retirement_can_write() -> TestResult {
    let root = scratch("cutover-cli-refusal")?;
    let registration = root.join("service.registration");
    let output = Command::new(env!("CARGO_BIN_EXE_grapt-migrate"))
        .args([
            "cutover",
            "--retirement",
            "retired",
            "--registration",
            registration
                .to_str()
                .ok_or("registration path is not UTF-8")?,
        ])
        .output()?;
    assert_eq!(output.status.code(), Some(2));
    let rendered: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(
        rendered["reason"],
        "cutover is owned by the unified R6 release authority; migration operator cannot commit it"
    );
    assert!(!registration.exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn hardlinked_candidate_is_refused_before_checkpoint_or_source_mutation() -> TestResult {
    let root = scratch("hardlink-candidate")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    std::fs::create_dir_all(&run_root)?;
    legacy_database(&source)?;
    let candidate = run_root.join("migration-candidate.sqlite3");
    std::fs::hard_link(&source, &candidate)?;
    let source_before = fingerprint_file(&source)?;
    let outcome = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-hardlink-candidate")?,
        "0.1.0",
        &FixedClock,
    );
    assert!(matches!(
        outcome,
        Err(RehearsalError::Snapshot(
            SnapshotError::PhysicalAlias { .. }
        ))
    ));
    assert_eq!(fingerprint_file(&source)?, source_before);
    assert!(!run_root.join("checkpoints").exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn unreadable_candidate_identity_is_not_treated_as_absent() -> TestResult {
    let root = scratch("unreadable-candidate")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    std::fs::create_dir_all(run_root.join("migration-candidate.sqlite3"))?;
    legacy_database(&source)?;
    let outcome = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-unreadable-candidate")?,
        "0.1.0",
        &FixedClock,
    );
    assert!(matches!(
        outcome,
        Err(RehearsalError::Snapshot(
            SnapshotError::PhysicalIdentityUnreadable { .. }
        ))
    ));
    assert!(!run_root.join("checkpoints").exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[cfg(unix)]
#[test]
fn dangling_symlink_identity_is_unreadable_not_absent() -> TestResult {
    use std::os::unix::fs::symlink;

    let root = scratch("dangling-candidate")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    std::fs::create_dir_all(&run_root)?;
    legacy_database(&source)?;
    symlink(
        root.join("absent-target.sqlite3"),
        run_root.join("migration-candidate.sqlite3"),
    )?;
    let outcome = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-dangling-candidate")?,
        "0.1.0",
        &FixedClock,
    );
    assert!(matches!(
        outcome,
        Err(RehearsalError::Snapshot(
            SnapshotError::PhysicalIdentityUnreadable { .. }
        ))
    ));
    assert!(!run_root.join("checkpoints").exists());
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn standalone_restore_refuses_physical_aliases() -> TestResult {
    let root = scratch("hardlink-restore")?;
    let source = root.join("source.sqlite3");
    legacy_database(&source)?;
    let aliased_backup = root.join("aliased-backup.sqlite3");
    std::fs::hard_link(&source, &aliased_backup)?;
    let destination = root.join("restored.sqlite3");
    assert!(matches!(
        restore_backup_rehearsal(&source, &aliased_backup, &destination),
        Err(RehearsalError::Snapshot(
            SnapshotError::PhysicalAlias { .. }
        ))
    ));
    assert!(!destination.exists());

    let distinct_backup = root.join("distinct-backup.sqlite3");
    std::fs::copy(&source, &distinct_backup)?;
    std::fs::hard_link(&distinct_backup, &destination)?;
    assert!(matches!(
        restore_backup_rehearsal(&source, &distinct_backup, &destination),
        Err(RehearsalError::Snapshot(
            SnapshotError::PhysicalAlias { .. }
        ))
    ));
    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// The join between this crate's evidence and the transition owner that
/// consumes it.
///
/// `eligibility_receipt` renders JSON for an operator; `lowered_eligibility`
/// renders the typed value `grapt-service` gates on. This asserts a REAL
/// rehearsal — real SQLite files, real migration, real backup restore —
/// produces evidence that satisfies the lowered form's own law, so the two
/// halves of the transition are joined by a value neither half hand-built.
#[test]
fn a_real_rehearsal_lowers_to_the_evidence_the_transition_owner_requires() -> TestResult {
    let root = scratch("lowered")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-lowered")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;

    let lowered = rehearsal.lowered_eligibility()?;
    let rendered = rehearsal.eligibility_receipt()?;

    // The lowered value and the operator's JSON describe ONE rehearsal, not two
    // independently derived opinions of it.
    assert_eq!(lowered.run().as_str(), rendered.run_id);
    assert_eq!(
        lowered.source().file().as_str(),
        rendered.source_fingerprint
    );
    assert_eq!(
        lowered.candidate().file().as_str(),
        rendered.candidate_fingerprint
    );
    assert_eq!(
        lowered.rollback().file().as_str(),
        rendered.rollback_fingerprint
    );
    assert_eq!(lowered.applied().len(), rendered.applied_migrations.len());

    // The constructor law held on real data rather than on a fixture: the
    // restored backup reproduces the source's domain, which is what makes the
    // word "rollback" true.
    assert_eq!(
        lowered.rollback().domain(),
        lowered.source().domain(),
        "a real rollback must reproduce the source domain"
    );

    // The activation baseline exists and is genuinely the MIGRATED state, not a
    // copy of the pre-migration domain. Without this a post-activation
    // comparison would be measuring the wrong baseline.
    let baseline = lowered
        .candidate()
        .domain()
        .ok_or("the candidate must carry an activation baseline")?;
    assert_ne!(
        Some(baseline),
        lowered.source().domain(),
        "the migrated candidate's domain must differ from the pre-migration domain"
    );

    // And it is about THIS database.
    assert!(lowered.describes_source(lowered.source().file()));

    // The baseline must be DETERMINISTIC, or comparing against it later would
    // report work that never happened. A second independent rehearsal of the
    // same source must derive the identical candidate domain. This is why the
    // canonical export covers a closed list of domain tables and excludes the
    // attribution surfaces that carry wall-clock values (`engine_epoch`, the
    // migration journal) — asserted here rather than trusted from that reading.
    let second_root = root.join("run-2");
    let second = rehearse(
        &source,
        &second_root,
        migration_id("r6mr-lowered-2")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;
    assert_eq!(
        second.lowered_eligibility()?.candidate().domain(),
        lowered.candidate().domain(),
        "the activation baseline must not depend on when the rehearsal ran"
    );

    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// J04: the delivered operator must be able to say what a rollback costs.
///
/// Both halves run through the SHIPPED BINARY rather than the library, because
/// the operator this exists for has an installed artifact and a backup — not a
/// checkout. A recovery path reachable only from `cargo` is not a recovery path.
#[test]
fn the_operator_reports_a_clean_rollback_and_then_refuses_to_call_a_lossy_one_clean() -> TestResult
{
    let root = scratch("disposition")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;

    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-disposition")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;
    let served = rehearsal.candidate().to_path_buf();

    // Nothing has happened since activation: the disposition must say the
    // rollback resumes everything.
    let clean = disposition_of(&run_root, &served)?;
    assert_eq!(clean["schema"], "grapt.release.recovery-disposition.v1");
    assert_eq!(clean["outcome"], "rollback-restores-everything");
    assert_eq!(clean["restores_everything"], true);
    assert!(
        clean.get("observed_domain_fingerprint").is_none(),
        "a clean rollback has no discarded work to name: {clean}"
    );

    // Now the engine does real work after activation: one durably appended
    // event on an existing aggregate, carrying its elected writer epoch exactly
    // as a live Rust engine's append would. This is the work a rollback to the
    // pre-cutover backup cannot resume.
    let connection = Connection::open(&served)?;
    connection.execute(
        "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type, data, writer_epoch)
         VALUES ('evt_after_activation', 'session', 'ses_r6', 5, 1700000000002,
                 'session.updated.1', '{\"title\":\"written after cutover\"}', 1)",
        [],
    )?;
    drop(connection);

    let lossy = disposition_of(&run_root, &served)?;
    assert_eq!(lossy["outcome"], "rollback-discards-post-activation-work");
    assert_eq!(
        lossy["restores_everything"], false,
        "restoring a pre-cutover backup over later work is not lossless"
    );
    // It must name the baseline, what is there now, and where the work it does
    // not resume continues to live.
    assert!(lossy["activation_baseline"].is_string());
    assert!(lossy["observed_domain_fingerprint"].is_string());
    assert_ne!(
        lossy["activation_baseline"],
        lossy["observed_domain_fingerprint"]
    );
    assert!(lossy["not_resumed_preserved_at"]
        .as_str()
        .is_some_and(|preserved| !preserved.is_empty() && preserved != "unresolvable"));
    // The rollback still recovers to the pre-migration state, and says so.
    assert_eq!(
        lossy["restored_domain_fingerprint"],
        clean["restored_domain_fingerprint"]
    );

    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// The executable rollback, driven from the shipped binary, never overwrites
/// what is in service: it produces a NEW restored artifact beside it.
#[test]
fn the_operator_restores_a_backup_beside_the_served_database_without_overwriting_it() -> TestResult
{
    let root = scratch("operator-restore")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-operator-restore")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;
    let backup = rehearsal.rollback().path().to_path_buf();
    let served = rehearsal.candidate().to_path_buf();
    let served_before = std::fs::read(&served)?;
    let restored = root.join("restored.sqlite3");

    let output = Command::new(env!("CARGO_BIN_EXE_grapt-migrate"))
        .args([
            "restore",
            "--source",
            &source.display().to_string(),
            "--backup",
            &backup.display().to_string(),
            "--restored",
            &restored.display().to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "restore refused: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    let report: serde_json::Value = serde_json::from_slice(&trimmed(&output.stdout))?;
    assert_eq!(report["schema"], "grapt.release.rollback-restore.v1");
    assert_eq!(report["outcome"], "restored");

    assert!(restored.is_file(), "the restore must produce a database");
    assert_eq!(
        std::fs::read(&served)?,
        served_before,
        "a rollback must never overwrite the database in service"
    );
    std::fs::remove_dir_all(root)?;
    Ok(())
}

fn trimmed(bytes: &[u8]) -> Vec<u8> {
    let text = String::from_utf8_lossy(bytes);
    text.trim().as_bytes().to_vec()
}

fn disposition_of(run_root: &Path, served: &Path) -> TestResult<serde_json::Value> {
    let output = Command::new(env!("CARGO_BIN_EXE_grapt-migrate"))
        .args([
            "disposition",
            "--run-root",
            &run_root.display().to_string(),
            "--served",
            &served.display().to_string(),
        ])
        .output()?;
    assert!(
        output.status.success(),
        "disposition refused: {}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(serde_json::from_slice(&trimmed(&output.stdout))?)
}

/// J03's OTHER window: a source that moved after it was audited.
///
/// The migrated-database trigger (`event_refuses_new_unfenced_write`) stops a
/// TypeScript engine writing into a database Rust already owns. It says nothing
/// about the window BEFORE activation, where the incumbent is still live and
/// still writing to its own oracle-era database while a rehearsal of that database
/// is in hand. A receipt minted from a stale audit would offer a candidate that
/// silently omits every such write.
///
/// So eligibility revalidates the source against the audit before it hands
/// anything over, and refuses as `RehearsalDrift` when the source has moved.
/// Both the operator's JSON and the lowered typed value run that identical
/// revalidation, so neither is a cheaper way to obtain a stale fact.
#[test]
fn a_source_written_after_its_audit_refuses_to_yield_eligibility() -> TestResult {
    let root = scratch("stale-source")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-stale-source")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;

    // The rehearsal is complete and both forms are available right now.
    rehearsal.eligibility_receipt()?;
    rehearsal.lowered_eligibility()?;

    // The still-live incumbent writes one more event to its own database --
    // exactly what happens if the engine is not quiesced before cutover.
    let connection = Connection::open(&source)?;
    connection.execute(
        "INSERT INTO event VALUES ('evt_r6_3', 'ses_r6', 5, 1700000000002,
         'session.updated.1', '{\"title\":\"written after the audit\"}')",
        [],
    )?;
    drop(connection);

    // Neither form will now hand over evidence, and both refuse by the same
    // named fact rather than one of them quietly succeeding.
    assert!(
        matches!(
            rehearsal.eligibility_receipt(),
            Err(HandoffError::RehearsalDrift)
        ),
        "a receipt minted from a stale audit would omit the write that just happened"
    );
    assert!(matches!(
        rehearsal.lowered_eligibility(),
        Err(HandoffError::RehearsalDrift)
    ));

    std::fs::remove_dir_all(root)?;
    Ok(())
}

/// The mint -> commit race, demonstrated in an isolated installation with real
/// files rather than argued about.
///
/// `a_source_written_after_its_audit_refuses_to_yield_eligibility` covers the
/// case where the write lands BEFORE evidence is taken. This covers the harder
/// one: evidence is already in hand and valid, and the write lands after it.
/// Nothing about the receipt changes — it stays internally consistent — so the
/// only thing that can catch it is observing the source AGAIN at activation and
/// comparing against what was audited.
///
/// That fresh observation is exactly what `QuiescedSource` carries into
/// `grapt-service::prepare_database_transition`, which refuses the mismatch
/// (`a_source_written_between_final_validation_and_activation_refuses_the_transition`).
/// This half proves the observation genuinely moves; that half proves the
/// transition refuses when it has. The two are joined by one typed value.
#[test]
fn a_write_after_valid_eligibility_moves_the_source_observation_the_transition_checks() -> TestResult
{
    let root = scratch("mint-commit-race")?;
    let source = root.join("source.sqlite3");
    let run_root = root.join("run");
    legacy_database(&source)?;
    let rehearsal = rehearse(
        &source,
        &run_root,
        migration_id("r6mr-mint-commit-race")?,
        "secode-rust@40d82e39",
        &FixedClock,
    )?;

    // Final validation succeeds and the operator now holds valid evidence.
    let eligibility = rehearsal.lowered_eligibility()?;

    // With the incumbent quiesced, re-observing the source at activation agrees
    // with the audit. Asserted so the check below cannot pass vacuously.
    let (_, quiesced_observation) = fingerprint_file(&source)?;
    assert!(
        eligibility.describes_source(&quiesced_observation),
        "a source that held still must still match its audit"
    );

    // The incumbent was NOT quiesced and writes one more event to the source
    // after the evidence was taken. The candidate is a copy made earlier, so it
    // does not contain this row.
    let connection = Connection::open(&source)?;
    connection.execute(
        "INSERT INTO event VALUES ('evt_r6_race', 'ses_r6', 6, 1700000000003,
         'session.updated.1', '{\"title\":\"written after validation\"}')",
        [],
    )?;
    drop(connection);

    // The receipt is UNCHANGED and still internally consistent -- it is the
    // world that moved, which is why re-observing is the only thing that helps.
    let (_, activation_observation) = fingerprint_file(&source)?;
    assert_ne!(
        activation_observation, quiesced_observation,
        "the incumbent's write must move the source's physical fingerprint"
    );
    assert!(
        !eligibility.describes_source(&activation_observation),
        "re-observing the source at activation is what catches a write that \
         landed after valid evidence was already in hand"
    );

    std::fs::remove_dir_all(root)?;
    Ok(())
}
