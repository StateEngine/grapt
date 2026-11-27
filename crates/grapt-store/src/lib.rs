//! Durable event store. Read side first: the Rust engine's FIRST contact with
//! a real database is read-only audit (spec §"Database transition" rule 1) —
//! open an oracle-era database, decode every event through the strict codec, and
//! prove the replay preconditions hold before any write path exists.

/// The durable attempt receipt. Public so a consumer can name the record and
/// the injected byte ceiling; every WRITE stays behind
/// [`write::EventStore`]'s governed methods, so there is no consumer route to
/// the table's own statements.
///
/// This paragraph also promised a "streaming page". R2C5I replaced that page
/// with a one-row cursor carrying a remaining-byte budget and moved the row
/// bounds to lane B, so there is no page here for a consumer to name — the
/// public API docs outlived the API.
pub mod attempt_receipt;
pub mod audit;
/// The named production clock adapter. Public for the same reason [`ids`] is:
/// a caller that wants [`migration::apply_with_clock`] must be able to name the
/// production authority.
pub mod clock;
pub mod compaction;
/// The content references a durable database carries, read with zero
/// payload bytes, so pre-bind carry admission can decide completeness.
pub mod content_refs;
pub mod decode;
/// The bounded durable capture page behind an interest-bound cursor: the
/// store's witness for the bounded durable capture page. The read stays a
/// single governed method on [`EventStore`]; the SQL it runs is crate-private,
/// and the cursor's own law lives in `grapt_types`.
pub mod event_feed;
pub mod event_publication;
/// Store-owned injection of durable rows no admission would accept, for
/// cross-crate counterexample fixtures. The write-chokepoint gate pins its
/// consumers to the reviewed fixture files; see the module for the law.
pub mod fixture_rows;
mod form_fact;
pub mod operation_mail;
pub mod provider_attempt_accounting;
pub use form_fact::DecodedFormFact;
pub mod commit_executor;
pub mod commit_projection;
/// Writer election and fencing. The MODULE is private: `elect`, `fence` and
/// `current` are the store's own authority and a consumer that could call them
/// would be reaching past the [`GovernedTransaction`] chokepoint. Only the
/// public vocabulary is re-exported below.
mod election;
pub mod epoch;
pub mod export;
mod governed;
pub mod history_visibility;
/// The named production identity adapter. Public because a caller that wants
/// [`write::EventStore::open_with_ids`] must be able to name the production
/// authority, and because the seed error is part of the store's typed open
/// failure surface.
pub mod ids;
pub mod log;
/// The durable seat mailbox. Public so a consumer can name the page and its
/// bounds; every WRITE stays behind [`write::EventStore`]'s governed methods,
/// so there is no consumer route to the table's own statements.
pub mod mailbox;
pub mod mcp_credential;
pub mod migration;
/// The production migration set. Public so an operator-facing caller can name
/// what [`write::EventStore::migrate`] applies, and so a witness can assert the
/// set's identities rather than trust a count.
pub mod migrations;
pub mod notify;
mod owned_open;
mod permission_revocation;
/// Durable compatibility state whose canonical owner is the store: plugin-owned
/// integration credentials and the oracle-compatible web-search selection.
pub mod plugin_compatibility;
pub mod projection;
/// The between-migrations quarantine marker and its read-only inspection.
pub mod quarantine;
pub mod read_pool;
pub mod read_snapshot;
pub mod replay;
pub(crate) mod retired_campaign;
pub mod session_event_queries;
/// Raw schema/progress-budget seam consumed by the semantic session-list owner.
pub mod session_list;
/// Governed appends for the assistant message a Step produces: its identity,
/// its ordered content items, and its usage observations.
pub mod session_message;
pub mod step_ended;
pub mod step_failed;
pub mod stored_terminal;
mod timing;
pub mod tool_terminal;
pub mod write;

