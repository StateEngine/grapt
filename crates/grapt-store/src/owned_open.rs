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
         {observed:?}; in grapt.jsonc set store.authority to the directive matching that \
         state: \"initialize\" creates an absent store, \"migrate\" applies a pending \
         migration plan, \"open-ready\" opens a ready store"
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
    #[error(
        "initialized store requires explicit migration authority: {0:?}; in grapt.jsonc set \
         store.authority to \"migrate\" to apply the pending migration plan, then to \
         \"open-ready\" to open the store once it is ready"
    )]
    MigrationAuthorizationRequired(Box<MigrationPlan>),
    #[error(
        "another creator won absent-path initialization; current state is {0:?}; in \
         grapt.jsonc set store.authority to the directive matching that state (\"migrate\" \
         while a migration is pending, \"open-ready\" once the store is ready) and re-open"
    )]
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

impl OwnedOpenError {
    /// The grapt.jsonc directive matching the observed state named in a
    /// stale-plan refusal: initialize an absent store, migrate only a
    /// current pending plan, open-ready a ready store. Obstructed states
    /// keep the refusal — no directive, because none would succeed.
    fn stale_plan_advice(observed: &StoreReadiness) -> &'static str {
        match observed {
            StoreReadiness::Absent => {
                "the observed store is absent; in grapt.jsonc set \
                                      store.authority to \"initialize\" — a migration plan \
                                      applies only to an existing store"
            }
            StoreReadiness::MigrationNeeded(_) => {
                "in grapt.jsonc set store.authority to \
                                                   \"migrate\" against the current store state, \
                                                   then to \"open-ready\" once the migration \
                                                   has been applied"
            }
            StoreReadiness::Ready | StoreReadiness::Slow | StoreReadiness::HotJournal => {
                "the observed store is ready; \
                                                                   in grapt.jsonc set \
                                                                   store.authority to \
                                                                   \"open-ready\" — the stale \
                                                                   plan has nothing to apply"
            }
            StoreReadiness::Unreadable(_) | StoreReadiness::QuarantinedOrCorrupt(_) => {
                "the observed store is obstructed; the refusal stands until the obstruction \
                 is resolved"
            }
        }
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
                    let store = Self::open_existing(path, &handle, engine_version, ids)?;
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
                    let store = Self::open_existing(path, &handle, engine_version, ids)?;
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
                    Self::recover_hot_journal_and_open(path, &handle, engine_version, ids, None)
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
                    driver.hook(OpenHook::BeforeMigration, path)?;
                    let migration =
                        match driver.migrate(path, &authorized, &handle, engine_version, clock) {
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
                    driver.hook(OpenHook::BeforeFinalOpen, path)?;
                    let store = Self::open_existing(path, &ready_handle, engine_version, ids)?;
                    Ok(OwnedEventStore {
                        store,
                        receipt: OwnedOpenReceipt::Migrated {
                            authorized,
                            migration,
                        },
                    })
                }
                other => Err(Self::authority_mismatch(
                    StoreReadiness::MigrationNeeded(observed),
                    other,
                )),
            },
        }
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
        let handle = crate::migration::authority::database_file_handle(path)
            .map_err(OwnedOpenError::FileIdentity)?;
        Self::recover_hot_journal_and_open(path, &handle, engine_version, ids, Some(operations))
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
        path: &Path,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        ids: &dyn IdAuthority,
        walk_operations: Option<i64>,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
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
                let store = Self::open_existing(path, &ready_handle, engine_version, ids)?;
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

    fn open_existing(
        path: &Path,
        handle: &DatabaseFileHandle,
        engine_version: &str,
        ids: &dyn IdAuthority,
    ) -> Result<EventStore, OwnedOpenError> {
        match Self::open_existing_with_ids(path, handle, engine_version, ids) {
            Ok(store) => Ok(store),
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

        std::fs::remove_dir_all(root)?;
        Ok(())
    }

    #[test]
    #[expect(clippy::too_many_lines, reason = "legacy")]
    fn migration_authority_refusals_name_the_setting_and_the_fix() {
        use crate::{
            BackupCapacity, BackupDirectory, BackupPermissions, CreateBackupFile, FutureBackup,
            WriteBackupFile,
        };
        let Some(migration) = grapt_types::MigrationId::new("0001") else {
            panic!("test fixture migration id must be valid");
        };
        let plan = MigrationPlan {
            pending: vec![migration],
            backup: FutureBackup {
                location: PathBuf::from("/backups/store"),
                minimum_free_bytes: 1024,
                capacity: BackupCapacity::Sufficient,
                directory: BackupDirectory::Readable,
                permissions: BackupPermissions {
                    create_file: CreateBackupFile::Authorized,
                    write_file: WriteBackupFile::Authorized,
                },
            },
        };

        let required =
            OwnedOpenError::MigrationAuthorizationRequired(Box::new(plan.clone())).to_string();
        assert!(
            required.starts_with("initialized store requires explicit migration authority:"),
            "{required}"
        );
        assert!(required.contains("in grapt.jsonc set store.authority to \"migrate\" to apply the pending migration plan"), "{required}");
        assert!(
            required.contains("then to \"open-ready\" to open the store once it is ready"),
            "{required}"
        );

        // The advice follows the observed state: a ready store has no plan
        // to migrate, and pointing the user at "migrate" would refuse again
        // on the next open.
        let stale = OwnedOpenError::StaleMigrationPlan {
            authorized: Box::new(plan.clone()),
            observed: Box::new(StoreReadiness::Ready),
        }
        .to_string();
        assert!(
            stale.starts_with("migration authorization is stale:"),
            "{stale}"
        );
        assert!(
            stale.contains(
                "the observed store is ready; in grapt.jsonc set store.authority to \"open-ready\""
            ),
            "{stale}"
        );
        assert!(
            !stale.contains("store.authority to \"migrate\""),
            "a ready store has no plan to migrate: {stale}"
        );

        let stale_absent = OwnedOpenError::StaleMigrationPlan {
            authorized: Box::new(plan.clone()),
            observed: Box::new(StoreReadiness::Absent),
        }
        .to_string();
        assert!(
            stale_absent.contains(
                "the observed store is absent; in grapt.jsonc set store.authority to \"initialize\""
            ),
            "{stale_absent}"
        );
        assert!(
            !stale_absent.contains("store.authority to \"migrate\""),
            "an absent store has no plan to migrate: {stale_absent}"
        );

        let stale_pending = OwnedOpenError::StaleMigrationPlan {
            authorized: Box::new(plan.clone()),
            observed: Box::new(StoreReadiness::MigrationNeeded(plan.clone())),
        }
        .to_string();
        assert!(
            stale_pending.contains(
                "in grapt.jsonc set store.authority to \"migrate\" against the current store state"
            ),
            "{stale_pending}"
        );
        assert!(
            stale_pending.contains("then to \"open-ready\" once the migration has been applied"),
            "{stale_pending}"
        );

        let mismatch = OwnedOpenError::AuthorityMismatch {
            observed: Box::new(StoreReadiness::Ready),
            authority: Box::new(OwnedOpenAuthority::Initialize),
        }
        .to_string();
        assert!(mismatch.starts_with("owned-open authority"), "{mismatch}");
        assert!(
            mismatch.contains(
                "in grapt.jsonc set store.authority to the directive matching that state"
            ),
            "{mismatch}"
        );
        assert!(
            mismatch.contains("\"initialize\" creates an absent store"),
            "{mismatch}"
        );
        assert!(
            mismatch.contains("\"migrate\" applies a pending migration plan"),
            "{mismatch}"
        );
        assert!(
            mismatch.contains("\"open-ready\" opens a ready store"),
            "{mismatch}"
        );

        let conflict =
            OwnedOpenError::InitializationConflict(Box::new(StoreReadiness::Ready)).to_string();
        assert!(
            conflict.starts_with("another creator won absent-path initialization;"),
            "{conflict}"
        );
        assert!(
            conflict.contains(
                "in grapt.jsonc set store.authority to the directive matching that state"
            ),
            "{conflict}"
        );
        assert!(
            conflict.contains(
                "\"migrate\" while a migration is pending, \"open-ready\" once the store is ready"
            ),
            "{conflict}"
        );
        assert!(conflict.contains("and re-open"), "{conflict}");
    }
}
