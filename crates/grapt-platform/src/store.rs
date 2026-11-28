use std::path::{Path, PathBuf};

use grapt_store::{
    preflight_metadata_read_only, EventStore, Obstruction, OwnedEventStore, OwnedOpenAuthority,
    OwnedOpenError, StoreReadiness, Unreadable,
};
use grapt_types::{ClockAuthority, IdAuthority};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstContactKind {
    Absent,
    Unreadable,
    MigrationNeeded,
    CorruptOrQuarantined,
    /// A hot rollback journal: recoverable by the owned open, distinct
    /// from unreadable and from corrupt-or-quarantined.
    HotJournal,
    Ready,
}

#[derive(Debug)]
pub struct PlatformFirstContact {
    database: PathBuf,
    readiness: StoreReadiness,
}

#[derive(Debug, thiserror::Error)]
pub enum PlatformStoreRefusal {
    #[error("first-contact store classification was unreadable: {0:?}")]
    Unreadable(Box<Unreadable>),
    #[error("first-contact store classification was corrupt or quarantined: {0:?}")]
    CorruptOrQuarantined(Box<Obstruction>),
    #[error("authority-bound owned open was refused: {0}")]
    OwnedOpen(#[from] OwnedOpenError),
    /// The store went back, not forward, while it was being opened: an
    /// existing store vanished or an open one turned older. Nothing was
    /// created in its place.
    #[error(
        "the store at {database} was found as {found:?} but is now {now:?}; it was moved, \
         deleted or replaced while being opened, and no store was created in its place. Put \
         the database back, or close the other process changing it, and start again"
    )]
    Receded {
        database: PathBuf,
        found: FirstContactKind,
        now: FirstContactKind,
    },
}

impl PlatformFirstContact {
    /// Classify startup metadata, not all historical pages. The authorized
    /// initialization/migration path still performs its full inspection.
    pub fn inspect(service_root: &Path) -> Self {
        let database = service_root.join("database.sqlite3");
        let readiness = preflight_metadata_read_only(&database);
        Self {
            database,
            readiness,
        }
    }

    pub fn kind(&self) -> FirstContactKind {
        kind(&self.readiness)
    }

    /// Open the store as it was found, attributing first contact to
    /// `engine_version`: an absent store is created, one with pending
    /// migrations is migrated behind a verified backup, and a ready store, or
    /// one with a hot journal to roll back, is opened. An unreadable, corrupt
    /// or newer store refuses.
    ///
    /// `engine_version` is not diagnostic text. On an oracle-era database this call
    /// is the FIRST Rust write contact, and the store stamps the string
    /// verbatim into the `engine_epoch` attribution row alongside
    /// `PRAGMA user_version = 2` (`grapt_store::epoch`). There is no reverse
    /// path (ruling 2026-08-08), so that row is the only permanent record of
    /// which engine took ownership of a user's database, and a wrong value
    /// cannot be repaired later — a database whose attribution disagrees is
    /// `CorruptAttribution` and refuses.
    ///
    /// Callers therefore pass the install's DECLARED build attribution
    /// (`$.product.build`), never a compile-time crate version: every crate in
    /// this workspace carries the same `0.1.0`, so a crate version cannot
    /// distinguish two builds and would stamp every database ever migrated with
    /// one indistinguishable constant. This layer forwards the declared
    /// identity and does not mint one.
    pub fn open(self, engine_version: &str) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let ids = grapt_store::OsRandomIds::seed()
            .map_err(grapt_store::WriteError::from)
            .map_err(OwnedOpenError::from)?;
        self.open_with_authorities(engine_version, &grapt_store::SystemClock, &ids)
    }

    pub fn open_with_authorities(
        self,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let store = self
            .authorize()?
            .open_with_authorities(engine_version, clock, ids)?;
        // A recovered hot journal whose page walk spent its budget opened
        // without a finished check. That is a fact the operator has to see once,
        // with the way to finish it, rather than a silent degrade.
        if matches!(
            store.receipt(),
            grapt_store::OwnedOpenReceipt::RecoveredHotJournal { slow: true }
        ) {
            eprintln!(
                "store: recovered a hot journal; the page check after crash recovery did \
                 not finish in the launch window. The store opened normally, and SQLite \
                 reports a damaged page when it is read."
            );
        }
        Ok(store)
    }

    /// The authority the observed state grants, before anything is written.
    pub(crate) fn authorize(self) -> Result<PreparedStoreOpen, PlatformStoreRefusal> {
        Ok(PreparedStoreOpen {
            found: kind(&self.readiness),
            authority: authority(self.readiness)?,
            database: self.database,
        })
    }
}

