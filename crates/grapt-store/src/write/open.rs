use super::{EventStore, WriteError};
use crate::election::WriterOwnerId;
use grapt_types::IdAuthority;
use rusqlite::Connection;
use std::path::Path;

mod fresh;
mod journal;
pub(crate) use fresh::{FreshBoundary, FreshOpenError};

#[derive(Debug, thiserror::Error)]
pub(crate) enum ExistingOpenError {
    #[error("database file identity could not be read: {0}")]
    Identity(std::io::Error),
    #[error("database path no longer names the classified file")]
    Replaced,
    #[error("sqlite open: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Open(#[from] WriteError),
}

impl EventStore {
    /// Opens a store under a freshly seeded production writer identity.
    ///
    /// Signature-compatible with every existing caller. The identity now comes
    /// from the named adapter in [`crate::ids`], seeded once per call, so two
    /// stores opened in one process remain two owners and a store opened after a
    /// restart can never reproduce a dead instance's identity.
    ///
    /// Seeding is fallible and its failure is typed
    /// ([`WriteError::InstanceSeed`]): a machine that cannot supply randomness
    /// refuses to open rather than aborting or proceeding under a weaker
    /// identity.
    pub fn open(path: &Path, engine_version: &str) -> Result<Self, WriteError> {
        let ids = crate::ids::OsRandomIds::seed()?;
        Self::open_with_ids(path, engine_version, &ids)
    }

    /// Opens a store under a caller-supplied identity authority.
    ///
    /// Additive: [`EventStore::open`] keeps its signature and every existing
    /// call site compiles unchanged. Its caller today is
    /// `both_store_seams_carry_the_injected_identity_token_into_the_owner`, which
    /// injects a known token and asserts the resulting owner carries it — the
    /// proof that this seam is consumed rather than merely present, which
    /// ambient randomness cannot express.
    pub fn open_with_ids(
        path: &Path,
        engine_version: &str,
        ids: &dyn IdAuthority,
    ) -> Result<Self, WriteError> {
        let conn = Connection::open(path)?;
        // Concurrent openers/writers serialize on the immediate transaction;
        // a bounded busy wait turns contention into ordering, not failure.
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        Self::open_prepared(conn, engine_version, ids)
    }

