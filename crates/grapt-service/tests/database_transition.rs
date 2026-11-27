//! R6 requirement 7: cutover is ONE elected service transition, with a
//! truthful rollback outcome.
//!
//! These run against a real registration file, a real OS file lock and the real
//! compare-and-swap — an isolated installation per test, under the system temp
//! directory. The controller's own election properties (stale owner, PID reuse,
//! concurrent starters, a failed candidate) are already witnessed in
//! `registration_durability.rs` and `controller_contract/`; this battery does
//! not restate them. What is witnessed here is the fact those batteries cannot
//! express, because the controller had no notion of a database: that an
//! activation is gated on evidence about *the database it will actually serve*,
//! and that the rollback which follows describes itself honestly.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use grapt_service::{
    prepare_database_transition, DatabaseTransitionRefusal, ElectedControllerAuthority,
    LockAttemptPolicy, Observation, ProbeOutcome, QuiescedSource, Retirement, ServiceController,
    SlotState,
};
use grapt_types::authority::{ClockAuthority, Timestamp};
use grapt_types::path::PathIdentity;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ReplacementOutcome, ServiceGeneration,
    ServiceInstanceId, ServiceRegistrationVersion, ServiceSlotId,
};
use grapt_types::{
    DatabaseEvidence, DatabaseFingerprint, MigrationEligibility, MigrationId, RecoveryDisposition,
};

type R = Result<(), Box<dyn std::error::Error>>;

static SCRATCH: AtomicU64 = AtomicU64::new(0);

const SLOT: &str = "slot-secode";
const ENVIRONMENT: &str = "env-alpha";

/// Fingerprints for the three databases a rehearsal produces, plus one that
/// belongs to a different installation entirely.
const SOURCE_FILE: &str = "source-bytes-ts-era";
const CANDIDATE_FILE: &str = "candidate-bytes-migrated";
const ROLLBACK_FILE: &str = "rollback-bytes-restored";
const FOREIGN_FILE: &str = "another-installations-database";
const PRE_MIGRATION_DOMAIN: &str = "domain-before-migration";
const ACTIVATION_BASELINE: &str = "domain-at-activation";

#[derive(Debug)]
struct FixedClock(i64);

impl ClockAuthority for FixedClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

/// An isolated installation that removes itself.
#[derive(Debug)]
struct Scratch {
    root: PathBuf,
}

