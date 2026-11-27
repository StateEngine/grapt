//! Explicit handover joins release evidence to the existing service election.
//!
//! Rehearse, observe incumbent retirement, acquire election custody, freshly
//! fingerprint the source, open and measure the served store, authenticate the
//! candidate, then commit through the controller CAS. Election custody spans
//! source validation through commit. Ordinary startup acquires the same election
//! before store effects so a concurrent starter cannot bypass writer exclusion.
//!
//! The source read stays after observed retirement. SHV09 hoists that actual read
//! above the wait; the live-writer journey must reject the resulting stale proof.
//! Release owns migration evidence, platform owns the store, and service owns
//! transition authority. This operation adds no parallel manager or migration.

use std::path::{Path, PathBuf};

use grapt_release::{fingerprint_file, rehearse};
use grapt_service::{
    prepare_database_transition, DatabaseTransitionRefusal, ElectedControllerAuthority,
    LockAttemptPolicy, Observation, ProbeOutcome, QuiescedSource, RegistrationRecord, Retirement,
    ServiceController, SwitchReceipt,
};
use grapt_store::SystemClock;
use grapt_types::service::{
    OwnerReceipt, ReplacementOutcome, ServiceInstanceId, ServiceRegistrationVersion,
};
use grapt_types::{DatabaseFingerprint, MigrationEligibility, MigrationId};

use crate::compose::ProductRefusal;

#[derive(Clone, Copy)]
pub(crate) enum CrashBoundary {
    BeforeRetirement,
    AfterRetirement,
    BeforeCommit,
    AfterCommit,
    Recovery,
}

/// Fixture builds terminate without destructors at a named boundary. The
/// environment selector and exit call do not exist in an ordinary build.
pub(crate) fn crash_boundary(boundary: CrashBoundary) {
    #[cfg(feature = "handover-crash-fixture")]
    {
        let name = match boundary {
            CrashBoundary::BeforeRetirement => "before-retirement",
            CrashBoundary::AfterRetirement => "after-retirement",
            CrashBoundary::BeforeCommit => "before-commit",
            CrashBoundary::AfterCommit => "after-commit",
            CrashBoundary::Recovery => "recovery",
        };
        if std::env::var_os("GRAPT_HANDOVER_CRASH_AT").as_deref()
            == Some(std::ffi::OsStr::new(name))
        {
            std::process::exit(86);
        }
    }
    #[cfg(not(feature = "handover-crash-fixture"))]
    let _ = boundary;
}

/// Everything the operation needs, supplied by the caller that owns the paths.
///
/// The source fingerprint is deliberately ABSENT from this struct. It is not an
/// input; it is something this operation observes for itself at the one moment
/// the observation is meaningful.
#[derive(Debug, Clone)]
pub(crate) struct HandoverRequest {
    /// The isolated installation whose registration slot is being handed over.
    pub(crate) service_root: PathBuf,
    /// The rehearsal run root holding the journal, candidate and backup.
    pub(crate) run_root: PathBuf,
    /// The pre-migration database the rehearsal audited.
    pub(crate) source: PathBuf,
    pub(crate) run_id: MigrationId,
    /// The attested build identity. Stamped durably on first Rust write
    /// contact, so it is the binary's source commit and never a crate version.
    pub(crate) engine_version: String,
    /// How many times to look for the incumbent's absence before calling it
    /// TimedOut. Bounded by construction: this operation never waits forever
    /// for a process that will not go away.
    pub(crate) retirement_attempts: u32,
    pub(crate) retirement_interval: std::time::Duration,
}

/// What one completed handover proves.
#[derive(Debug)]
pub(crate) struct HandoverReceipt {
    pub(crate) switch: SwitchReceipt,
    /// What actually happened to the incumbent.
    pub(crate) retirement: Retirement,
    /// The database the successor is entitled to serve.
    pub(crate) serving: DatabaseFingerprint,
    /// The source as observed AFTER retirement — the value the transition was
    /// actually validated against, recorded so an operator can see it rather
    /// than trust that it happened.
    pub(crate) observed_source: DatabaseFingerprint,
}

impl HandoverReceipt {
    /// Whether an incumbent was actually retired, as opposed to the slot having
    /// been empty. Both are legitimate handovers and they are different facts,
    /// so the receipt reports which one happened.
    pub(crate) fn retired_an_incumbent(&self) -> bool {
        matches!(self.retirement, Retirement::Retired)
    }

