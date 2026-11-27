//! R6 database transition, rollback rehearsal, and handoff evidence.
//!
//! This crate is deliberately above `grapt-store` and below the unified R6
//! release authority. The store remains the only schema-migration owner. This
//! layer proves a disposable clone can cross versions and be restored, then
//! emits typed evidence for a separate authority; it cannot elect, retire, or
//! switch a service.

mod export;
mod handoff;
mod journal;
mod rehearsal;
mod snapshot;

pub use export::{export_domain, DomainExport, DomainExportError};
pub use handoff::{attest_serving_store, path_identity, HandoffError, MigrationEligibilityReceipt};
pub use journal::{JournalError, RehearsalPhase, RehearsalStatus};
pub use rehearsal::{
    domain_fingerprint, inspect_status, rehearse, restore_backup_rehearsal, RehearsalError,
    RehearsalReceipt, RollbackReceipt,
};
pub use snapshot::{fingerprint_file, SnapshotError};