impl Scratch {
    fn new(name: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let ordinal = SCRATCH.fetch_add(1, Ordering::Relaxed);
        let root = std::env::temp_dir().join(format!(
            "grapt-service-mxt-{}-{ordinal}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root)?;
        Ok(Self { root })
    }

    fn slot_path(&self) -> PathBuf {
        self.root.join("registration")
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn fingerprint(value: &str) -> Result<DatabaseFingerprint, Box<dyn std::error::Error>> {
    DatabaseFingerprint::new(value).ok_or_else(|| format!("fingerprint {value}").into())
}

fn evidence(
    file: &str,
    domain: Option<&str>,
) -> Result<DatabaseEvidence, Box<dyn std::error::Error>> {
    let domain = match domain {
        Some(domain) => Some(fingerprint(domain)?),
        None => None,
    };
    Ok(DatabaseEvidence::new(fingerprint(file)?, domain))
}

/// The evidence a completed rehearsal produces: the rollback reproduces the
/// source's domain, and the candidate carries the activation baseline.
fn rehearsed() -> Result<MigrationEligibility, Box<dyn std::error::Error>> {
    MigrationEligibility::new(
        MigrationId::new("mxt-run-1").ok_or("run id")?,
        evidence(SOURCE_FILE, Some(PRE_MIGRATION_DOMAIN))?,
        evidence(CANDIDATE_FILE, Some(ACTIVATION_BASELINE))?,
        evidence(ROLLBACK_FILE, Some(PRE_MIGRATION_DOMAIN))?,
        vec![MigrationId::new("20260811_rebuild_event_tables_family_tagged").ok_or("migration")?],
    )
    .ok_or_else(|| "a completed rehearsal must lower to eligibility".into())
}

fn owner(
    instance: &str,
    generation: u64,
    pid: u32,
) -> Result<OwnerReceipt, Box<dyn std::error::Error>> {
    Ok(OwnerReceipt::new(
        ServiceSlotId::new(SLOT).ok_or("slot")?,
        ServiceInstanceId::new(instance).ok_or("instance")?,
        EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
        ServiceGeneration::new(generation),
        ProcessId::new(pid).ok_or("pid")?,
        Endpoint::new("http://127.0.0.1:4096".to_owned()).ok_or("endpoint")?,
    ))
}

/// The ordinary case: the incumbent retired and the source still reads exactly
/// as the rehearsal audited it.
fn quiesced(observed: &str) -> Result<QuiescedSource, Box<dyn std::error::Error>> {
    Ok(QuiescedSource::observed_after(
        &Retirement::Retired,
        fingerprint(observed)?,
    )?)
}

fn observation(declared: OwnerReceipt) -> Result<Observation, Box<dyn std::error::Error>> {
    Ok(Observation::new(
        declared.clone(),
        ProbeOutcome::Attributed(declared),
        ServiceRegistrationVersion::new("0.1.0").ok_or("version")?,
    ))
}

fn authority(path: PathBuf) -> Result<ElectedControllerAuthority, Box<dyn std::error::Error>> {
    ElectedControllerAuthority::acquire(
        path,
        LockAttemptPolicy::new(1_000_000).ok_or("nonzero lock policy")?,
    )
    .map_err(Into::into)
}

fn controller<'a>(
    store: &'a ElectedControllerAuthority,
    clock: &'a FixedClock,
) -> Result<ServiceController<'a>, Box<dyn std::error::Error>> {
    Ok(ServiceController::new(
        ServiceSlotId::new(SLOT).ok_or("slot")?,
        EnvironmentReceipt::new(ENVIRONMENT).ok_or("environment")?,
        store,
        clock,
    ))
}

#[test]
fn a_transition_serving_the_proven_candidate_elects_through_the_real_compare_and_swap() -> R {
    let scratch = Scratch::new("admitted")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_787_595_200_123);
    let controller = controller(&store, &clock)?;

    let discovered = controller.discover()?;
    assert!(
        matches!(discovered.state(), SlotState::Empty),
        "an isolated installation starts with an unheld slot"
    );

    let declared = owner("inst-rust-1", 0, 4321)?;
    let prepared = prepare_database_transition(
        &controller,
        &discovered,
        &observation(declared.clone())?,
        &fingerprint(CANDIDATE_FILE)?,
        &quiesced(SOURCE_FILE)?,
        &rehearsed()?,
    )?;
    let switch = controller.commit(prepared, Retirement::NoIncumbent)?;

    assert_eq!(switch.outcome(), ReplacementOutcome::Replaced);
    assert_eq!(switch.committed_owner(), Some(&declared));
    // The election is the controller's, committed to a real file. The gate did
    // not build a second registration path to get here.
    assert!(
        scratch.slot_path().is_file(),
        "the transition must commit through the registration the controller owns"
    );
    Ok(())
}

#[test]
fn evidence_about_another_database_is_refused_before_anything_is_written() -> R {
    let scratch = Scratch::new("foreign-evidence")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_787_595_200_123);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    let refusal = prepare_database_transition(
        &controller,
        &discovered,
        &observation(owner("inst-rust-1", 0, 4321)?)?,
        // The engine is about to serve a database from a different installation.
        // The receipt is genuine; it is simply not about THIS database.
        &fingerprint(FOREIGN_FILE)?,
        &quiesced(SOURCE_FILE)?,
        &rehearsed()?,
    )
    .err()
    .ok_or("a receipt about another database must not admit a transition")?;

    assert!(matches!(
        refusal,
        DatabaseTransitionRefusal::ServingDatabaseIsNotTheProvenCandidate { .. }
    ));
    assert_eq!(refusal.label(), "transition-serving-database-unproven");
    let rendered = refusal.to_string();
    assert!(
        rendered.contains(FOREIGN_FILE) && rendered.contains(CANDIDATE_FILE),
        "the refusal must name both databases so an operator can see the mismatch: {rendered}"
    );

    // The point of refusing in `prepare` rather than at commit: no CAS ran, so
    // the slot is untouched rather than cleaned up afterwards.
    assert!(
        !scratch.slot_path().exists(),
        "a refused transition must not have written a registration"
    );
    Ok(())
}

#[test]
fn serving_the_unmigrated_source_is_refused_even_though_the_receipt_is_genuine() -> R {
    let scratch = Scratch::new("source-not-candidate")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_787_595_200_123);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    // The subtle one. This receipt IS about this installation — the source
    // fingerprint is its own. But the source is the pre-migration database, so
    // activating against it would serve unmigrated data under an engine that
    // believes the migration happened.
    let refusal = prepare_database_transition(
        &controller,
        &discovered,
        &observation(owner("inst-rust-1", 0, 4321)?)?,
        &fingerprint(SOURCE_FILE)?,
        &quiesced(SOURCE_FILE)?,
        &rehearsed()?,
    )
    .err()
    .ok_or("the pre-migration source is not an activatable database")?;

    assert!(matches!(
        refusal,
        DatabaseTransitionRefusal::ServingDatabaseIsNotTheProvenCandidate { .. }
    ));
    assert!(!scratch.slot_path().exists());
    Ok(())
}