    /// The controller's own classification of the switch, carried through
    /// rather than restated, so the receipt cannot disagree with the CAS.
    pub(crate) fn switch_outcome(&self) -> ReplacementOutcome {
        self.switch.outcome()
    }

    /// Render the operator-facing evidence.
    ///
    /// Consuming, and it lives here rather than in the composition root
    /// because the mapping is this operation's vocabulary: the root decides
    /// WHICH receipt to publish, not what a handover receipt means.
    pub(crate) fn into_evidence(self) -> crate::receipt::HandoverEvidence {
        crate::receipt::HandoverEvidence {
            schema: "grapt.product.handover.v1",
            retired_incumbent: self.retired_an_incumbent(),
            switch_outcome: format!("{:?}", self.switch_outcome()),
            serving: self.serving.as_str().to_owned(),
            observed_source: self.observed_source.as_str().to_owned(),
        }
    }
}

/// Serve after an explicitly invoked, validated service handover.
///
/// The whole operation, in the order that makes it a gate: validate FIRST, with
/// the store still closed, then let the ordinary composition open it and serve.
/// `PlatformOwner::prepare` migrates the service database in place under
/// `Migrate` authority, so a check placed after it would run after the very
/// change it exists to gate.
///
/// The explicit CLI operation enters this sequence; ordinary startup shares
/// election exclusion but does not produce handover evidence.
pub(crate) fn serve_after_handover(
    service_root: &Path,
    request: HandoverRequest,
) -> Result<crate::receipt::LaunchReceipt, ProductRefusal> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| ProductRefusal::Compose(error.to_string()))?;
    let validated = runtime.block_on(validate(&request))?;
    drop(runtime);
    crate::compose::run_inner(
        service_root,
        crate::startup::Admission::Handover(Box::new(validated)),
    )
}

