//! Exclusive new-file initialization; existing-user migration is unchanged.
use super::*;
use crate::write::{FreshBoundary, FreshOpenError};
use std::fs::OpenOptions;

impl EventStore {
    pub(super) fn initialize_and_open(
        path: &Path,
        engine_version: &str,
        clock: &dyn ClockAuthority,
        ids: &dyn IdAuthority,
        driver: &dyn MigrationDriver,
    ) -> Result<OwnedEventStore, OwnedOpenError> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt;
            // Keep the retained creation handle's existing identity contract:
            // readers/writers may share it; deletion or rename may not.
            options.share_mode(0x0000_0001 | 0x0000_0002);
        }
        let created = match options.open(path) {
            Ok(created) => created,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                return Err(OwnedOpenError::InitializationConflict(Box::new(
                    preflight_read_only(path),
                )));
            }
            Err(error) => return Err(OwnedOpenError::FileIdentity(error)),
        };
        let handle =
            DatabaseFileHandle::from_file(created).map_err(OwnedOpenError::FileIdentity)?;
        driver.hook(OpenHook::FreshCreated, path)?;
        let store = Self::open_created_with_authorities(
            path,
            &handle,
            engine_version,
            clock,
            ids,
            |boundary| {
                let point = match boundary {
                    FreshBoundary::SqliteOpened => OpenHook::FreshSqliteOpened,
                    FreshBoundary::MigrationRecorded(position) => {
                        OpenHook::FreshMigrationRecorded(position)
                    }
                    FreshBoundary::BeforeCommit => OpenHook::FreshBeforeCommit,
                    FreshBoundary::AfterCommit => OpenHook::FreshAfterCommit,
                };
                driver.hook(point, path)
            },
        )
        .map_err(|error| match error {
            FreshOpenError::Existing(ExistingOpenError::Identity(error)) => {
                OwnedOpenError::FileIdentity(error)
            }
            FreshOpenError::Existing(ExistingOpenError::Replaced) => {
                OwnedOpenError::PathIdentityChanged
            }
            FreshOpenError::Existing(ExistingOpenError::Sqlite(error))
            | FreshOpenError::Sqlite(error) => OwnedOpenError::Open(WriteError::Sqlite(error)),
            FreshOpenError::Existing(ExistingOpenError::Open(error))
            | FreshOpenError::Open(error) => OwnedOpenError::Open(error),
            FreshOpenError::Migration(error) => OwnedOpenError::Migration(error),
            FreshOpenError::NotEmpty | FreshOpenError::ForeignKey => {
                OwnedOpenError::PostActionNotReady(Box::new(preflight_read_only(path)))
            }
        })?;
        // The file was just created and migrated by this call; a second full
        // walk would repeat that work, so this reclassifies metadata only.
        let observation = preflight_metadata_observed(path);
        match observation.readiness {
            StoreReadiness::Ready => {
                if Self::require_handle(observation.handle)?.identity() != handle.identity() {
                    return Err(OwnedOpenError::PathIdentityChanged);
                }
                Ok(OwnedEventStore {
                    store,
                    receipt: OwnedOpenReceipt::Initialized(MigrationOutcome::AlreadyCurrent),
                })
            }
            other => Err(OwnedOpenError::PostActionNotReady(Box::new(other))),
        }
    }
}

#[cfg(test)]
mod tests;
