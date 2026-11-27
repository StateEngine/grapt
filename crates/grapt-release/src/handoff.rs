use crate::export::export_domain;
use crate::journal::{Journal, JournalError, RehearsalPhase};
use crate::rehearsal::{compare_migrated_domain, RehearsalReceipt};
use crate::snapshot::{
    fingerprint_file, open_read_only, require_distinct_physical_files,
    revalidate_distinct_physical_files, PhysicalFileIdentity, SnapshotError,
};
use grapt_types::{DatabaseEvidence, MigrationEligibility};
use serde::Serialize;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MigrationEligibilityReceipt {
    pub schema: &'static str,
    pub outcome: &'static str,
    pub run_id: String,
    pub source_database: PathBuf,
    pub source_fingerprint: String,
    pub source_domain_fingerprint: String,
    pub candidate_database: PathBuf,
    pub candidate_fingerprint: String,
    pub rollback_database: PathBuf,
    pub rollback_fingerprint: String,
    pub rollback_domain_fingerprint: String,
    pub applied_migrations: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum HandoffError {
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Export(#[from] crate::DomainExportError),
    #[error(transparent)]
    Journal(#[from] JournalError),
    #[error("rehearsed source, candidate, or rollback evidence changed before handoff")]
    RehearsalDrift,
    #[error("the rehearsal has not reached its ReadyForHandoff checkpoint")]
    NotReady,
    #[error(
        "the rollback's domain state does not reproduce the source's; restoring it would not \
         undo the migration, so it cannot be carried as rollback evidence"
    )]
    RollbackDoesNotReproduceSource,
    #[error("the open serving store does not reproduce the rehearsed candidate")]
    ServingDomainMismatch,
    #[error("the serving store has no persistent database path")]
    ServingPathUnavailable,
}

/// Bind rehearsal evidence to the store actually opened by the platform.
/// The store owner may migrate in place, so physical bytes need not equal the
/// rehearsal copy. Domain equivalence must be measured before lowering evidence
/// for that concrete file; a supplied expected fingerprint cannot establish it.
pub fn attest_serving_store(
    eligibility: &MigrationEligibility,
    store: &grapt_store::EventStore,
) -> Result<(MigrationEligibility, grapt_types::DatabaseFingerprint), HandoffError> {
    let connection = store.connection();
    let path = connection
        .path()
        .ok_or(HandoffError::ServingPathUnavailable)?;
    let domain = export_domain(connection)?.fingerprint()?;
    if eligibility.candidate().domain() != Some(&domain) {
        return Err(HandoffError::ServingDomainMismatch);
    }
    let (_, observed) = fingerprint_file(Path::new(path))?;
    let measured = MigrationEligibility::new(
        eligibility.run().clone(),
        eligibility.source().clone(),
        DatabaseEvidence::new(observed.clone(), Some(domain)),
        eligibility.rollback().clone(),
        eligibility.applied().to_vec(),
    )
    .ok_or(HandoffError::RollbackDoesNotReproduceSource)?;
    Ok((measured, observed))
}

impl RehearsalReceipt {
    pub fn eligibility_receipt(&self) -> Result<MigrationEligibilityReceipt, HandoffError> {
        revalidate_rehearsal(self)?;
        let journal = Journal::open(&self.root, self.run_id.clone())?;
        if !journal.has(RehearsalPhase::ReadyForHandoff) {
            return Err(HandoffError::NotReady);
        }
        Ok(MigrationEligibilityReceipt {
            schema: "grapt.release.migration-eligibility.v1",
            outcome: "eligible-for-unified-cutover-review",
            run_id: self.run_id.as_str().to_owned(),
            source_database: self.source().to_path_buf(),
            source_fingerprint: self.source_file_fingerprint().as_str().to_owned(),
            source_domain_fingerprint: self.source_domain_fingerprint().as_str().to_owned(),
            candidate_database: self.candidate().to_path_buf(),
            candidate_fingerprint: self.candidate_file_fingerprint().as_str().to_owned(),
            rollback_database: self.rollback().path().to_path_buf(),
            rollback_fingerprint: self.rollback().file_fingerprint().as_str().to_owned(),
            rollback_domain_fingerprint: self.rollback().domain_fingerprint().as_str().to_owned(),
            applied_migrations: self
                .applied_migrations()
                .iter()
                .map(|migration| migration.as_str().to_owned())
                .collect(),
        })
    }

    /// The same eligibility fact, lowered to the typed form a transition owner
    /// can consume.
    ///
    /// `eligibility_receipt` renders the operator's JSON; this renders the
    /// value `grapt-service` requires. Both run the identical revalidation
    /// first, so neither is a cheaper way to obtain the fact: evidence whose
    /// files have drifted is `RehearsalDrift` here exactly as it is there.
    ///
    /// The lowered form deliberately carries identities and not handles. The
    /// consumer owns a service slot, not a filesystem, and must not acquire one
    /// by receiving this.
    pub fn lowered_eligibility(&self) -> Result<MigrationEligibility, HandoffError> {
        revalidate_rehearsal(self)?;
        let journal = Journal::open(&self.root, self.run_id.clone())?;
        if !journal.has(RehearsalPhase::ReadyForHandoff) {
            return Err(HandoffError::NotReady);
        }
        MigrationEligibility::new(
            self.run_id.clone(),
            DatabaseEvidence::new(
                self.source_file_fingerprint().clone(),
                Some(self.source_domain_fingerprint().clone()),
            ),
            DatabaseEvidence::new(
                self.candidate_file_fingerprint().clone(),
                Some(self.candidate_domain_fingerprint().clone()),
            ),
            DatabaseEvidence::new(
                self.rollback().file_fingerprint().clone(),
                Some(self.rollback().domain_fingerprint().clone()),
            ),
            self.applied_migrations().to_vec(),
        )
        .ok_or(HandoffError::RollbackDoesNotReproduceSource)
    }
}

/// Resolve a rehearsed path to its canonical identity.
///
/// A path that cannot be canonicalized becomes `Unresolvable` rather than the
/// string as written: the type has exactly one normalization outcome, and a
/// location nobody can resolve is a fact, not a usable path. This runs after
/// revalidation has already proved the files are present, so `Unresolvable`
/// here means the path stopped resolving under us, not that it never existed.
pub fn path_identity(path: &Path) -> grapt_types::path::PathIdentity {
    match std::fs::canonicalize(path) {
        Ok(resolved) => grapt_types::path::PathIdentity::canonical(resolved.display().to_string()),
        Err(_) => grapt_types::path::PathIdentity::Unresolvable,
    }
}

fn revalidate_rehearsal(receipt: &RehearsalReceipt) -> Result<(), HandoffError> {
    let source_identity = PhysicalFileIdentity::required(receipt.source())?;
    let candidate_identity = PhysicalFileIdentity::required(receipt.candidate())?;
    let rollback_identity = PhysicalFileIdentity::required(receipt.rollback().path())?;
    require_distinct_physical_files(&[&source_identity, &candidate_identity, &rollback_identity])?;
    let (_, source_file) = fingerprint_file(receipt.source())?;
    let (_, candidate_file) = fingerprint_file(receipt.candidate())?;
    let (_, rollback_file) = fingerprint_file(receipt.rollback().path())?;
    if &source_file != receipt.source_file_fingerprint()
        || &candidate_file != receipt.candidate_file_fingerprint()
        || &rollback_file != receipt.rollback().file_fingerprint()
    {
        return Err(HandoffError::RehearsalDrift);
    }
    let source_connection = open_read_only(receipt.source())?;
    let source_export = export_domain(&source_connection)?;
    drop(source_connection);
    let candidate_connection = open_read_only(receipt.candidate())?;
    let candidate_export = export_domain(&candidate_connection)?;
    drop(candidate_connection);
    let rollback_connection = open_read_only(receipt.rollback().path())?;
    let rollback_domain = export_domain(&rollback_connection)?.fingerprint()?;
    drop(rollback_connection);
    if source_export.fingerprint()? != *receipt.source_domain_fingerprint()
        || rollback_domain != *receipt.rollback().domain_fingerprint()
    {
        return Err(HandoffError::RehearsalDrift);
    }
    compare_migrated_domain(&source_export, &candidate_export)
        .map_err(|_| HandoffError::RehearsalDrift)?;
    revalidate_distinct_physical_files(&[
        &source_identity,
        &candidate_identity,
        &rollback_identity,
    ])?;
    Ok(())
}