/// Phase A — the GATE, and it must run before the store is opened.
///
/// This is the half whose position in the sequence is load-bearing. The
/// platform opens the service database with `Migrate` authority and migrates it
/// IN PLACE, so validating afterwards would run the check after the very change
/// it exists to gate. Everything here happens first: obtain the evidence, wait
/// for the incumbent to actually be gone, and only then read the source.
///
/// Returns the two proofs the election needs. It deliberately does not elect
/// anything: an operator holding a validated handover has not yet taken a slot.
pub(crate) async fn validate(request: &HandoverRequest) -> Result<Validated, ProductRefusal> {
    let config = grapt_platform::PublishedConfig::load(&request.service_root)
        .map_err(|error| refused(format!("handover configuration unavailable: {error}")))?;
    if config.build().as_str() != request.engine_version {
        return Err(refused(
            "[transition-build-mismatch] configured build differs from migration build",
        ));
    }
    let rehearsal = rehearse(
        &request.source,
        &request.run_root,
        request.run_id.clone(),
        &request.engine_version,
        &SystemClock,
    )
    .map_err(|error| refused(format!("rehearsal evidence unavailable: {error}")))?;
    let eligibility = rehearsal
        .lowered_eligibility()
        .map_err(|error| refused(format!("migration eligibility refused: {error}")))?;

    // The incumbent is read from the registration the controller owns rather
    // than supplied, so a handover cannot be pointed at some other process.
    let registration = read_registration(&request.service_root)?;
    let incumbent = registration
        .as_deref()
        .map(RegistrationRecord::decode)
        .transpose()
        .map_err(|error| refused(format!("registration is unreadable: {error}")))?;
    crash_boundary(CrashBoundary::BeforeRetirement);
    crate::fixture_seam::announce_before_retirement(
        incumbent
            .as_ref()
            .map(|record| record.owner().pid().value()),
    );
    crate::fixture_seam::hold(crate::fixture_seam::HoldPoint::BeforeRetirement).await;
    let retirement = match &incumbent {
        Some(record) => {
            crate::startup::await_retirement(
                record.owner(),
                request.retirement_attempts,
                request.retirement_interval,
                &request.service_root,
            )
            .await?
        }
        None => Retirement::NoIncumbent,
    };
    if matches!(retirement, Retirement::TimedOut) {
        return Err(transition_refused(
            DatabaseTransitionRefusal::IncumbentMayStillBeWriting {
                retirement: "timed-out",
            },
        ));
    }
    crash_boundary(CrashBoundary::AfterRetirement);

    let policy =
        LockAttemptPolicy::new(3).ok_or_else(|| refused("registration lock bound was zero"))?;
    let authority =
        ElectedControllerAuthority::acquire(request.service_root.join("registration"), policy)
            .map_err(|error| refused(error.to_string()))?;
    let current_registration = authority
        .read_registration()
        .map_err(|error| refused(error.to_string()))?;
    if current_registration != registration && current_registration.is_some() {
        return Err(refused("registration changed during retirement"));
    }
    if let Some(record) = &incumbent {
        // `validate` holds the election it acquired above: a live incumbent
        // would hold the same lock, so the held authority is the observed
        // absence on every OS.
        let dead = crate::startup::DeadPredecessor::observed_dead(record.owner().clone());
        crate::ready::retire_dead_predecessor(&request.service_root, &dead)
            .map_err(|error| ProductRefusal::Custody(error.to_string()))?;
    }
    let registration = current_registration;

    // ONLY NOW. Hoisting this above the wait type-checks and passes every test
    // without a live writer, and silently discards whatever the incumbent wrote
    // on its way down.
    let (_, observed_source) = fingerprint_file(&request.source)
        .map_err(|error| refused(format!("source observation failed: {error}")))?;

    let quiesced = QuiescedSource::observed_after(&retirement, observed_source.clone())
        .map_err(transition_refused)?;

    // The gate itself: the source must still be what the rehearsal audited. A
    // source that moved means the candidate is missing those writes, and this
    // refusal happens BEFORE the platform migrates anything.
    if !eligibility.describes_source(quiesced.observed()) {
        return Err(refused(format!(
            "[transition-source-moved-since-audit] the source moved after it was audited:              rehearsal audited {}, observed {}. Migrating now would discard the writes that              landed in between",
            eligibility.source().file().as_str(),
            observed_source.as_str()
        )));
    }

    Ok(Validated {
        expected_build: request.engine_version.clone(),
        successor: ServiceInstanceId::new(format!(
            "handover-{}",
            grapt_store::OsRandomIds::seed()
                .map_err(|error| refused(error.to_string()))?
                .instance()
        ))
        .ok_or_else(|| refused("successor instance identity is invalid"))?,
        authority,
        registration,
        eligibility,
        quiesced,
        retirement,
        observed_source,
        serving: None,
    })
}

/// The proofs phase A produced, consumed by the election.
#[derive(Debug)]
pub(crate) struct Validated {
    expected_build: String,
    successor: ServiceInstanceId,
    authority: ElectedControllerAuthority,
    registration: Option<Vec<u8>>,
    eligibility: MigrationEligibility,
    quiesced: QuiescedSource,
    retirement: Retirement,
    observed_source: DatabaseFingerprint,
    /// Measured from the open store after release verifies its domain.
    serving: Option<DatabaseFingerprint>,
}

impl Validated {
    pub(crate) fn expected_build(&self) -> &str {
        &self.expected_build
    }
    pub(crate) fn successor(&self) -> &ServiceInstanceId {
        &self.successor
    }
    pub(crate) fn measure_store(
        mut self,
        platform: &grapt_platform::PlatformOwner,
    ) -> Result<Self, ProductRefusal> {
        let (eligibility, serving) =
            grapt_release::attest_serving_store(&self.eligibility, platform.store()).map_err(
                |error| refused(format!("[transition-serving-database-unproven] {error}")),
            )?;
        self.eligibility = eligibility;
        self.serving = Some(serving);
        Ok(self)
    }
}