    pub(crate) fn open_existing_with_ids(
        path: &Path,
        expected: &crate::migration::authority::DatabaseFileHandle,
        engine_version: &str,
        ids: &dyn IdAuthority,
    ) -> Result<Self, ExistingOpenError> {
        expected.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, expected.identity())?;
        let conn = expected.open_sqlite(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        conn.busy_timeout(std::time::Duration::from_secs(10))?;
        expected.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, expected.identity())?;
        let store = Self::open_prepared(conn, engine_version, ids)?;
        expected.revalidate().map_err(ExistingOpenError::Identity)?;
        Self::require_file_identity(path, expected.identity())?;
        Ok(store)
    }

    fn require_file_identity(
        path: &Path,
        expected: &crate::migration::authority::DatabaseFileIdentity,
    ) -> Result<(), ExistingOpenError> {
        let observed = crate::migration::authority::database_file_identity(path)
            .map_err(ExistingOpenError::Identity)?;
        if &observed != expected {
            return Err(ExistingOpenError::Replaced);
        }
        Ok(())
    }

    /// Opens a second connection under an EXISTING owner identity.
    ///
    /// Fencing is about ownership, not connections. One owner may legitimately
    /// hold several connections to a database — they serialize on
    /// `BEGIN IMMEDIATE` and must not fence each other, or a writer would lock
    /// itself out by opening a second handle.
    ///
    /// The identity can only be cloned from a store that already minted it;
    /// [`WriterOwnerId`] has no public constructor, so this cannot be used to
    /// assume another writer's identity.
    pub fn open_as(
        path: &Path,
        engine_version: &str,
        owner: &WriterOwnerId,
    ) -> Result<Self, WriteError> {
        let mut store = Self::open(path, engine_version)?;
        store.owner = owner.clone();
        Ok(store)
    }

    pub fn open_in_memory() -> Result<Self, WriteError> {
        let ids = crate::ids::OsRandomIds::seed()?;
        Self::open_in_memory_with_ids(&ids)
    }

    /// In-memory counterpart of [`EventStore::open_with_ids`].
    ///
    /// Additive for the same reason, and exercised by the same witness: a
    /// deterministic identity is what makes the injected-token link assertable,
    /// and most fixtures in this workspace use a database that never touches
    /// disk, so the seam has to exist on both construction paths or the
    /// in-memory one would be unwitnessed.
    pub fn open_in_memory_with_ids(ids: &dyn IdAuthority) -> Result<Self, WriteError> {
        let conn = Connection::open_in_memory()?;
        Self::open_prepared(conn, "in-memory", ids)
    }

    /// The one place a freshly opened connection becomes a store.
    ///
    /// Both public open paths funnel through here so the initialization ORDER
    /// is stated once and cannot drift apart between them: foreign keys before
    /// any transaction, then the stamp/schema transaction, then the
    /// governed-write denial.
    pub(super) fn open_prepared(
        mut conn: Connection,
        engine_version: &str,
        ids: &dyn IdAuthority,
    ) -> Result<Self, WriteError> {
        // BEFORE `stamp_and_prepare`, and here is what that buys, stated
        // precisely because an earlier version of this comment overreached.
        //
        // `stamp_and_prepare` opens `BEGIN IMMEDIATE`, runs `ensure_schema`,
        // and COMMITS before it returns. So there are three placements, not
        // two, and they are not equivalent:
        //
        //   * HERE (before the call) — the preparation transaction itself runs
        //     with enforcement on. That transaction stamps the epoch, creates
        //     every governed table and executes the four-arm target-shape
        //     probe, which stages rows into `event_sequence` and `event`. This
        //     is the only placement under which that work is checked.
        //   * AFTER the call returns — enforcement is enabled for all LATER
        //     work and the store still ends up correct for everything a
        //     consumer does. It leaves only the preparation/migration
        //     transaction unchecked. This is a weaker design, NOT an inert one.
        //   * INSIDE `stamp_and_prepare`'s open transaction — inert. `PRAGMA
        //     foreign_keys` is a documented no-op inside an open transaction,
        //     measured rather than quoted: set there it reads back 0 both
        //     inside that transaction and after it commits.
        //
        // The earlier claim that "a call one line lower would enforce nothing"
        // was FALSE, because it ignored that commit. R01 does not attribute
        // this ordering either — see `F3B4B_RESIDUALS.md` item 9, which records
        // the placement as structure-carried and currently unwitnessed.
        Self::enforce_foreign_keys(&conn)?;
        // BEFORE the stamp/schema transaction. A half-migrated database must
        // not be stamped, prepared or handed back: the refusal has to happen
        // while the file is still exactly what the interrupted run left.
        Self::refuse_a_quarantined_database(&conn)?;
        let mut budget = crate::session_list::install_vm_budget(
            &mut conn,
            crate::session_list::SqliteVmBudget::open_verification(),
        )?;
        let deferred = budget.suspend();
        budget.stamp_and_prepare(engine_version, |tx| Self::ensure_schema(tx, &deferred))?;
        budget.finish()?;
        // AFTER it, and deliberately so. `ensure_schema` creates the target
        // indexes with `IF NOT EXISTS`, which is a no-op against a same-named
        // index of the wrong shape; the verification has to observe what
        // actually survived that statement rather than what it asked for.
        Self::verify_index_shapes(&conn)?;
        // Admission and legacy rollback-journal recovery finish before the
        // journal mode changes; refused databases remain untouched.
        journal::configure(&conn)?;
        Self::guarded(conn, ids)
    }

    /// Refuses a database whose migration run entered quarantine and never
    /// left, naming the outstanding debt and its verified rollback point.
    fn refuse_a_quarantined_database(conn: &Connection) -> Result<(), WriteError> {
        match crate::quarantine::inspect(conn)? {
            None => Ok(()),
            Some(quarantine) => Err(WriteError::MigrationQuarantined {
                detail: quarantine.describe(),
            }),
        }
    }
}
