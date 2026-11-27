//! Migration identities. Law (`CORRECT`): migration and rollback receipts are
//! typed facts, not log text. Rulings in force (2026-08-08): Rust
//! stamps `PRAGMA user_version = 2` plus an `engine_epoch` attribution row at
//! its first schema change; there is NO reverse path — recovery is the
//! pre-conversion backup only.

crate::id::opaque_id! { MigrationId }
crate::id::opaque_id! { DatabaseFingerprint }
crate::id::opaque_id! { BackupId }

/// The engine epoch stamped into a database. oracle-era databases are epoch 0
/// (empirically verified on the real clone); Rust ownership begins at 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct EngineEpoch(pub u32);

impl EngineEpoch {
    pub const TYPESCRIPT: EngineEpoch = EngineEpoch(0);
    pub const RUST: EngineEpoch = EngineEpoch(2);
}

/// One database's identity within a rehearsed migration.
///
/// Two independent fingerprints, because they answer different questions and a
/// migration deliberately changes one without changing the other: `file` is the
/// physical bytes, `domain` is the canonical exported domain state. A migration
/// that preserved the domain necessarily rewrote the file, so collapsing these
/// into one value would make "the data survived" unstateable.
///
/// `domain` is optional because it is not always taken — exporting requires
/// opening the database, and it may be recorded before or without that read.
/// `None` means NOT MEASURED; it never means "empty" or "unchanged".
///
/// Identity only, deliberately: no path. A consumer of this value decides
/// whether evidence is about the database in front of it by comparing
/// fingerprints, and a recorded path would be the weaker answer to that same
/// question — it goes stale the moment a file is moved or promoted. Operators
/// who need locations read them from the operator receipt, which carries all
/// three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DatabaseEvidence {
    file: DatabaseFingerprint,
    domain: Option<DatabaseFingerprint>,
}

impl DatabaseEvidence {
    pub fn new(file: DatabaseFingerprint, domain: Option<DatabaseFingerprint>) -> Self {
        Self { file, domain }
    }

    pub fn file(&self) -> &DatabaseFingerprint {
        &self.file
    }

    /// The exported domain state, or `None` when it was never measured.
    pub fn domain(&self) -> Option<&DatabaseFingerprint> {
        self.domain.as_ref()
    }
}

/// The lowered, service-facing fact that a rehearsal proved one database
/// eligible for a transition.
///
/// # Why this lives in the types crate
///
/// The rehearsal that produces this evidence owns a database and a filesystem;
/// the service transition that consumes it owns neither, and must not acquire
/// them. `grapt-service` depends on this crate and nothing else, so lowering
/// the identity here lets the transition owner require the evidence without the
/// dependency inversion that importing the release crate would be. The
/// filesystem revalidation stays with the owner that can perform it.
///
/// # This is EVIDENCE, never AUTHORITY
///
/// Holding one of these does not entitle anybody to switch a service. It states
/// what a rehearsal observed; whether that rehearsal is about the database now
/// in front of you is a separate question the consumer must ask by comparing
/// [`source`](Self::source). The migration operator refuses `cutover` by design
/// and that refusal is unaffected by this type existing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrationEligibility {
    run: MigrationId,
    source: DatabaseEvidence,
    candidate: DatabaseEvidence,
    rollback: DatabaseEvidence,
    applied: Vec<MigrationId>,
}

impl MigrationEligibility {
    /// Build the lowered evidence, refusing a rollback that does not reproduce
    /// the source's domain state.
    ///
    /// That refusal is the one law this constructor carries, and it is the law
    /// that makes the word "rollback" true. The backup exists so a failed
    /// transition can return the domain to what it was before; if the restored
    /// domain fingerprint is not the source's, the file is a copy of something
    /// else and restoring it would not undo the migration. The rehearsal proves
    /// this equality directly, so evidence that fails it was never rehearsed.
    ///
    /// A source or rollback whose domain was never measured (`None`) cannot
    /// establish the equality and is refused for that reason rather than
    /// admitted on the grounds that nothing contradicted it — absent is not
    /// equal.
    pub fn new(
        run: MigrationId,
        source: DatabaseEvidence,
        candidate: DatabaseEvidence,
        rollback: DatabaseEvidence,
        applied: Vec<MigrationId>,
    ) -> Option<Self> {
        let (Some(source_domain), Some(rollback_domain)) = (source.domain(), rollback.domain())
        else {
            return None;
        };
        if source_domain != rollback_domain {
            return None;
        }
        Some(Self {
            run,
            source,
            candidate,
            rollback,
            applied,
        })
    }