/// Phase B — the election, through the controller's existing compare-and-swap.
///
/// The outer operation consumes the proofs and retains election custody.
fn elect(
    validated: &Validated,
    authority: &ElectedControllerAuthority,
    declared: &OwnerReceipt,
    probe: ProbeOutcome,
    version: &ServiceRegistrationVersion,
) -> Result<HandoverReceipt, ProductRefusal> {
    let clock = SystemClock;
    let controller = ServiceController::new(
        declared.slot().clone(),
        declared.environment().clone(),
        authority,
        &clock,
    );
    let discovered = controller
        .discover()
        .map_err(|error| refused(format!("registration discovery refused: {error}")))?;
    if discovered.bytes() != validated.registration.as_deref() {
        return Err(refused("registration changed after handover validation"));
    }
    let observation = Observation::new(declared.clone(), probe, version.clone());
    let serving = validated
        .serving
        .as_ref()
        .ok_or_else(|| refused("serving database was not measured"))?;
    let prepared = prepare_database_transition(
        &controller,
        &discovered,
        &observation,
        serving,
        &validated.quiesced,
        &validated.eligibility,
    )
    .map_err(transition_refused)?;
    crash_boundary(CrashBoundary::BeforeCommit);
    let switch = controller
        .commit(prepared, validated.retirement.clone())
        .map_err(|error| refused(format!("registration commit refused: {error}")))?;
    if switch.committed_owner() != Some(declared) {
        return Err(refused(
            "registration compare-and-swap did not commit the declared owner",
        ));
    }
    crash_boundary(CrashBoundary::AfterCommit);
    Ok(HandoverReceipt {
        switch,
        retirement: validated.retirement.clone(),
        serving: serving.clone(),
        observed_source: validated.observed_source.clone(),
    })
}

/// Acquire the election and commit the handover in one step.
///
/// The registration lock, the authority and the election all belong to this
/// operation rather than to the composition root: `compose` should express
/// WHICH path is taken, not how the handover half works. Returning the
/// authority keeps the rest of the serve lifecycle identical to the ordinary
/// path, which is the property that makes "ordinary startup is unchanged"
/// checkable by reading one branch.
pub(crate) fn acquire_and_elect(
    validated: Validated,
    service_root: &Path,
    declared: &OwnerReceipt,
    probe: ProbeOutcome,
    version: &ServiceRegistrationVersion,
) -> Result<(ElectedControllerAuthority, HandoverReceipt), ProductRefusal> {
    if validated.authority.registration_path() != Some(service_root.join("registration").as_path())
    {
        return Err(refused(
            "handover authority belongs to another service root",
        ));
    }
    let receipt = elect(&validated, &validated.authority, declared, probe, version)?;
    Ok((validated.authority, receipt))
}

/// The owner currently registered in an installation, if any.
///
/// Read through the controller's own record type rather than parsed here, so
/// this cannot disagree with what the election will see.
fn read_registration(service_root: &Path) -> Result<Option<Vec<u8>>, ProductRefusal> {
    use std::io::Read;
    let path = service_root.join("registration");
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(refused(format!("registration read failed: {error}"))),
    };
    let mut bytes = Vec::new();
    file.take(8_513)
        .read_to_end(&mut bytes)
        .map_err(|error| refused(format!("registration read failed: {error}")))?;
    if bytes.len() > 8_512 {
        return Err(refused("registration exceeds 8512 bytes"));
    }
    Ok(Some(bytes))
}

fn transition_refused(refusal: DatabaseTransitionRefusal) -> ProductRefusal {
    // The transition's own label is carried through rather than flattened, so
    // an operator sees which law refused rather than a generic failure.
    refused(format!("[{}] {refusal}", refusal.label()))
}