#[test]
fn a_rollback_with_no_work_after_activation_restores_everything() -> R {
    let eligibility = rehearsed()?;
    let disposition = eligibility
        .recovery_disposition(
            // Nothing was written since activation: the domain is still the baseline.
            &fingerprint(ACTIVATION_BASELINE)?,
            PathIdentity::canonical("/install/run/post-activation.sqlite3"),
        )
        .ok_or("measured evidence must classify")?;

    assert!(disposition.is_lossless());
    match disposition {
        RecoveryDisposition::RestoresEverything { restored_domain } => {
            assert_eq!(restored_domain.as_str(), PRE_MIGRATION_DOMAIN);
        }
        other => return Err(format!("unexpected disposition {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_rollback_after_real_work_names_what_it_discards_and_is_never_called_lossless() -> R {
    let eligibility = rehearsed()?;
    let observed = fingerprint("domain-with-work-done-after-activation")?;
    let preserved = PathIdentity::canonical("/install/run/post-activation.sqlite3");

    let disposition = eligibility
        .recovery_disposition(&observed, preserved.clone())
        .ok_or("measured evidence must classify")?;

    assert!(
        !disposition.is_lossless(),
        "restoring a pre-cutover backup over later work is not lossless"
    );
    match disposition {
        RecoveryDisposition::DiscardsPostActivationWork {
            restored_domain,
            activation_baseline,
            observed_domain,
            preserved_at,
        } => {
            // It still recovers — to the pre-migration state, which is the only
            // state a backup can carry.
            assert_eq!(restored_domain.as_str(), PRE_MIGRATION_DOMAIN);
            assert_eq!(activation_baseline.as_str(), ACTIVATION_BASELINE);
            // And it says exactly what is not resumed, and where that work
            // still lives, rather than reporting a clean recovery.
            assert_eq!(observed_domain, observed);
            assert_eq!(preserved_at, preserved);
        }
        other => return Err(format!("unexpected disposition {other:?}").into()),
    }
    Ok(())
}

#[test]
fn a_source_written_between_final_validation_and_activation_refuses_the_transition() -> R {
    let scratch = Scratch::new("source-moved")?;
    let store = authority(scratch.slot_path())?;
    let clock = FixedClock(1_787_595_200_123);
    let controller = controller(&store, &clock)?;
    let discovered = controller.discover()?;

    // The rehearsal audited SOURCE_FILE. The incumbent then wrote to its own
    // database before the switch committed, so the source no longer reads as
    // audited -- and the migrated candidate does not contain those writes.
    let moved = QuiescedSource::observed_after(
        &Retirement::Retired,
        fingerprint("source-bytes-with-a-later-incumbent-write")?,
    )?;

    let refusal = prepare_database_transition(
        &controller,
        &discovered,
        &observation(owner("inst-rust-1", 0, 4321)?)?,
        // Everything else is correct: this IS the proven candidate.
        &fingerprint(CANDIDATE_FILE)?,
        &moved,
        &rehearsed()?,
    )
    .err()
    .ok_or("a source that moved after its audit must not authorise activation")?;

    assert!(matches!(
        refusal,
        DatabaseTransitionRefusal::SourceMovedSinceItWasAudited { .. }
    ));
    assert_eq!(refusal.label(), "transition-source-moved-since-audit");
    // This loss would never appear in a rollback disposition: it happens AT
    // activation, so the refusal is the only thing that can report it.
    assert!(
        refusal.to_string().contains("discard them silently"),
        "the refusal must say what activating anyway would cost: {refusal}"
    );
    assert!(
        !scratch.slot_path().exists(),
        "a refused transition must not have written a registration"
    );
    Ok(())
}

#[test]
fn an_incumbent_that_would_not_retire_cannot_produce_a_quiesced_source_at_all() -> R {
    // The dangerous case is not "the source moved" -- it is "we cannot know
    // whether it moved", because something that refuses to stop is exactly what
    // may write next. Both non-retiring outcomes are refused by name, and the
    // proof cannot be constructed, so no transition can be attempted with one.
    for (retirement, expected) in [
        (Retirement::TimedOut, "timed-out"),
        (
            Retirement::Failed {
                reason: "sigterm ignored".to_owned(),
            },
            "failed",
        ),
    ] {
        let refusal = QuiescedSource::observed_after(&retirement, fingerprint(SOURCE_FILE)?)
            .err()
            .ok_or("a live incumbent cannot be treated as quiesced")?;
        assert!(matches!(
            refusal,
            DatabaseTransitionRefusal::IncumbentMayStillBeWriting { .. }
        ));
        assert_eq!(refusal.label(), "transition-incumbent-may-still-be-writing");
        assert!(
            refusal.to_string().contains(expected),
            "the refusal must name which retirement outcome it saw: {refusal}"
        );
    }

    // And the admitting outcomes really do admit, so this is not passing by
    // refusing everything.
    for retirement in [Retirement::Retired, Retirement::NoIncumbent] {
        assert!(QuiescedSource::observed_after(&retirement, fingerprint(SOURCE_FILE)?).is_ok());
    }
    Ok(())
}
