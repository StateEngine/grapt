use crate::migration::authority::{
    apply_authorized_with_clock, preflight_bounded_observed, preflight_metadata_observed,
    preflight_observed, DatabaseFileHandle,
};
use crate::migration::preflight::{
    preflight_read_only, MigrationPlan, Obstruction, StoreReadiness, Unreadable,
};
use crate::write::ExistingOpenError;
use crate::{
    EventStore, MigrationError, MigrationOutcome, OsRandomIds, ProductionMigrations, SystemClock,
    WriteError,
};
use grapt_types::{ClockAuthority, IdAuthority};
use std::path::Path;

mod fresh;

/// Explicit authority for one exact read-only observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OwnedOpenAuthority {
    OpenReady,
    Initialize,
    Migrate(MigrationPlan),
}

#[derive(Debug, PartialEq, Eq)]
pub enum OwnedOpenReceipt {
    OpenedReady,
    Initialized(MigrationOutcome),
    Migrated {
        authorized: MigrationPlan,
        migration: MigrationOutcome,
    },
    /// The classified hot rollback journal was rolled back through a
    /// bounded writable connection before the store opened. `slow` says the
    /// post-recovery page walk spent its budget, so the store opened without a
    /// finished check: the run proceeds and the slow check is surfaced.
    RecoveredHotJournal {
        slow: bool,
    },
}

#[derive(Debug)]
pub struct OwnedEventStore {
    store: EventStore,
    receipt: OwnedOpenReceipt,
}

impl OwnedEventStore {
    pub fn store(&self) -> &EventStore {
        &self.store
    }

    pub fn store_mut(&mut self) -> &mut EventStore {
        &mut self.store
    }

    pub fn receipt(&self) -> &OwnedOpenReceipt {
        &self.receipt
    }

    /// Consume the exact owned-open authority and hand its one writable store
    /// to the next product owner.
    ///
    /// This is deliberately consuming: a caller cannot retain the
    /// classification receipt while constructing a second view of the same
    /// SQLite transaction boundary.
    pub fn into_store(self) -> EventStore {
        self.store
    }
}

/// Typed refusal; unreadable, corrupt, absent, stale, and migration debt never collapse.
#[derive(Debug, thiserror::Error)]
pub enum OwnedOpenError {
    #[error("store path is unreadable: {0:?}")]
    Unreadable(Box<Unreadable>),
    #[error("store is quarantined or corrupt: {0:?}")]
    QuarantinedOrCorrupt(Box<Obstruction>),
    #[error(
        "owned-open authority {authority:?} does not authorize the observed store state \
         {observed:?}; {RESTART}"
    )]
    AuthorityMismatch {
        observed: Box<StoreReadiness>,
        authority: Box<OwnedOpenAuthority>,
    },
    #[error(
        "migration authorization is stale: authorized {authorized:?}, observed {observed:?}; \
         {advice}",
        advice = Self::stale_plan_advice(.observed.as_ref())
    )]
    StaleMigrationPlan {
        authorized: Box<MigrationPlan>,
        observed: Box<StoreReadiness>,
    },
    #[error("another creator won absent-path initialization; current state is {0:?}; {RESTART}")]
    InitializationConflict(Box<StoreReadiness>),
    #[error("database file identity could not be established: {0}")]
    FileIdentity(std::io::Error),
    #[error("database path no longer names the file classified read-only")]
    PathIdentityChanged,
    #[error("store migration failed before an owned store could be returned: {0}")]
    Migration(#[from] MigrationError),
    #[error("store open failed before an owned store could be returned: {0}")]
    Open(#[from] WriteError),
    /// The hot-journal rollback succeeded but the store could not be
    /// reopened through the normal owned-open path afterwards. Typed
    /// separately (and without `#[from]`, which would duplicate the
    /// `Open` `From<WriteError>`) so a failed recovery is never read as an
    /// ordinary open failure.
    #[error("hot-journal recovery failed before an owned store could be returned: {0}")]
    HotJournalRecovery(WriteError),
    #[error("store preparation completed without a second Ready classification: {0:?}")]
    PostActionNotReady(Box<StoreReadiness>),
}

/// The way out when the store changed between its read-only observation and
/// the open: the next start observes it again and acts on what it finds.
const RESTART: &str = "the store changed while it was being opened; close any other Grapt \
                       process using this profile and start again";

impl OwnedOpenError {
    /// What to do about a stale plan, given the state observed now. An
    /// obstructed store keeps its refusal: no restart would get past it.
    fn stale_plan_advice(observed: &StoreReadiness) -> &'static str {
        match observed {
            StoreReadiness::Unreadable(_) | StoreReadiness::QuarantinedOrCorrupt(_) => {
                "the observed store is obstructed; the refusal stands until the obstruction \
                 is resolved"
            }
            _ => RESTART,
        }
    }
}