pub use audit::{audit, AuditReport};
pub use compaction::{
    CacheRebuild, CachedWindow, CompactionAttempt, CompactionError, CompactionOutcome,
};
pub use content_refs::{
    carried_content_refs, session_content_refs, CarryEnumerationBounds, ContentRefsError,
};
// The store's aggregate key is `grapt_event::AggregateId`, and every consumer
// that appends must now name one. Re-exported rather than asking six crates to
// take a direct `grapt-event` dependency: that would add six arrows to the
// dependency graph — and to the ruled ALLOWED set the direction gate enforces —
// purely so a caller can spell a type the store already hands it back.
pub use grapt_event::AggregateId;
// Inbox and subagent owners consume these durable codecs through the store,
// preserving their existing dependency edges.
pub use grapt_event::{agent_message, session_tool_identity};
// `EventKind` is deliberately NOT re-exported: the store no longer owns a
// kind vocabulary. The closed one is `grapt_event::EventKind`.
pub use decode::{DecodeError, DecodedEvent};
pub use election::{Election, ElectionError, LostWriterState, WriterOwnerId};
pub use epoch::{
    attribution_version_at, attribution_version_read_only, stamp_and_prepare, validate_read_only,
    AttributionError, AttributionRead, EpochError, EpochOutcome,
};
pub use event_feed::{
    CapturePage, CaptureReadError, CaptureRow, MAX_CAPTURE_PAGE_BYTES, MAX_CAPTURE_PAGE_ROWS,
};
pub use export::{export_canonical, CanonicalExport, TableDigest};
pub use ids::{InstanceSeedError, OsEntropy, OsRandomIds};
// `GovernedTransaction` is the whole public surface of the write chokepoint.
// `WriteGate`, `GovernedTable` and the lift guard stay unnameable outside this
// crate: a consumer that could hold one would hold the bypass.
pub use clock::SystemClock;
pub use governed::GovernedTransaction;
// Fixture-only durable injection through the store's own connection; the
// write-chokepoint gate pins its consumers to the reviewed fixture files.
pub use fixture_rows::{EventRow, FixtureRows};
pub use log::EventLog;
pub use migration::{
    apply as apply_migrations, apply_with_clock as apply_migrations_with_clock, BackupReceipt,
    BoundDatabaseError, BoundDatabaseFile, Migration, MigrationError, MigrationOutcome,
};
pub use migrations::ProductionMigrations;
pub use quarantine::{MigrationQuarantine, QuarantineError};
// The live-notice boundary. `PendingNotices` stays unnameable outside this
// crate: holding one would be holding a notice for a fact that has not
// committed, which is the exact state `CommittedNotices` exists to make
// unreachable.
pub use attempt_receipt::{
    AttemptReceiptReclaimed, AttemptReceiptRecord, AttemptReceiptReserved, AttemptRecordCeiling,
    AttemptSettlement, QuarantineReason, ReceiptQuarantined, RecoveryRow, RecoveryStep,
    ReserveInput, TerminalCoordinates,
};
pub use mailbox::remote::{
    HistoryRows, MailBindingExpectation, RemoteMailWakeClaim, RemoteMailWakeScan,
    RemoteMailboxAdmission, RemoteMailboxCheckpointPage, RemoteMailboxInput, RemoteMailboxPage,
    RemoteMailboxRecord, RemoteMailboxScope, MAX_REMOTE_MAIL_RECORD_BYTES,
};
pub use mailbox::{
    MailboxAcknowledged, MailboxAdmitted, MailboxConsumed, MailboxPage, MailboxPageEntry,
    MailboxUnreadableRow, MAX_MAILBOX_PAGE_BYTES, MAX_MAILBOX_PAGE_MESSAGES,
};
pub use migration::preflight::{
    preflight_metadata_read_only, preflight_read_only, preflight_with_walk_budget, BackupCapacity,
    BackupDirectory, BackupPermissions, CreateBackupFile, FutureBackup, MigrationPlan, Obstruction,
    StoreReadiness, Unreadable, WriteBackupFile,
};
pub use notify::{CommittedNotices, DurableNotice, Notifier, MAX_PENDING_NOTICES};
pub use owned_open::{OwnedEventStore, OwnedOpenAuthority, OwnedOpenError, OwnedOpenReceipt};
pub use plugin_compatibility::{
    IntegrationCredentialRecord, IntegrationCredentialWrite, MAX_INTEGRATION_CREDENTIAL_BYTES,
    MAX_PLUGIN_COMPATIBILITY_ID_BYTES, WEBSEARCH_PROVIDER_KEY,
};
pub use projection::{ProjectionId, ProjectionStatus};
pub use replay::{ConsistencyReport, ReplayFold};
pub use write::{
    sqlite_storage_failure, AppendInput, Appended, CaughtUp, EventStore, FencedWriter, HeldWriters,
    Projector, ProjectorError, WriteError,
};