    /// The operator's label for the run. NOT an identity: it is supplied by
    /// whoever invoked the rehearsal and two unrelated runs may carry the same
    /// one. Compare [`source`](Self::source) to decide what a receipt is about.
    pub fn run(&self) -> &MigrationId {
        &self.run
    }

    /// The database that was audited. This is the identity a consumer matches
    /// against the database it is about to act on.
    pub fn source(&self) -> &DatabaseEvidence {
        &self.source
    }

    /// The migrated candidate the transition would serve.
    pub fn candidate(&self) -> &DatabaseEvidence {
        &self.candidate
    }

    /// The restored pre-migration backup — the only recovery path, since no
    /// reverse migration exists.
    pub fn rollback(&self) -> &DatabaseEvidence {
        &self.rollback
    }

    pub fn applied(&self) -> &[MigrationId] {
        &self.applied
    }

    /// Whether this evidence is about `database`, by physical bytes.
    pub fn describes_source(&self, database: &DatabaseFingerprint) -> bool {
        self.source.file() == database
    }

    /// What a rollback would and would not recover, answered from this receipt.
    ///
    /// The same law as [`RecoveryDisposition::classify`], reached through the
    /// evidence for a caller that still holds it. `None` when either domain was
    /// never measured — an unmeasured baseline cannot establish that nothing
    /// changed, and "we did not look" must not render as "nothing was lost".
    pub fn recovery_disposition(
        &self,
        observed_domain: &DatabaseFingerprint,
        preserved_at: crate::path::PathIdentity,
    ) -> Option<RecoveryDisposition> {
        Some(RecoveryDisposition::classify(
            self.rollback.domain()?.clone(),
            self.candidate.domain()?.clone(),
            observed_domain,
            preserved_at,
        ))
    }
}

/// What restoring the pre-cutover backup would and would not recover.
///
/// The law this type exists to make unsayable: **restoring a pre-cutover backup
/// is not lossless if work happened after activation.** There is no reverse
/// migration (ruling 2026-08-08), so the backup is the only recovery path — and
/// precisely because it is the only one, describing it honestly matters more
/// than describing it favourably. Rolling back returns the domain to its
/// pre-migration state, which means every event the Rust engine durably
/// recorded after activation is NOT in the restored database.
///
/// The two variants are separate rather than one struct with a boolean because
/// a boolean invites being ignored; a caller must match on
/// [`DiscardsPostActivationWork`](Self::DiscardsPostActivationWork) to render a
/// disposition at all.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryDisposition {
    /// The served database's domain is still exactly the activation baseline.
    /// Nothing was durably recorded after activation, so restoring the backup
    /// resumes everything.
    RestoresEverything {
        /// The domain state both the backup and the source carry.
        restored_domain: DatabaseFingerprint,
    },
    /// The served domain has moved on from the activation baseline. Restoring
    /// the backup is still the only recovery path, and it still works — but it
    /// discards what is named here, and no caller may describe it as lossless.
    DiscardsPostActivationWork {
        restored_domain: DatabaseFingerprint,
        /// The domain proven at activation.
        activation_baseline: DatabaseFingerprint,
        /// The domain observed now. The difference between this and the
        /// baseline is the work a rollback does not resume.
        observed_domain: DatabaseFingerprint,
        /// Where that work continues to live. A rollback preserves the
        /// post-activation database rather than deleting it, so the discarded
        /// work remains recoverable by hand even though this transition does
        /// not resume it.
        preserved_at: crate::path::PathIdentity,
    },
}