fn refused(reason: impl Into<String>) -> ProductRefusal {
    ProductRefusal::Registration(reason.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::service::{
        Endpoint, EnvironmentReceipt, ProcessId, ServiceGeneration, ServiceSlotId,
    };
    use grapt_types::DatabaseEvidence;

    #[test]
    fn validated_handover_excludes_reentry_until_its_custody_is_released(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-handover-custody-{}",
            grapt_store::OsRandomIds::seed()?.instance()
        ));
        std::fs::create_dir(&root)?;
        std::process::Command::new("git")
            .args(["init", "--quiet"])
            .arg(&root)
            .status()?
            .success()
            .then_some(())
            .ok_or("fixture git initialization failed")?;
        std::fs::write(
            root.join("grapt.jsonc"),
            r#"{
            "store": {"authority": "migrate"},
            "product": {"build": "secode-rust@custody"},
            "provider": {"id":"openai", "model":"gpt-5.6", "base_model":"gpt-5.6",
                "family":"gpt-5", "route":"responses", "account":"account-a",
                "credential_version":"credential-v1", "policy_version":"df0-policy-v1",
                "variants":[]}}
        "#,
        )?;
        let source = root.join("database.sqlite3");
        let python = if cfg!(windows) { "python" } else { "python3" };
        let initialized = std::process::Command::new(python)
            .args(["-c", "import sqlite3,sys; c=sqlite3.connect(sys.argv[1]); c.executescript('CREATE TABLE event_sequence (aggregate_id TEXT PRIMARY KEY, seq INTEGER NOT NULL, owner_id TEXT); CREATE TABLE event (id TEXT PRIMARY KEY, aggregate_id TEXT NOT NULL, seq INTEGER NOT NULL, created INTEGER NOT NULL, type TEXT NOT NULL, data TEXT NOT NULL); CREATE TABLE kv (key TEXT PRIMARY KEY, value TEXT NOT NULL); CREATE TABLE project (id TEXT PRIMARY KEY, name TEXT NOT NULL);'); c.commit(); c.close()"])
            .arg(&source).status()?;
        assert!(initialized.success(), "fixture database creation failed");
        let request = HandoverRequest {
            service_root: root.clone(),
            run_root: root.join("run"),
            source: source.clone(),
            run_id: MigrationId::new("custody").ok_or("run id")?,
            engine_version: "secode-rust@custody".to_owned(),
            retirement_attempts: 1,
            retirement_interval: std::time::Duration::from_millis(1),
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        let proof = runtime.block_on(validate(&request))?;
        assert!(matches!(proof.retirement, Retirement::NoIncumbent));
        let before = std::fs::read(&source)?;
        for _ in 0..3 {
            let contender = ElectedControllerAuthority::acquire(
                root.join("registration"),
                LockAttemptPolicy::new(1).ok_or("policy")?,
            );
            assert!(
                contender.is_err(),
                "validated handover released election before commit"
            );
            assert_eq!(std::fs::read(&source)?, before);
            assert!(!root.join("registration").exists());
        }
        drop(proof);
        let recovered = ElectedControllerAuthority::acquire(
            root.join("registration"),
            LockAttemptPolicy::new(1).ok_or("policy")?,
        )?;
        drop(recovered);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn an_unattributed_probe_cannot_elect_a_measured_store(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let source = DatabaseFingerprint::new("source").ok_or("source fingerprint")?;
        let candidate = DatabaseFingerprint::new("candidate").ok_or("candidate fingerprint")?;
        let domain = DatabaseFingerprint::new("domain").ok_or("domain fingerprint")?;
        let eligibility = MigrationEligibility::new(
            MigrationId::new("probe").ok_or("run")?,
            DatabaseEvidence::new(source.clone(), Some(domain.clone())),
            DatabaseEvidence::new(candidate.clone(), Some(domain.clone())),
            DatabaseEvidence::new(
                DatabaseFingerprint::new("rollback").ok_or("rollback fingerprint")?,
                Some(domain),
            ),
            Vec::new(),
        )
        .ok_or("eligibility")?;
        let successor = ServiceInstanceId::new("successor").ok_or("instance")?;
        let declared = OwnerReceipt::new(
            ServiceSlotId::new("product").ok_or("slot")?,
            successor.clone(),
            EnvironmentReceipt::new("test").ok_or("environment")?,
            ServiceGeneration::FIRST,
            ProcessId::new(1).ok_or("pid")?,
            Endpoint::new("http://127.0.0.1:1").ok_or("endpoint")?,
        );
        let proof = Validated {
            expected_build: "test".to_owned(),
            successor,
            authority: ElectedControllerAuthority::isolated(),
            registration: None,
            eligibility,
            quiesced: QuiescedSource::observed_after(&Retirement::NoIncumbent, source.clone())?,
            retirement: Retirement::NoIncumbent,
            observed_source: source,
            serving: Some(candidate),
        };
        let version = ServiceRegistrationVersion::new("0.1.0").ok_or("version")?;
        let result = elect(
            &proof,
            &proof.authority,
            &declared,
            ProbeOutcome::HealthyUnattributed,
            &version,
        );
        assert!(
            result.is_err(),
            "declared identity replaced the actual unattributed probe"
        );
        assert!(proof.authority.read_registration()?.is_none());
        Ok(())
    }
}
