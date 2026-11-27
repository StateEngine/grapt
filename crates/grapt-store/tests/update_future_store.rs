//! Prepare a disposable forward-migrated store for the native UPR refusal.
//! No production migration or schema is added to the shipped engine.

use std::path::PathBuf;

use grapt_store::migration::{apply_with_clock, Migration, MigrationOutcome};
use grapt_store::{migration::preflight, ProductionMigrations};
use grapt_types::{ClockAuthority, MigrationId, Timestamp};

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;
const MARKER: &[u8] = b"Grapt UPR disposable profile v1\n";

#[derive(Debug)]
struct FixtureClock;
impl ClockAuthority for FixtureClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(7)
    }
}

struct Forward(MigrationId);
impl Migration for Forward {
    fn id(&self) -> MigrationId {
        self.0.clone()
    }

    fn up(&self, transaction: &rusqlite::Transaction<'_>) -> std::result::Result<(), String> {
        transaction
            .execute_batch("CREATE TABLE upr_forward_fixture (value INTEGER NOT NULL)")
            .map_err(|error| error.to_string())
    }
}

#[test]
#[ignore = "native UPR fixture: requires GRAPT_UPR_FORWARD_PROFILE and its scratch marker"]
fn migrate_disposable_profile_beyond_the_installed_build() -> Result {
    let profile = PathBuf::from(
        std::env::var_os("GRAPT_UPR_FORWARD_PROFILE").ok_or("forward profile was not supplied")?,
    );
    let metadata = std::fs::symlink_metadata(&profile)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err("forward profile is not a real scratch directory".into());
    }
    let marker = profile.join("upr-fixture-marker");
    let metadata = std::fs::symlink_metadata(&marker)?;
    if !metadata.is_file()
        || metadata.file_type().is_symlink()
        || metadata.len() != MARKER.len() as u64
    {
        return Err("forward profile marker is missing or invalid".into());
    }
    if std::fs::read(marker)? != MARKER {
        return Err("forward profile marker is invalid".into());
    }
    let database = profile.join("database.sqlite3");
    let metadata = std::fs::symlink_metadata(&database)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 64 * 1024 * 1024
    {
        return Err("forward fixture database is nonregular or oversized".into());
    }
    if !matches!(
        preflight::preflight_read_only(&database),
        preflight::StoreReadiness::Ready
    ) {
        return Err("forward fixture requires a ready store from init-profile".into());
    }
    let backups = profile.join("upr-forward-backups");
    std::fs::create_dir(&backups)?;
    let production = ProductionMigrations::load()?;
    let forward =
        Forward(MigrationId::new("20990101_upr_forward_fixture").ok_or("invalid fixture id")?);
    let mut migrations = production.as_slice().to_vec();
    migrations.push(&forward);
    let outcome = apply_with_clock(
        &database,
        &backups,
        "UPR forward fixture",
        &migrations,
        &FixtureClock,
    )?;
    match outcome {
        MigrationOutcome::Applied { applied, backup } => {
            assert_eq!(applied, vec![forward.id()]);
            assert!(
                backup.backup.is_file(),
                "forward migration omitted its backup"
            );
            println!(
                "UPR forward migration applied; backup {}",
                backup.backup.display()
            );
        }
        MigrationOutcome::AlreadyCurrent => {
            return Err("forward fixture was already migrated".into())
        }
    }
    assert!(matches!(
        preflight::preflight_read_only(&database),
        preflight::StoreReadiness::QuarantinedOrCorrupt(preflight::Obstruction::Migration(
            grapt_store::migration::MigrationError::UnknownRecorded(_)
        ))
    ));
    Ok(())
}
