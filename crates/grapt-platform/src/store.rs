use std::path::{Path, PathBuf};

use grapt_store::{
    preflight_metadata_read_only, EventStore, Obstruction, OwnedEventStore, OwnedOpenAuthority,
    OwnedOpenError, StoreReadiness, Unreadable,
};
use grapt_types::{ClockAuthority, IdAuthority};

use crate::config::StoreDirective;

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
    #[error("{}", directive_mismatch(*.directive, *.observed))]
    DirectiveMismatch {
        directive: StoreDirective,
        observed: FirstContactKind,
    },
    #[error("authority-bound owned open was refused: {0}")]
    OwnedOpen(#[from] OwnedOpenError),
}

/// One safe mismatch message for terminal and serve/window refusals. Only
/// typed classification names cross this boundary; no plan, path or cause.
fn directive_mismatch(directive: StoreDirective, observed: FirstContactKind) -> String {
    let target = match observed {
        FirstContactKind::Absent => Some("initialize"),
        FirstContactKind::MigrationNeeded => Some("migrate"),
        FirstContactKind::HotJournal | FirstContactKind::Ready => Some("open-ready"),
        FirstContactKind::Unreadable | FirstContactKind::CorruptOrQuarantined => None,
    };
    match target {
        Some(target) => format!(
            "store directive {directive:?} does not authorize first-contact state \
             {observed:?}; in grapt.jsonc set store.authority to \"{target}\" \
             to match that state"
        ),
        None => {
            "profile store could not be opened; preserve the profile and repair its store access"
                .to_owned()
        }
    }
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

    /// Open the store under `directive`, attributing first contact to
    /// `engine_version`.
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
    pub fn open(
        self,
        directive: StoreDirective,
        engine_version: &str,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let ids = grapt_store::OsRandomIds::seed()
            .map_err(grapt_store::WriteError::from)
            .map_err(OwnedOpenError::from)?;
        self.open_with_authorities(directive, engine_version, &grapt_store::SystemClock, &ids)
    }

    pub fn open_with_authorities(
        self,
        directive: StoreDirective,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        let store = self
            .authorize(directive)?
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

    pub(crate) fn authorize(
        self,
        directive: StoreDirective,
    ) -> Result<PreparedStoreOpen, PlatformStoreRefusal> {
        let observed = kind(&self.readiness);
        let authority = match (self.readiness, directive) {
            (StoreReadiness::Ready, StoreDirective::OpenReady) => OwnedOpenAuthority::OpenReady,
            (StoreReadiness::Absent, StoreDirective::Initialize) => OwnedOpenAuthority::Initialize,
            (StoreReadiness::MigrationNeeded(plan), StoreDirective::Migrate) => {
                OwnedOpenAuthority::Migrate(plan)
            }
            (StoreReadiness::HotJournal, StoreDirective::OpenReady) => {
                OwnedOpenAuthority::OpenReady
            }
            (StoreReadiness::Unreadable(error), _) => {
                return Err(PlatformStoreRefusal::Unreadable(Box::new(error)))
            }
            (StoreReadiness::QuarantinedOrCorrupt(obstruction), _) => {
                return Err(PlatformStoreRefusal::CorruptOrQuarantined(Box::new(
                    obstruction,
                )))
            }
            _ => {
                return Err(PlatformStoreRefusal::DirectiveMismatch {
                    directive,
                    observed,
                })
            }
        };
        Ok(PreparedStoreOpen {
            database: self.database,
            authority,
        })
    }
}

#[derive(Debug)]
pub(crate) struct PreparedStoreOpen {
    database: PathBuf,
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

    fn open_with_authorities(
        self,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
    ) -> Result<OwnedEventStore, PlatformStoreRefusal> {
        EventStore::open_owned_with_authorities(
            &self.database,
            engine_version,
            self.authority,
            clock,
            ids,
        )
        .map_err(PlatformStoreRefusal::OwnedOpen)
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

#[cfg(test)]
mod advice_tests {
    use super::*;

    #[test]
    fn directive_refusal_names_the_state_and_next_configuration_step() {
        for (observed, target) in [
            (FirstContactKind::Absent, "initialize"),
            (FirstContactKind::MigrationNeeded, "migrate"),
            (FirstContactKind::HotJournal, "open-ready"),
            (FirstContactKind::Ready, "open-ready"),
        ] {
            let message = PlatformStoreRefusal::DirectiveMismatch {
                directive: StoreDirective::Initialize,
                observed,
            }
            .to_string();
            assert!(
                message.contains(&format!("state {observed:?}")),
                "missing state: {message}"
            );
            assert!(
                message.contains(&format!("set store.authority to \"{target}\"")),
                "missing advice: {message}"
            );
        }
        for observed in [
            FirstContactKind::Unreadable,
            FirstContactKind::CorruptOrQuarantined,
        ] {
            let message = PlatformStoreRefusal::DirectiveMismatch {
                directive: StoreDirective::Initialize,
                observed,
            }
            .to_string();
            assert!(message.contains("preserve the profile"));
            assert!(
                !message.contains("set store.authority"),
                "obstructed state gained authority advice: {message}"
            );
        }
    }
}