impl RecoveryDisposition {
    /// Classify a rollback against what the database actually looks like now.
    ///
    /// `observed_domain` is the domain exported from the served database at the
    /// moment rollback is considered; the owner that can open it supplies it.
    ///
    /// This takes the two proven domains as values rather than reaching into a
    /// [`MigrationEligibility`], and that is a measured requirement rather than
    /// a style choice. The question "what would a rollback cost?" is asked
    /// AFTER activation, when the served database has legitimately changed —
    /// and at that point the rehearsal cannot be re-run to rebuild a receipt,
    /// because resuming over a changed candidate is exactly what the
    /// resurrection guard refuses. The durable journal still holds both proven
    /// domains, so the disposition must be answerable from those alone.
    /// [`MigrationEligibility::recovery_disposition`] is the same law reached
    /// through a receipt, for callers that still hold one.
    ///
    /// Note the asymmetry that makes this honest: equality of the observed
    /// domain with the baseline is required to claim a lossless restore.
    /// Anything else — including a domain that merely *looks* smaller or that
    /// cannot be explained — is treated as post-activation work, because the
    /// failure this guards is claiming losslessness wrongly, not claiming loss
    /// wrongly.
    pub fn classify(
        restored_domain: DatabaseFingerprint,
        activation_baseline: DatabaseFingerprint,
        observed_domain: &DatabaseFingerprint,
        preserved_at: crate::path::PathIdentity,
    ) -> Self {
        if &activation_baseline == observed_domain {
            return RecoveryDisposition::RestoresEverything { restored_domain };
        }
        RecoveryDisposition::DiscardsPostActivationWork {
            restored_domain,
            activation_baseline,
            observed_domain: observed_domain.clone(),
            preserved_at,
        }
    }

    /// Whether this rollback resumes everything.
    ///
    /// Deliberately the only boolean on the type, and deliberately phrased so
    /// the dangerous answer is the one that requires the positive variant.
    pub fn is_lossless(&self) -> bool {
        matches!(self, RecoveryDisposition::RestoresEverything { .. })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DatabaseEvidence, DatabaseFingerprint, EngineEpoch, MigrationEligibility, MigrationId,
    };

    #[test]
    fn rust_epoch_is_newer_than_typescript() {
        assert!(EngineEpoch::RUST > EngineEpoch::TYPESCRIPT);
    }

    fn fingerprint(value: &str) -> Option<DatabaseFingerprint> {
        DatabaseFingerprint::new(value)
    }

    fn evidence(file: &str, domain: Option<&str>) -> Option<DatabaseEvidence> {
        Some(DatabaseEvidence::new(
            fingerprint(file)?,
            match domain {
                Some(domain) => Some(fingerprint(domain)?),
                None => None,
            },
        ))
    }

    fn eligibility(
        source_domain: Option<&str>,
        rollback_domain: Option<&str>,
    ) -> Option<Option<MigrationEligibility>> {
        Some(MigrationEligibility::new(
            MigrationId::new("run-1")?,
            evidence("source-bytes", source_domain)?,
            evidence("candidate-bytes", None)?,
            evidence("rollback-bytes", rollback_domain)?,
            vec![MigrationId::new("20260811_rebuild")?],
        ))
    }

    #[test]
    fn a_rollback_reproducing_the_source_domain_is_admitted() -> Result<(), String> {
        let admitted = eligibility(Some("domain-a"), Some("domain-a"))
            .ok_or("fixture identities must be valid")?
            .ok_or("a rollback that reproduces the source domain is the rehearsed case")?;
        assert_eq!(admitted.applied().len(), 1);
        assert!(admitted
            .describes_source(&DatabaseFingerprint::new("source-bytes").ok_or("fingerprint")?));
        assert!(!admitted
            .describes_source(&DatabaseFingerprint::new("another-database").ok_or("fingerprint")?));
        Ok(())
    }

    #[test]
    fn a_rollback_that_does_not_reproduce_the_source_domain_is_refused() -> Result<(), String> {
        assert!(
            eligibility(Some("domain-a"), Some("domain-b"))
                .ok_or("fixture identities must be valid")?
                .is_none(),
            "a backup restoring a different domain does not undo the migration, \
             so it cannot be carried as rollback evidence"
        );
        Ok(())
    }

    #[test]
    fn an_unmeasured_domain_cannot_stand_in_for_equality() -> Result<(), String> {
        for (source, rollback) in [
            (None, Some("domain-a")),
            (Some("domain-a"), None),
            (None, None),
        ] {
            assert!(
                eligibility(source, rollback)
                    .ok_or("fixture identities must be valid")?
                    .is_none(),
                "an unmeasured domain is absent, not equal: {source:?} vs {rollback:?}"
            );
        }
        Ok(())
    }
}