/// The path a caller named, which may be a file link, and the real file it
/// resolved to when the open began. SQLite only ever opens the real file. The
/// caller's path must still lead to the bound file before the store is
/// written and before it is handed over, so a link removed or retargeted
/// meanwhile refuses rather than leaving the caller with another file.
struct StorePath<'a> {
    supplied: &'a Path,
    real: std::path::PathBuf,
}

impl<'a> StorePath<'a> {
    fn resolve(supplied: &'a Path) -> Result<Self, OwnedOpenError> {
        let real = crate::migration::authority::real_database_path(supplied)
            .map_err(OwnedOpenError::FileIdentity)?;
        Ok(Self { supplied, real })
    }

    fn still_names(&self, handle: &DatabaseFileHandle) -> Result<(), OwnedOpenError> {
        let real = crate::migration::authority::real_database_path(self.supplied)
            .map_err(|_| OwnedOpenError::PathIdentityChanged)?;
        let identity = crate::migration::authority::database_file_identity(self.supplied)
            .map_err(|_| OwnedOpenError::PathIdentityChanged)?;
        if real != self.real || identity != *handle.identity() {
            return Err(OwnedOpenError::PathIdentityChanged);
        }
        Ok(())
    }
}

trait MigrationDriver {
    fn migrate(
        &self,
        path: &Path,
        plan: &MigrationPlan,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        clock: &dyn ClockAuthority,
    ) -> Result<MigrationOutcome, MigrationError>;

    fn hook(&self, _point: OpenHook, _path: &Path) -> Result<(), MigrationError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenHook {
    AfterClassification,
    BeforeMigration,
    AfterMigration,
    BeforeFinalOpen,
    FreshCreated,
    FreshSqliteOpened,
    FreshMigrationRecorded(usize),
    FreshBeforeCommit,
    FreshAfterCommit,
}

struct ProductionMigrationDriver;

impl MigrationDriver for ProductionMigrationDriver {
    fn migrate(
        &self,
        path: &Path,
        plan: &MigrationPlan,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        clock: &dyn ClockAuthority,
    ) -> Result<MigrationOutcome, MigrationError> {
        let migrations = ProductionMigrations::load()?;
        apply_authorized_with_clock(
            path,
            engine_version,
            migrations.as_slice().as_slice(),
            clock,
            plan,
            handle,
        )
    }
}

impl EventStore {
    pub fn open_owned(
        path: &Path,
        engine_version: &str,
        authority: OwnedOpenAuthority,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let ids = OsRandomIds::seed().map_err(WriteError::from)?;
        Self::open_owned_with_authorities(path, engine_version, authority, &SystemClock, &ids)
    }

    pub fn open_owned_with_authorities(
        path: &Path,
        engine_version: &str,
        authority: OwnedOpenAuthority,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        Self::open_owned_with_driver(
            path,
            engine_version,
            authority,
            clock,
            ids,
            &ProductionMigrationDriver,
        )
    }

