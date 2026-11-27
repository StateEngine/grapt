//! Fixtures shared by the platform vertical batteries.
//!
//! A sibling MODULE, not an `include!`. Both batteries live in the same
//! integration-test crate, so they can import these directly — which is what
//! removes the duplicated inclusion and the module-wide dead_code allow that
//! including one set into two files forced.

use std::path::{Path, PathBuf};

use grapt_platform::{
    CredentialAuthority, CredentialRefusal, CredentialResolutionRequest, PlatformOwner,
    PlatformRefusal,
};
use grapt_provider::{CredentialPlacement, ProtocolFamily};
use grapt_store::{apply_migrations, EventStore, MigrationOutcome, ProductionMigrations};
use grapt_types::{
    ClockAuthority, CredentialRecordId, IdAuthority, IdToken, ResolvedModelTarget, Timestamp,
};
use rusqlite::Connection;

pub(super) type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

#[derive(Debug)]
pub(super) struct FixedAuthorities;

impl ClockAuthority for FixedAuthorities {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_787_595_200_123)
    }
}

impl IdAuthority for FixedAuthorities {
    fn mint(&self) -> IdToken {
        match IdToken::new("df0-platform-fixed-owner") {
            Some(token) => token,
            None => panic!("the fixed platform identity must be non-empty"),
        }
    }
}

pub(super) fn scratch(label: &str) -> TestResult<PathBuf> {
    let root =
        std::env::temp_dir().join(format!("grapt-df0-platform-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir_all(&root)?;
    Ok(root)
}

pub(super) fn config(
    authority: &str,
    build: &str,
    variants: &[String],
    selected: Option<&str>,
) -> String {
    config_with_coordinates(
        authority,
        build,
        variants,
        selected,
        "openai",
        "responses",
        "account-a",
        "credential-v1",
    )
}

#[allow(
    clippy::too_many_arguments,
    reason = "the witness names every independently mismatched grant coordinate"
)]
pub(super) fn config_with_coordinates(
    authority: &str,
    build: &str,
    variants: &[String],
    selected: Option<&str>,
    provider: &str,
    route: &str,
    account: &str,
    credential_version: &str,
) -> String {
    let variants = variants
        .iter()
        .map(|variant| format!("\"{variant}\""))
        .collect::<Vec<_>>()
        .join(",");
    let selected = match selected {
        Some(selected) => format!(",\"selected_variant\":\"{selected}\""),
        None => String::new(),
    };
    format!(
        "{{\n\
         \"store\":{{\"authority\":\"{authority}\"}},\n\
         \"product\":{{\"build\":\"{build}\"}},\n\
         \"provider\":{{\n\
           \"id\":\"{provider}\",\"model\":\"gpt-5.6\",\n\
           \"base_model\":\"gpt-5.6\",\"family\":\"gpt-5\",\n\
           \"route\":\"{route}\",\"account\":\"{account}\",\n\
           \"credential_version\":\"{credential_version}\",\n\
           \"policy_version\":\"df0-policy-v1\",\n\
           \"variants\":[{variants}]{selected}\n\
         }}\n}}"
    )
}

pub(super) fn write_config(
    root: &Path,
    authority: &str,
    build: &str,
    variants: &[String],
    selected: Option<&str>,
) -> TestResult {
    std::fs::write(
        root.join("grapt.jsonc"),
        config(authority, build, variants, selected),
    )?;
    std::fs::write(
        root.join("provider.credential"),
        "sentinel-provider-secret\n",
    )?;
    Ok(())
}

pub(super) fn credential_authority_and_target(
    root: &Path,
) -> TestResult<(CredentialAuthority, ResolvedModelTarget)> {
    let owner =
        PlatformOwner::prepare_with_authorities(root, &FixedAuthorities, &FixedAuthorities)?;
    let parts = owner.into_provider_parts()?;
    let (store, targets, _build, credential, _first_contact) = parts.into_components();
    drop(store);
    let target = targets.issue()?;
    drop(targets);
    Ok((credential, target))
}

pub(super) fn resolve_refusal(
    credential: CredentialAuthority,
    target: &ResolvedModelTarget,
    record: CredentialRecordId,
    protocol_family: ProtocolFamily,
    placement: CredentialPlacement,
    accepted_label: &'static str,
) -> TestResult<CredentialRefusal> {
    credential
        .resolve(CredentialResolutionRequest::new(
            target,
            record,
            protocol_family,
            placement,
        ))
        .err()
        .ok_or_else(|| format!("credential authority accepted {accepted_label}").into())
}

pub(super) fn platform_refusal(root: &Path) -> TestResult<PlatformRefusal> {
    PlatformOwner::prepare_with_authorities(root, &FixedAuthorities, &FixedAuthorities)
        .err()
        .ok_or_else(|| "platform unexpectedly accepted the credential source".into())
}

#[cfg(unix)]
pub(super) fn symlink_file(source: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, link)
}

#[cfg(unix)]
pub(super) fn symlink_dir(source: &Path, link: &Path) -> std::io::Result<()> {
    std::os::unix::fs::symlink(source, link)
}

#[cfg(windows)]
pub(super) fn symlink_dir(source: &Path, link: &Path) -> std::io::Result<()> {
    let output = std::process::Command::new("cmd.exe")
        .args(["/d", "/c", "mklink", "/J"])
        .arg(link)
        .arg(source)
        .output()?;
    if output.status.success() {
        Ok(())
    } else {
        Err(std::io::Error::other(format!(
            "junction creation failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        )))
    }
}

#[cfg(unix)]
pub(super) fn remove_source_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

#[cfg(windows)]
pub(super) fn remove_source_link(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link)
}

#[cfg(unix)]
pub(super) fn remove_link_dir(link: &Path) -> std::io::Result<()> {
    std::fs::remove_file(link)
}

#[cfg(windows)]
pub(super) fn remove_link_dir(link: &Path) -> std::io::Result<()> {
    std::fs::remove_dir(link)
}

pub(super) fn legacy_database(path: &Path) -> TestResult {
    Connection::open(path)?.execute_batch(
        "CREATE TABLE event_sequence (aggregate_id text PRIMARY KEY, seq integer NOT NULL, owner_id text);
         CREATE TABLE event (id text PRIMARY KEY, aggregate_id text NOT NULL, seq integer NOT NULL, created integer NOT NULL, type text NOT NULL, data text NOT NULL);",
    )?;
    Ok(())
}

pub(super) fn ready_database(root: &Path) -> TestResult {
    let database = root.join("database.sqlite3");
    let store = EventStore::open(&database, "df0-platform-test")?;
    drop(store);
    let migrations = ProductionMigrations::load()?;
    match apply_migrations(&database, root, "df0-platform-test", &migrations.as_slice())? {
        MigrationOutcome::Applied { .. } | MigrationOutcome::AlreadyCurrent => Ok(()),
    }
}