/// What each observed state lets the open do. Nothing here writes.
fn authority(readiness: StoreReadiness) -> Result<OwnedOpenAuthority, PlatformStoreRefusal> {
    match readiness {
        StoreReadiness::Ready | StoreReadiness::Slow | StoreReadiness::HotJournal => {
            Ok(OwnedOpenAuthority::OpenReady)
        }
        StoreReadiness::Absent => Ok(OwnedOpenAuthority::Initialize),
        StoreReadiness::MigrationNeeded(plan) => Ok(OwnedOpenAuthority::Migrate(plan)),
        StoreReadiness::Unreadable(error) => Err(PlatformStoreRefusal::Unreadable(Box::new(error))),
        StoreReadiness::QuarantinedOrCorrupt(obstruction) => Err(
            PlatformStoreRefusal::CorruptOrQuarantined(Box::new(obstruction)),
        ),
    }
}

#[derive(Debug)]
pub(crate) struct PreparedStoreOpen {
    database: PathBuf,
    found: FirstContactKind,
    authority: OwnedOpenAuthority,
}

impl PreparedStoreOpen {
    pub(crate) fn open(
        self,
        engine_version: &str,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let ids = grapt_store::OsRandomIds::seed()
            .map_err(grapt_store::WriteError::from)
            .map_err(OwnedOpenError::from)?;
        self.open_with_authorities(engine_version, &grapt_store::SystemClock, &ids)
    }

    /// Another process may create or migrate the store between the read-only
    /// observation and this open, and a rolled-back hot journal may reveal a
    /// pending migration. The store refuses to act on a stale observation; it
    /// is observed once more and opened as it is now, but only if it moved
    /// forward, and never by creating it. A store that vanished or turned
    /// older refuses, so an existing store is never replaced by a new empty
    /// one. A second change refuses.
    fn open_with_authorities(
        self,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let open = |granted| {
            EventStore::open_owned_with_authorities(
                &self.database,
                engine_version,
                granted,
                clock,
                ids,
            )
        };
        let found = self.found;
        let error = match open(self.authority) {
            Err(error) => error,
            opened => return opened.map_err(PlatformStoreRefusal::OwnedOpen),
        };
        let Some(reported) = changed_to(&error) else {
            return Err(PlatformStoreRefusal::OwnedOpen(error));
        };
        // The furthest the store was seen to come, before and during the open.
        let high = progress(found).max(progress(reported));
        let now = preflight_metadata_read_only(&self.database);
        let now_kind = kind(&now);
        // An obstructed store refuses as itself.
        let granted = authority(now)?;
        // A retry never creates a store: one that is absent now was seen a
        // moment ago, and an empty store must not take its place.
        if matches!(granted, OwnedOpenAuthority::Initialize) || progress(now_kind) < high {
            return Err(PlatformStoreRefusal::Receded {
                database: self.database.clone(),
                found,
                now: now_kind,
            });
        }
        open(granted).map_err(PlatformStoreRefusal::OwnedOpen)
    }
}

/// The state an owned-open refusal saw when the store changed under it, or
/// `None` for a refusal that a second look would not change.
fn changed_to(error: &OwnedOpenError) -> Option<FirstContactKind> {
    match error {
        OwnedOpenError::AuthorityMismatch { observed, .. }
        | OwnedOpenError::StaleMigrationPlan { observed, .. }
        | OwnedOpenError::InitializationConflict(observed)
        | OwnedOpenError::PostActionNotReady(observed) => Some(kind(observed)),
        _ => None,
    }
}

/// How far a store has come: absent, then created with work pending (a hot
/// journal hides which), then ready. A retry may only hold or advance.
fn progress(kind: FirstContactKind) -> u8 {
    match kind {
        FirstContactKind::Absent => 0,
        FirstContactKind::MigrationNeeded | FirstContactKind::HotJournal => 1,
        FirstContactKind::Ready => 2,
        // Never compared: authority() refuses these states first.
        FirstContactKind::Unreadable | FirstContactKind::CorruptOrQuarantined => 0,
    }
}

fn kind(readiness: &StoreReadiness) -> FirstContactKind {
    match readiness {
        StoreReadiness::Absent => FirstContactKind::Absent,
        StoreReadiness::Unreadable(_) => FirstContactKind::Unreadable,
        StoreReadiness::MigrationNeeded(_) => FirstContactKind::MigrationNeeded,
        StoreReadiness::QuarantinedOrCorrupt(_) => FirstContactKind::CorruptOrQuarantined,
        StoreReadiness::HotJournal => FirstContactKind::HotJournal,
        // Not reachable today (this projection is fed by metadata or
        // whole-database classifications, and only a bounded walk returns Slow),
        // but a slow store is ready to open, and a panic here would take down the
        // launcher over a check that merely ran long.
        StoreReadiness::Slow | StoreReadiness::Ready => FirstContactKind::Ready,
    }
}