    fn open_owned_with_driver(
        path: &Path,
        engine_version: &str,
        authority: OwnedOpenAuthority,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
        driver: &dyn MigrationDriver,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let store_path = StorePath::resolve(path)?;
        let path = store_path.real.as_path();
        // Ready opens verify ownership, schema and compatibility without walking
        // every historical page. Explicit first contact still audits the DB.
        let observation = match &authority {
            OwnedOpenAuthority::OpenReady => preflight_metadata_observed(path),
            _ => preflight_observed(path),
        };
        driver.hook(OpenHook::AfterClassification, path)?;
        match observation.readiness {
            StoreReadiness::Unreadable(error) => Err(OwnedOpenError::Unreadable(Box::new(error))),
            StoreReadiness::QuarantinedOrCorrupt(obstruction) => {
                Err(OwnedOpenError::QuarantinedOrCorrupt(Box::new(obstruction)))
            }
            // Not reachable today: this classification asks only for Metadata or
            // a whole-database walk, and only a bounded walk returns Slow. It is
            // still handled as usable rather than asserted: a launcher that
            // panics is a worse failure than the slow check it would report, and
            // this stays correct if a future caller ever passes a bounded walk.
            StoreReadiness::Slow => match authority {
                OwnedOpenAuthority::OpenReady => {
                    let handle = Self::require_handle(observation.handle)?;
                    let store = Self::open_existing(&store_path, &handle, engine_version, ids)?;
                    Ok(OwnedEventStore {
                        store,
                        receipt: OwnedOpenReceipt::OpenedReady,
                    })
                }
                other => Err(Self::authority_mismatch(StoreReadiness::Slow, other)),
            },
            StoreReadiness::Ready => match authority {
                OwnedOpenAuthority::OpenReady => {
                    let handle = Self::require_handle(observation.handle)?;
                    let store = Self::open_existing(&store_path, &handle, engine_version, ids)?;
                    Ok(OwnedEventStore {
                        store,
                        receipt: OwnedOpenReceipt::OpenedReady,
                    })
                }
                other => Err(Self::authority_mismatch(StoreReadiness::Ready, other)),
            },
            StoreReadiness::HotJournal => match authority {
                OwnedOpenAuthority::OpenReady => {
                    let handle = Self::require_handle(observation.handle)?;
                    Self::recover_hot_journal_and_open(
                        &store_path,
                        &handle,
                        engine_version,
                        ids,
                        None,
                    )
                }
                other => Err(Self::authority_mismatch(StoreReadiness::HotJournal, other)),
            },
            StoreReadiness::Absent => match authority {
                OwnedOpenAuthority::Initialize => {
                    Self::initialize_and_open(path, engine_version, clock, ids, driver)
                }
                OwnedOpenAuthority::Migrate(authorized) => {
                    Err(OwnedOpenError::StaleMigrationPlan {
                        authorized: Box::new(authorized),
                        observed: Box::new(StoreReadiness::Absent),
                    })
                }
                other => Err(Self::authority_mismatch(StoreReadiness::Absent, other)),
            },
            StoreReadiness::MigrationNeeded(observed) => match authority {
                OwnedOpenAuthority::Migrate(authorized) => {
                    if observed != authorized {
                        return Err(OwnedOpenError::StaleMigrationPlan {
                            authorized: Box::new(authorized),
                            observed: Box::new(StoreReadiness::MigrationNeeded(observed)),
                        });
                    }
                    let handle = Self::require_handle(observation.handle)?;
                    Self::migrate_and_open(
                        &store_path,
                        authorized,
                        &handle,
                        engine_version,
                        clock,
                        ids,
                        driver,
                    )
                }
                other => Err(Self::authority_mismatch(
                    StoreReadiness::MigrationNeeded(observed),
                    other,
                )),
            },
        }
    }

    /// Migrate the classified store under the exact authorized plan, then
    /// open it once it classifies Ready again.
    fn migrate_and_open(
        store_path: &StorePath<'_>,
        authorized: MigrationPlan,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
        driver: &dyn MigrationDriver,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let path = store_path.real.as_path();
        driver.hook(OpenHook::BeforeMigration, store_path.supplied)?;
        store_path.still_names(handle)?;
        let migration = match driver.migrate(path, &authorized, handle, engine_version, clock) {
            Ok(migration) => migration,
            Err(MigrationError::AuthorizedPlanStale) => {
                return Err(OwnedOpenError::StaleMigrationPlan {
                    authorized: Box::new(authorized),
                    observed: Box::new(preflight_read_only(path)),
                });
            }
            Err(MigrationError::PathIdentityChanged) => {
                return Err(OwnedOpenError::PathIdentityChanged);
            }
            Err(error) => return Err(OwnedOpenError::Migration(error)),
        };
        driver.hook(OpenHook::AfterMigration, path)?;
        let ready_handle = Self::require_ready(path)?;
        // The Ready store must be the file just migrated, not another ready
        // store put in its place, or the writer would not match the backup.
        if ready_handle.identity() != handle.identity() {
            return Err(OwnedOpenError::PathIdentityChanged);
        }
        driver.hook(OpenHook::BeforeFinalOpen, store_path.supplied)?;
        let store = Self::open_existing(store_path, &ready_handle, engine_version, ids)?;
        Ok(OwnedEventStore {
            store,
            receipt: OwnedOpenReceipt::Migrated {
                authorized,
                migration,
            },
        })
    }

