//! Real pathname ABA against every owned-open route; no synthetic file identity.
use super::*;
use crate::{preflight_read_only, EventStore, OwnedOpenAuthority, StoreReadiness};
use grapt_types::{IdAuthority, IdToken, Timestamp};
use std::cell::RefCell;
use std::io;

struct Swap {
    path: PathBuf,
    stranger: PathBuf,
    held: PathBuf,
    swapped: bool,
    used: bool,
}
thread_local! {
    static SWAP: RefCell<Option<Swap>> = const { RefCell::new(None) };
}
struct SwapLease;
impl SwapLease {
    fn install(path: &Path, stranger: &Path) -> Self {
        SWAP.with(|slot| {
            assert!(slot.borrow().is_none(), "nested SQLite swap witness");
            *slot.borrow_mut() = Some(Swap {
                path: path.to_path_buf(),
                stranger: stranger.to_path_buf(),
                held: path.with_extension("held"),
                swapped: false,
                used: false,
            });
        });
        Self
    }
}
impl Drop for SwapLease {
    fn drop(&mut self) {
        let restore = after_open();
        assert!(
            restore.is_ok(),
            "could not restore owned pathname: {restore:?}"
        );
        SWAP.with(|slot| {
            slot.borrow_mut().take();
        });
    }
}
pub(super) fn before_open() -> io::Result<()> {
    SWAP.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(swap) = slot.as_mut() else {
            return Ok(());
        };
        if swap.used {
            return Err(io::Error::other("swap witness opened twice"));
        }
        std::fs::rename(&swap.path, &swap.held)?;
        swap.swapped = true;
        std::os::unix::fs::symlink(&swap.stranger, &swap.path)?;
        swap.used = true;
        Ok(())
    })
}
pub(super) fn after_open() -> io::Result<()> {
    SWAP.with(|slot| {
        let mut slot = slot.borrow_mut();
        if let Some(swap) = slot.as_mut().filter(|swap| swap.swapped) {
            std::fs::remove_file(&swap.path)?;
            std::fs::rename(&swap.held, &swap.path)?;
            swap.swapped = false;
        }
        Ok(())
    })
}

#[derive(Debug)]
struct Authorities;
impl ClockAuthority for Authorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_786_060_800_123)
    }
}
impl IdAuthority for Authorities {
    fn mint(&self) -> IdToken {
        match IdToken::new("mac04-swap-witness") {
            Some(id) => id,
            None => panic!("fixed identity is empty"),
        }
    }
}
fn legacy(path: &Path) -> Result<(), rusqlite::Error> {
    rusqlite::Connection::open(path)?.execute_batch(
        "CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL, owner_id text);
         CREATE TABLE event (id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL, created integer NOT NULL, type text NOT NULL, data text NOT NULL);
         CREATE TABLE session (id text PRIMARY KEY, project_id text NOT NULL, workspace_id text, parent_id text, directory text NOT NULL, title text NOT NULL, time_updated integer NOT NULL);
         INSERT INTO event_sequence VALUES ('ses_mac04_swap', 0, NULL);
         INSERT INTO event VALUES ('evt_mac04_swap', 'ses_mac04_swap', 0, 1, 'session.created.1', '{}');
         INSERT INTO session VALUES ('ses_mac04_swap', 'project', NULL, NULL, '/work', 'Owned open', 1);"
    )
}
fn witness(stage: &str) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-mac04-{stage}-{}", std::process::id()));
    if root.try_exists()? {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir(&root)?;
    let path = root.join("owned.db");
    let stranger = root.join("stranger.db");
    let authority = match stage {
        "fresh" => {
            std::fs::File::create_new(&stranger)?;
            OwnedOpenAuthority::Initialize
        }
        "existing" => {
            for db in [&path, &stranger] {
                drop(EventStore::open_owned_with_authorities(
                    db,
                    "mac04",
                    OwnedOpenAuthority::Initialize,
                    &Authorities,
                    &Authorities,
                )?);
            }
            rusqlite::Connection::open(&stranger)?.execute_batch("DROP INDEX event_type_id_idx")?;
            OwnedOpenAuthority::OpenReady
        }
        "migration" => {
            legacy(&path)?;
            legacy(&stranger)?;
            let StoreReadiness::MigrationNeeded(plan) = preflight_read_only(&path) else {
                return Err("legacy witness did not need migration".into());
            };
            OwnedOpenAuthority::Migrate(plan)
        }
        _ => return Err("unknown MAC04 witness stage".into()),
    };
    let before = std::fs::read(&stranger)?;
    {
        let _lease = SwapLease::install(&path, &stranger);
        let error = EventStore::open_owned_with_authorities(
            &path,
            "mac04",
            authority,
            &Authorities,
            &Authorities,
        )
        .err()
        .ok_or("a stranger SQLite connection escaped owned open")?;
        assert!(
            error
                .to_string()
                .contains("database binding changed during SQLite open"),
            "{stage}: {error}"
        );
        SWAP.with(|slot| {
            assert!(
                slot.borrow()
                    .as_ref()
                    .is_some_and(|swap| swap.used && !swap.swapped),
                "swap-and-restore seam did not execute"
            )
        });
    }
    assert_eq!(
        std::fs::read(&stranger)?,
        before,
        "{stage}: a stranger was modified"
    );
    std::fs::remove_dir_all(&root)?;
    Ok(())
}
#[test]
fn mac_guard_rejects_swap_and_restore_during_fresh_open() -> Result<(), Box<dyn std::error::Error>>
{
    witness("fresh")
}
#[test]
fn mac_guard_rejects_swap_and_restore_during_existing_open(
) -> Result<(), Box<dyn std::error::Error>> {
    witness("existing")
}
#[test]
fn mac_guard_rejects_swap_and_restore_during_migration_open(
) -> Result<(), Box<dyn std::error::Error>> {
    witness("migration")
}
