use crate::decode::DecodeError;
use crate::election::ElectionError;

#[derive(Debug, thiserror::Error)]
pub enum WriteError {
    #[error("sqlite: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Decode(#[from] DecodeError),
    #[error(transparent)]
    Epoch(#[from] crate::epoch::EpochError),
    #[error(transparent)]
    Election(#[from] ElectionError),
    #[error("identity is already claimed; create requires a fresh identity")]
    AggregateClaimed { aggregate: grapt_event::AggregateId },
    /// The writer identity could not be seeded from OS randomness, so the store
    /// refuses to open. Typed rather than a panic: an identity that is not
    /// unique over time would silently defeat fencing, so proceeding is worse
    /// than refusing.
    #[error(transparent)]
    InstanceSeed(#[from] crate::ids::InstanceSeedError),
    /// The connection refused to enable foreign-key enforcement, so the store
    /// refuses to open rather than hand back one whose central integrity
    /// constraint is inert.
    ///
    /// Typed, and carrying the value that was read back, because the two
    /// failure modes are indistinguishable without it: `PRAGMA foreign_keys =
    /// ON` reports no error on a build compiled without foreign-key support, it
    /// simply has no effect.
    #[error(
        "foreign-key enforcement could not be enabled on this connection (PRAGMA foreign_keys read back {read_back}); refusing to open a store whose event -> event_sequence constraint would be inert"
    )]
    ForeignKeysUnavailable { read_back: i64 },
    #[error("store journal policy unavailable (journal={journal}, synchronous={synchronous})")]
    JournalUnavailable { journal: String, synchronous: i64 },
    /// The database is mid-migration: a run entered quarantine and never left.
    ///
    /// Refused BEFORE the stamp/schema transaction, because a half-migrated
    /// database can reach a shape that satisfies `verify_columns` and the
    /// target-shape probe by accident. Without this the open would either
    /// succeed over partially converted data, or fail naming a COLUMN — which
    /// reads as an alien database rather than as an interrupted migration that
    /// can be resumed and has a verified rollback point.
    ///
    /// Detection and refusal only: opening never resumes, repairs or re-drives.
    #[error("{detail}; re-run the production migration set to resume, or restore the backup")]
    MigrationQuarantined { detail: String },
    /// The quarantine marker itself could not be read, so the open fails
    /// closed rather than treating corruption as an absent marker.
    #[error(transparent)]
    Quarantine(#[from] crate::quarantine::QuarantineError),
    /// An index carries a target NAME over the wrong COLUMN LIST.
    ///
    /// `CREATE INDEX IF NOT EXISTS` silently honours a preexisting namesake, so
    /// a database that reached this shape by any route other than the store's
    /// own first contact can be running on the TypeScript era's family-BLIND
    /// indexes while every column check passes: `PRAGMA table_info` reports
    /// columns and cannot see an index at all. A surviving family-blind UNIQUE
    /// index enforces the wrong law.
    #[error(
        "index {index} exists with the wrong shape (columns {found:?}, unique {found_unique}); the target is columns {expected:?}, unique {expected_unique}. A same-named index from an earlier schema was silently honoured by CREATE INDEX IF NOT EXISTS"
    )]
    IndexShapeMismatch {
        index: &'static str,
        found: Vec<String>,
        found_unique: bool,
        expected: &'static [&'static str],
        expected_unique: bool,
    },
    /// This store instance holds no epoch for the aggregate. Distinct from
    /// `ElectionError::UnknownWriter`, which means no writer is elected in the
    /// DATABASE: this one means the caller never asked to be that writer.
    /// Append never elects on a caller's behalf — an append that elected itself
    /// would make fencing vacuous, because every writer would simply take
    /// ownership on its way past the check.
    #[error(
        "this store instance holds no elected writer epoch for {family}:{aggregate}; call elect_writer first"
    )]
    NotElected {
        family: &'static str,
        aggregate: String,
    },
    #[error(
        "append diverged: event {id} exists with different identity (aggregate/type/created/writer epoch/data)"
    )]
    Divergence { id: String },
    #[error("corrupt stored sequence {value} for aggregate {aggregate}")]
    CorruptSequence { aggregate: String, value: i64 },
    #[error("sequence space exhausted for aggregate {aggregate}")]
    SequenceExhausted { aggregate: String },
    #[error("projector {name} failed: {message}")]
    Projector { name: String, message: String },
    /// The database failed under a projector, which is not the projector
    /// refusing the fact.
    #[error("projector {name} failed: {source}")]
    ProjectorStorage {
        name: String,
        source: rusqlite::Error,
    },
    /// A projector that names itself with nothing cannot key a watermark: every
    /// unnamed projection would share one row and read as each other's progress.
    #[error("a projection must have a non-empty identity to key its watermark")]
    UnnamedProjection,
    /// The same sequence projected twice, or a projection moving backwards.
    /// Refused rather than silently no-op'd, so a retry cannot quietly double a
    /// projected row's effect.
    #[error(
        "projection {projection} is at {stored} for {aggregate}; advancing to {attempted} is not forward progress"
    )]
    WatermarkNotMonotonic {
        projection: &'static str,
        aggregate: String,
        stored: u64,
        attempted: u64,
    },
    #[error("corrupt stored watermark {value} for projection {projection} on {aggregate}")]
    CorruptWatermark {
        projection: &'static str,
        aggregate: String,
        value: i64,
    },
    /// A projection claiming progress its canonical input does not have.
    /// Unreachable through this crate's write path — see `crate::projection`.
    #[error(
        "projection {projection} is at {watermark} for {aggregate}, past the durable head {head:?}: a projection may lag but never lead"
    )]
    WatermarkLeads {
        projection: &'static str,
        aggregate: String,
        watermark: u64,
        head: Option<u64>,
    },
    /// A quarantine record already exists for this attempt and DISAGREES with
    /// what is being offered. Never an overwrite: the first durable report of a
    /// corruption is the one artifact the quarantine exists to keep.
    #[error(
        "attempt receipt {attempt} is already quarantined as {held}; {offered} was offered and refused"
    )]
    ReceiptQuarantineConflict {
        attempt: String,
        held: String,
        offered: String,
    },
    /// The record is addressed to a different seat than the caller declared.
    /// The expected recipient is supplied SEPARATELY from the record, so a
    /// record that names its own recipient cannot authorise itself.
    #[error("mailbox message {message} is addressed to {found}, not the expected {expected}")]
    MailboxWrongRecipient {
        message: String,
        expected: String,
        found: String,
    },
    /// A different record already exists under this message id. Exact retry
    /// reconciles ONLY byte-identical canonical data; conflicting reuse of a
    /// message identity refuses and writes nothing.
    #[error("mailbox message {message} already holds different canonical data")]
    MailboxConflict { message: String },
    /// A stored mailbox row's bytes did not decode, or its indexed columns
    /// disagree with the decoded bytes. Refused rather than skipped: a mailbox
    /// that silently drops a message it cannot read is worse than one that says
    /// it cannot read it.
    #[error("mailbox row {message} is unreadable: {reason}")]
    MailboxUndecodable { message: String, reason: String },
    /// Acknowledging or consuming a message this seat never durably admitted.
    /// Local admission comes FIRST, and this refusal is that ordering.
    #[error("mailbox message {message} was never admitted here")]
    MailboxNotAdmitted { message: String },
    /// An explicit supersedes edge naming a message that does not exist.
    #[error("mailbox message {message} supersedes {target}, which does not exist")]
    MailboxSupersedesMissing { message: String, target: String },
    /// An explicit supersedes edge reaching into another seat's mailbox.
    /// Suppression is a same-recipient relation; a cross-recipient edge would
    /// let one seat silence another's mail.
    #[error("mailbox message {message} supersedes {target}, which belongs to another recipient")]
    MailboxSupersedesCrossRecipient { message: String, target: String },
    /// A stored admission position that is not a position at all.
    #[error("mailbox recipient {recipient} holds a corrupt admission sequence {value}")]
    MailboxCorruptSequence { recipient: String, value: i64 },
    /// The recipient's local sequence cannot advance without wrapping onto a
    /// position it has already used.
    #[error("mailbox recipient {recipient} has exhausted its admission sequence")]
    MailboxSequenceExhausted { recipient: String },
    /// A zero-message page reads nothing and would be indistinguishable from an
    /// exhausted mailbox.
    #[error("a mailbox page limit of zero reads nothing; ask for at least one message")]
    MailboxPageLimitZero,

    /// A complete progress fact needs a fresh recovery slice; no cursor moved.
    #[error("remote mail progress exceeds the remaining recovery slice budget")]
    RemoteMailProgressBudget,
    #[error("remote mailbox refuses: {reason}")]
    RemoteMailbox { reason: String },
    #[error("agent result mail refuses: {reason}")]
    OperationMail { reason: String },
    // -- attempt receipt (R2C5A1). Seven distinct arms because a reconciler
    // behaves differently for each; a collapsed refusal would not say what to
    // do next. Decode and column/record disagreement are deliberately SEPARATE:
    // the first is bytes it cannot read, the second is two readable stories
    // that contradict each other.
    /// A stored receipt's bytes did not decode through this crate's canonical
    /// decoder, or exceeded the injected record ceiling before parsing began.
    #[error("attempt receipt {attempt} is undecodable: {reason}")]
    ReceiptUndecodable { attempt: String, reason: String },
    /// A stored receipt's indexed columns disagree with its decoded bytes.
    /// Every indexed column has a bytes counterpart, so this covers all seven.
    #[error("attempt receipt {attempt} disagrees with its own bytes: {detail}")]
    ReceiptRecordMismatch { attempt: String, detail: String },
    /// A receipt's marker instants run backwards. Unrepresentable through A0's
    /// consuming transitions, so it arises only from a foreign writer or
    /// corruption.
    #[error("attempt receipt {attempt} has out-of-order markers: {detail}")]
    ReceiptMarkerOrder { attempt: String, detail: String },
    // There is deliberately NO `ReceiptMarkerConflict` arm here. A different
    // instant offered for a held marker is carried by A0's landed
    // `MarkerOutcome::Conflict { held, offered }`, which is already typed,
    // already reports both instants and already preserves the held one — and
    // which ruling 3 requires this lane to CONSUME rather than restate. A second
    // carrier in this enum would be one law with two spellings, free to drift,
    // and no single edit could then attribute either. The marker ORDER refusal
    // above is a different condition (instants running backwards) and is not
    // expressible as a `MarkerOutcome`, which is why it lives here.
    /// A different canonical record already exists under this attempt id. Exact
    /// retry reconciles ONLY byte-identical canonical data.
    #[error("attempt receipt {attempt} already holds different canonical data")]
    ReceiptConflict { attempt: String },
    /// A marker or retention call for an attempt that was never reserved here.
    /// Reserve comes FIRST, and this refusal is that ordering.
    #[error("attempt receipt {attempt} was never reserved here")]
    ReceiptNotReserved { attempt: String },
    /// The row's elected epoch is not the one the caller holds. Two processes,
    /// one stale: the stale one refuses rather than overwriting.
    #[error(
        "attempt receipt {attempt} was reserved under epoch {held}, not the caller's {offered}"
    )]
    ReceiptStaleEpoch {
        attempt: String,
        held: u32,
        offered: u32,
    },
    /// A deletion was attempted while `settled_ms IS NULL`. An unsettled receipt
    /// is the only evidence an external call may still be outstanding, so it is
    /// never removed.
    #[error("attempt receipt {attempt} is unsettled and may not be deleted")]
    ReceiptUnsettledDelete { attempt: String },
    /// A reclaim whose named terminal Step fact could not be read back inside
    /// the deleting transaction, or which is not this receipt's terminal.
    #[error("attempt receipt {attempt} cannot be reclaimed against {terminal}: {detail}")]
    ReceiptTerminalUnproven {
        attempt: String,
        terminal: String,
        detail: String,
    },
    /// The whole record exceeds the injected byte ceiling. Raised at RESERVE,
    /// before any I/O and before any external call — never by a marker write,
    /// which cannot grow a record whose markers are fixed width.
    #[error(
        "attempt receipt {attempt} encodes to {bytes} bytes, above the {ceiling}-byte record ceiling"
    )]
    ReceiptRecordTooLarge {
        attempt: String,
        bytes: usize,
        ceiling: u32,
    },
    /// The Step-failure codec refused to encode the offered fact. One fact
    /// can reach this: a decode-only `StepTerminalReason::Unknown`, which this
    /// build may read and project but may never write back as a durable event
    /// of its own.
    #[error(transparent)]
    StepFailedPayloadRefused(#[from] grapt_event::session_step_failed::FactError),
    #[error("plugin compatibility input was refused: {detail}")]
    PluginCompatibilityInput { detail: String },
    #[error("plugin compatibility identity {identity} already stores different data")]
    PluginCompatibilityConflict { identity: String },
    #[error("corrupt {owner} state: {detail}")]
    PluginCompatibilityCorrupt { owner: &'static str, detail: String },
    #[error("MCP credential coordinate or value exceeds its bound")]
    McpCredentialInput,
    #[error("MCP credential changed; reread before retrying")]
    McpCredentialConflict,
    #[error("stored MCP credential revision or value is corrupt")]
    McpCredentialCorrupt,
    #[error("MCP credential revision exhausted")]
    McpCredentialExhausted,
}

impl WriteError {
    /// Whether the database itself failed (unreadable, read-only, missing its
    /// schema, out of space, locked) rather than one row or item it holds.
    /// A caller that may skip a bad item must still fail on these.
    pub fn is_storage_failure(&self) -> bool {
        match self {
            Self::Sqlite(error) => sqlite_storage_failure(error),
            Self::Election(ElectionError::Sqlite(_)) | Self::ProjectorStorage { .. } => true,
            _ => false,
        }
    }
}

/// Why a [`super::Projector`] did not project a fact: it refused the fact, or
/// the database failed under it. Kept apart so a caller that may skip one bad
/// item still fails on the database.
#[derive(Debug, thiserror::Error)]
pub enum ProjectorError {
    #[error("{0}")]
    Refused(String),
    #[error("sqlite: {0}")]
    Storage(rusqlite::Error),
}

impl From<String> for ProjectorError {
    fn from(reason: String) -> Self {
        Self::Refused(reason)
    }
}

impl From<&str> for ProjectorError {
    fn from(reason: &str) -> Self {
        Self::Refused(reason.to_owned())
    }
}

impl From<rusqlite::Error> for ProjectorError {
    fn from(error: rusqlite::Error) -> Self {
        if sqlite_storage_failure(&error) {
            Self::Storage(error)
        } else {
            Self::Refused(error.to_string())
        }
    }
}

/// [`WriteError::is_storage_failure`] for a bare SQLite error. A constraint
/// refusal is about the row written; a value that does not convert is about
/// the row read. Every other engine failure is the database's.
pub fn sqlite_storage_failure(error: &rusqlite::Error) -> bool {
    match error {
        rusqlite::Error::SqliteFailure(code, _) => {
            code.code != rusqlite::ErrorCode::ConstraintViolation
        }
        rusqlite::Error::SqlInputError { .. } => true,
        _ => false,
    }
}