    /// Recovery driven with a caller-chosen page-walk bound.
    ///
    /// Only the witness calls this: the launch bound cannot be made small enough
    /// for a fixture store to exhaust, so the bound is a parameter here and a
    /// constant everywhere else.
    #[doc(hidden)]
    pub fn recover_hot_journal_with_bound(
        path: &Path,
        engine_version: &str,
        ids: &dyn IdAuthority,
        operations: i64,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let store_path = StorePath::resolve(path)?;
        let handle = crate::migration::authority::database_file_handle(&store_path.real)
            .map_err(OwnedOpenError::FileIdentity)?;
        Self::recover_hot_journal_and_open(
            &store_path,
            &handle,
            engine_version,
            ids,
            Some(operations),
        )
    }

    /// Rolls back the classified hot journal through one bounded writable
    /// connection, then re-classifies and opens like any Ready store.
    ///
    /// The single schema read IS the recovery: the SQLite pager runs the
    /// rollback journal when the first page read reaches a database a
    /// crashed writer left mid-transaction, and only a writable connection
    /// may perform it. No store semantics run on the recovery connection:
    /// no epoch stamp, no migration. The second classification is the only
    /// gate between the rollback and the returned store.
    fn recover_hot_journal_and_open(
        store_path: &StorePath<'_>,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        ids: &dyn IdAuthority,
        walk_operations: Option<i64>,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let path = store_path.real.as_path();
        store_path.still_names(handle)?;
        let sqlite =
            |error: rusqlite::Error| OwnedOpenError::HotJournalRecovery(WriteError::Sqlite(error));
        let connection = handle
            .open_sqlite(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)
            .map_err(sqlite)?;
        connection
            .busy_timeout(std::time::Duration::from_secs(10))
            .map_err(sqlite)?;
        connection
            .query_row("SELECT COUNT(*) FROM sqlite_master", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(sqlite)?;
        drop(connection);
        // SQLite's rollback is its own crash recovery and is atomic by design, so
        // the walk after it is our extra check, not part of recovery. It stays
        // bounded: a multi-minute walk must not hold the launcher's readiness.
        let observation = match walk_operations {
            Some(operations) => {
                crate::migration::authority::preflight_bounded_observation(path, operations)
            }
            None => preflight_bounded_observed(path),
        };
        match observation.readiness {
            // A spent walk budget is not a reason to refuse a store whose
            // rollback already succeeded: SQLite's recovery is the atomic part,
            // and the walk is our extra check. It opens, and the caller reports
            // the slow check.
            StoreReadiness::Ready | StoreReadiness::Slow => {
                let slow = matches!(observation.readiness, StoreReadiness::Slow);
                let ready_handle = Self::require_handle(observation.handle)?;
                if ready_handle.identity() != handle.identity() {
                    return Err(OwnedOpenError::PathIdentityChanged);
                }
                let store = Self::open_existing(store_path, &ready_handle, engine_version, ids)?;
                Ok(OwnedEventStore {
                    store,
                    receipt: OwnedOpenReceipt::RecoveredHotJournal { slow },
                })
            }
            other => Err(OwnedOpenError::PostActionNotReady(Box::new(other))),
        }
    }

    fn require_ready(path: &Path) -> Result<DatabaseFileHandle, OwnedOpenError> {
        // Post-action classification only: the action above already proved the
        // pages it touched, and migration keeps its own whole-database walk.
        let observation = preflight_metadata_observed(path);
        match observation.readiness {
            StoreReadiness::Ready => Self::require_handle(observation.handle),
            other => Err(OwnedOpenError::PostActionNotReady(Box::new(other))),
        }
    }

    fn require_handle(
        handle: Option<DatabaseFileHandle>,
    ) -> Result<DatabaseFileHandle, OwnedOpenError> {
        handle.ok_or(OwnedOpenError::PathIdentityChanged)
    }

    fn authority_mismatch(
        observed: StoreReadiness,
        authority: OwnedOpenAuthority,
    ) -> OwnedOpenError {
        OwnedOpenError::AuthorityMismatch {
            observed: Box::new(observed),
            authority: Box::new(authority),
        }
    }

    /// The one way an owned open hands over a store. The caller's path must
    /// lead to the bound file before SQLite opens it and again once the store
    /// is open and prepared, so a link changed meanwhile never hands over a
    /// store for a file the caller no longer names.
    fn open_existing(
        store_path: &StorePath<'_>,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        ids: &dyn IdAuthority,
    ) -> Result<EventStore, OwnedOpenError> {
        store_path.still_names(handle)?;
        match Self::open_existing_with_ids(&store_path.real, handle, engine_version, ids) {
            Ok(store) => {
                store_path.still_names(handle)?;
                Ok(store)
            }
            Err(ExistingOpenError::Identity(error)) => Err(OwnedOpenError::FileIdentity(error)),
            Err(ExistingOpenError::Replaced) => Err(OwnedOpenError::PathIdentityChanged),
            Err(ExistingOpenError::Sqlite(error)) => {
                Err(OwnedOpenError::Open(WriteError::Sqlite(error)))
            }
            Err(ExistingOpenError::Open(error)) => Err(OwnedOpenError::Open(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grapt_types::{IdToken, Timestamp};
    use rusqlite::Connection;
    #[cfg(windows)]
    use std::fs::OpenOptions;
    use std::path::PathBuf;

    type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

    #[derive(Debug)]
    struct FixedAuthorities;

    impl ClockAuthority for FixedAuthorities {
        fn now(&self) -> Timestamp {
            Timestamp::from_millis(1_786_060_800_123)
        }
    }

    impl IdAuthority for FixedAuthorities {
        fn mint(&self) -> IdToken {
            let Some(token) = IdToken::new("owned-open-fixed") else {
                panic!("fixed owned-open identity must be non-empty");
            };
            token
        }
    }

    enum TestDriver {
        Fail,
        AddUnknownAfterMigration,
        RecordPendingBeforeMigration,
        ReplaceReady,
        CreateLegacy,
        ReplaceAfterReady,
        /// Put a ready copy in place of the file just migrated.
        ReplaceAfterMigration,
        /// Point the caller's link at another file before the migration writes.
        #[cfg(unix)]
        RetargetLinkBeforeMigration(PathBuf),
        /// Remove the caller's link before the store is handed over.
        #[cfg(unix)]
        RemoveLinkBeforeFinalOpen,
    }

    impl MigrationDriver for TestDriver {
        fn migrate(
            &self,
            path: &Path,
            plan: &MigrationPlan,
            handle: &DatabaseFileHandle,
            engine_version: &str,
            clock: &dyn ClockAuthority,
        ) -> Result<MigrationOutcome, MigrationError> {
            if matches!(self, Self::Fail) {
                return Err(MigrationError::Backup(std::io::Error::other(
                    "injected backup failure",
                )));
            }
            let migrations = ProductionMigrations::load()?;
            apply_authorized_with_clock(
                path,
                engine_version,
                migrations.as_slice().as_slice(),
                clock,
                plan,
                handle,
            )
        }

        fn hook(&self, point: OpenHook, path: &Path) -> Result<(), MigrationError> {
            match (self, point) {
                (Self::ReplaceReady, OpenHook::AfterClassification) => {
                    replace_with_copy(path, "classified")?
                }
                (Self::CreateLegacy, OpenHook::AfterClassification) => legacy_database(path)?,
                (Self::RecordPendingBeforeMigration, OpenHook::BeforeMigration) => {
                    let migrations = ProductionMigrations::load()?;
                    let first = migrations.as_slice()[0].id();
                    let conn = Connection::open(path)?;
                    conn.execute_batch("CREATE TABLE rust_migration (id TEXT PRIMARY KEY, time_completed INTEGER NOT NULL);")?;
                    conn.execute(
                        "INSERT INTO rust_migration VALUES (?1, 1)",
                        [first.as_str()],
                    )?;
                }
                (Self::AddUnknownAfterMigration, OpenHook::AfterMigration) => {
                    Connection::open(path)?.execute(
                        "INSERT INTO rust_migration VALUES ('future_unknown_migration', 1)",
                        [],
                    )?;
                }
                (Self::ReplaceAfterReady, OpenHook::BeforeFinalOpen) => {
                    replace_with_copy(path, "ready")?
                }
                (Self::ReplaceAfterMigration, OpenHook::AfterMigration) => {
                    replace_with_copy(path, "migrated")?
                }
                #[cfg(unix)]
                (Self::RetargetLinkBeforeMigration(target), OpenHook::BeforeMigration) => {
                    std::fs::remove_file(path)?;
                    std::os::unix::fs::symlink(target, path)?;
                }
                #[cfg(unix)]
                (Self::RemoveLinkBeforeFinalOpen, OpenHook::BeforeFinalOpen) => {
                    std::fs::remove_file(path)?
                }
                _ => {}
            }
            Ok(())
        }
    }

    fn scratch(label: &str) -> TestResult<PathBuf> {
        let name = format!("grapt-owned-open-unit-{label}-{}", std::process::id());
        let root = std::env::temp_dir().join(name);
        if root.exists() {
            std::fs::remove_dir_all(&root)?;
        }
        std::fs::create_dir_all(&root)?;
        Ok(root)
    }

    fn legacy_database(path: &Path) -> Result<(), MigrationError> {
        Connection::open(path)?.execute_batch(
            "CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL, owner_id text);
            CREATE TABLE event (id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL, created integer NOT NULL, type text NOT NULL, data text NOT NULL);",
        )?;
        Ok(())
    }

    fn plan(path: &Path) -> MigrationPlan {
        match preflight_read_only(path) {
            StoreReadiness::MigrationNeeded(plan) => plan,
            other => panic!("expected migration plan, found {other:?}"),
        }
    }

    fn replace_with_copy(path: &Path, label: &str) -> Result<(), std::io::Error> {
        #[cfg(windows)]
        let original_creation = path.metadata()?.created()?;
        let held = path.with_extension(format!("{label}.db"));
        std::fs::rename(path, &held)?;
        std::fs::copy(held, path)?;
        #[cfg(windows)]
        {
            use std::os::windows::fs::FileTimesExt;
            let replacement = OpenOptions::new().write(true).open(path)?;
            replacement.set_times(std::fs::FileTimes::new().set_created(original_creation))?;
        }
        Ok(())
    }

    fn backup_count(root: &Path) -> TestResult<usize> {
        Ok(std::fs::read_dir(root)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_name().to_string_lossy().ends_with(".bak"))
            .count())
    }

    fn open_with(
        path: &Path,
        authority: OwnedOpenAuthority,
        driver: &dyn MigrationDriver,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        EventStore::open_owned_with_driver(
            path,
            "owned-open-test",
            authority,
            &FixedAuthorities,
            &FixedAuthorities,
            driver,
        )
    }

    fn assert_replacement_refused(result: Result<OwnedEventStore, OwnedOpenError>) {
        #[cfg(unix)]
        assert!(matches!(result, Err(OwnedOpenError::PathIdentityChanged)));
        #[cfg(windows)]
        assert!(matches!(
            result,
            Err(OwnedOpenError::Migration(MigrationError::Backup(ref error)))
                if error.kind() == std::io::ErrorKind::PermissionDenied
                    || error.raw_os_error() == Some(32)
        ));
    }

    #[test]
    fn second_ready_classification_refuses_future_journal_state() -> TestResult {
        let root = scratch("migration-refusals")?;
        let database = root.join("second-ready.db");
        legacy_database(&database)?;
        let result = open_with(
            &database,
            OwnedOpenAuthority::Migrate(plan(&database)),
            &TestDriver::AddUnknownAfterMigration,
        );
        assert!(matches!(
            result,
            Err(OwnedOpenError::PostActionNotReady(readiness))
                if matches!(
                    *readiness,
                    StoreReadiness::QuarantinedOrCorrupt(Obstruction::Migration(
                        MigrationError::UnknownRecorded(_)
                    ))
                )
        ));
        let database = root.join("change-before-migrate.db");
        legacy_database(&database)?;
        let backups_before = backup_count(&root)?;
        assert!(matches!(
            open_with(
                &database,
                OwnedOpenAuthority::Migrate(plan(&database)),
                &TestDriver::RecordPendingBeforeMigration,
            ),
            Err(OwnedOpenError::StaleMigrationPlan { .. })
        ));
        assert_eq!(backup_count(&root)?, backups_before);
        let database = root.join("migration-failure.db");
        legacy_database(&database)?;
        let before = std::fs::read(&database)?;
        assert!(matches!(
            open_with(
                &database,
                OwnedOpenAuthority::Migrate(plan(&database)),
                &TestDriver::Fail
            ),
            Err(OwnedOpenError::Migration(_))
        ));
        assert_eq!(std::fs::read(&database)?, before);
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn file_replacements_and_concurrent_creation_never_return_a_store() -> TestResult {
        let root = scratch("file-races")?;
        let ready = root.join("ready.db");
        legacy_database(&ready)?;
        drop(open_with(
            &ready,
            OwnedOpenAuthority::Migrate(plan(&ready)),
            &ProductionMigrationDriver,
        )?);
        let ready_bytes = std::fs::read(&ready)?;
        assert_replacement_refused(open_with(
            &ready,
            OwnedOpenAuthority::OpenReady,
            &TestDriver::ReplaceReady,
        ));
        #[cfg(windows)]
        {
            let held = ready.with_extension("classified.db");
            assert!(!held.exists(), "the identity lease allowed rename to start");
        }
        assert_eq!(std::fs::read(&ready)?, ready_bytes);

        let absent = root.join("absent.db");
        assert!(matches!(
            open_with(&absent, OwnedOpenAuthority::Initialize, &TestDriver::CreateLegacy),
            Err(OwnedOpenError::InitializationConflict(readiness))
                if matches!(*readiness, StoreReadiness::MigrationNeeded(_))
        ));

        let migrate = root.join("migrate.db");
        legacy_database(&migrate)?;
        assert_replacement_refused(open_with(
            &migrate,
            OwnedOpenAuthority::Migrate(plan(&migrate)),
            &TestDriver::ReplaceAfterReady,
        ));
        assert!(matches!(
            preflight_read_only(&migrate),
            StoreReadiness::Ready
        ));

        let swapped = root.join("swapped.db");
        legacy_database(&swapped)?;
        assert_replacement_refused(open_with(
            &swapped,
            OwnedOpenAuthority::Migrate(plan(&swapped)),
            &TestDriver::ReplaceAfterMigration,
        ));

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn a_link_retargeted_or_removed_during_the_open_refuses() -> TestResult {
        let root = scratch("link-races")?;
        let (real, other, link) = (
            root.join("real.db"),
            root.join("other.db"),
            root.join("link.db"),
        );
        legacy_database(&real)?;
        legacy_database(&other)?;
        std::os::unix::fs::symlink(&real, &link)?;
        let before = std::fs::read(&real)?;
        assert!(matches!(
            open_with(
                &link,
                OwnedOpenAuthority::Migrate(plan(&link)),
                &TestDriver::RetargetLinkBeforeMigration(other.clone()),
            ),
            Err(OwnedOpenError::PathIdentityChanged)
        ));
        assert_eq!(
            std::fs::read(&real)?,
            before,
            "a store behind a retargeted link was written"
        );
        assert_eq!(backup_count(&root)?, 0);
        std::fs::remove_file(&link)?;
        std::os::unix::fs::symlink(&real, &link)?;
        assert!(matches!(
            open_with(
                &link,
                OwnedOpenAuthority::Migrate(plan(&link)),
                &TestDriver::RemoveLinkBeforeFinalOpen,
            ),
            Err(OwnedOpenError::PathIdentityChanged)
        ));
        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    fn refusals_after_the_store_changes_name_a_restart_not_a_setting() {
        let plan = MigrationPlan {
            pending: Vec::new(),
            backup: crate::FutureBackup {
                location: PathBuf::from("/backups/store"),
                minimum_free_bytes: 1024,
                capacity: crate::BackupCapacity::Sufficient,
                directory: crate::BackupDirectory::Readable,
                permissions: crate::BackupPermissions {
                    create_file: crate::CreateBackupFile::Authorized,
                    write_file: crate::WriteBackupFile::Authorized,
                },
            },
        };
        let changed = [
            OwnedOpenError::StaleMigrationPlan {
                authorized: Box::new(plan.clone()),
                observed: Box::new(StoreReadiness::Ready),
            },
            OwnedOpenError::AuthorityMismatch {
                observed: Box::new(StoreReadiness::Ready),
                authority: Box::new(OwnedOpenAuthority::Initialize),
            },
            OwnedOpenError::InitializationConflict(Box::new(StoreReadiness::Ready)),
        ];
        for refusal in changed {
            let message = refusal.to_string();
            assert!(message.contains("start again"), "{message}");
            assert!(!message.contains("store.authority"), "{message}");
        }
    }
}
