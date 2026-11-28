use super::*;
use grapt_types::ProviderId;
use std::path::PathBuf;

struct Profile(PathBuf);
impl Profile {
    fn new(label: &str) -> io::Result<Self> {
        let path = std::env::temp_dir().join(format!(
            "grapt-connections-store-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&path)?;
        profile_security::protect_directory(&path)?;
        profile_security::isolate_test_keystore()?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn legacy(family: &str, credential: Option<&str>) -> Vec<u8> {
    let mut provider = serde_json::json!({
        "id":"openai", "model":"retained-model", "base_model":"retained-model",
        "protocol_family":family, "account":"retained-account", "route":"responses",
        "credential_version":"chatgpt-oauth-v1", "policy_version":"policy-v1"
    });
    if let Some(credential) = credential {
        provider["credential"] = credential.into();
    }
    serde_json::json!({"provider":provider})
        .to_string()
        .into_bytes()
}

#[test]
fn key_install_recovers_each_side_of_the_metadata_commit_without_resurrection(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_login::vault::{
        ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultKey, VaultSecret,
    };
    use grapt_types::Timestamp;
    let profile = Profile::new("key-install")?;
    let mut store = Store::open(&profile.0)?;
    let mut records = store.migrate(&legacy("openai-responses", None))?;
    let connection = records.connections[0].id.clone();
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    let (first, discovery) = next_key_files(&connection, records.connections.first(), "fixture")?;
    records.connections[0].method = AuthMethod::Key;
    records.connections[0].credential_file = first.clone();
    records.connections[0].discovery_file = discovery;
    records.connections[0].oauth_file = None;
    records.connections[0].credential_format = CredentialFormat::Vault {
        binding: binding.clone(),
    };
    let secret = |value: &str| VaultSecret::api_key(value.to_owned().into());
    let listing = br#"{"data":[{"id":"retained-model"}]}"#;
    let committed = store.install_key(
        &records,
        &connection,
        secret("first-key")?,
        &binding,
        listing,
        Timestamp::from_millis(1),
    )?;
    assert!(committed.cleanup_error.is_none());
    let vault = SealedFileVault::open(&profile.0)?;
    let (next, next_listing) = next_key_files(&connection, records.connections.first(), "unused")?;
    assert_ne!(next, first);
    let intent = serde_json::json!({"connection":connection.as_str(),"credential":next});
    let next_key = VaultKey::new(connection.clone(), &next)?;
    let mut unpublished = records.clone();
    unpublished.connections[0].credential_file = next.clone();
    unpublished.connections[0].discovery_file = next_listing.clone();
    assert!(store
        .install_key_with(
            &unpublished,
            &connection,
            secret("uncommitted-key")?,
            &binding,
            listing,
            Timestamp::from_millis(2),
            || Err::<(), _>(io::Error::other("live preparation refused")),
        )
        .is_err());
    assert!(store.save(&records).is_err());
    drop(store);
    let mut store = Store::open(&profile.0)?;
    assert_eq!(store.load()?, Some(records.clone()));
    assert!(vault.get(&next_key, &binding)?.is_none());
    assert!(!profile.0.join(&next_listing).exists());
    assert!(vault
        .get(&VaultKey::new(connection.clone(), &first)?, &binding)?
        .is_some());
    records.connections[0].credential_file = next.clone();
    records.connections[0].discovery_file = next_listing.clone();
    store.install_key(
        &records,
        &connection,
        secret("second-key")?,
        &binding,
        listing,
        Timestamp::from_millis(3),
    )?;
    // The commit is durable, but its cleanup never ran before the crash.
    profile_security::replace_private_file(
        &profile.0,
        "connection-install.json",
        &serde_json::to_vec(&intent)?,
    )?;
    drop(store);
    let store = Store::open(&profile.0)?;
    assert_eq!(store.load()?, Some(records));
    assert!(vault.get(&next_key, &binding)?.is_some());
    assert!(profile.0.join(next_listing).exists());
    assert_eq!(
        next_key_files(
            &connection,
            store
                .load()?
                .as_ref()
                .and_then(|records| records.connections.first()),
            "unused"
        )?
        .0,
        first
    );
    drop(store);
    let foreign = serde_json::json!({"connection":"another-provider","credential":next});
    profile_security::replace_private_file(
        &profile.0,
        "connection-install.json",
        &serde_json::to_vec(&foreign)?,
    )?;
    assert!(Store::open(&profile.0).is_err());
    assert!(vault.get(&next_key, &binding)?.is_some());
    Ok(())
}

#[test]
fn key_install_distinguishes_visible_commits_and_rejects_oauth_before_writes(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_login::vault::{
        ApiDestination, CredentialVault, DestinationBinding, OAuthRefreshDestination,
        SealedFileVault, VaultKey, VaultSecret,
    };
    use grapt_types::Timestamp;
    for failure in [
        install::Failure::IntentSync,
        install::Failure::MetadataSync,
        install::Failure::Cleanup,
    ] {
        let profile = Profile::new(&format!("install-{failure:?}"))?;
        let mut store = Store::open(&profile.0)?;
        let original = store.migrate(&legacy("openai-responses", None))?;
        let mut records = original.clone();
        let connection = records.connections[0].id.clone();
        let (file, listing) = next_key_files(&connection, records.connections.first(), "failure")?;
        let api = ApiDestination::parse("https://api.example/v1")?;
        let binding = DestinationBinding::key(api);
        let selected = &mut records.connections[0];
        selected.credential_file = file.clone();
        selected.discovery_file = listing;
        selected.credential_format = CredentialFormat::Vault {
            binding: binding.clone(),
        };
        let oauth_binding = DestinationBinding::oauth(
            ApiDestination::parse("https://api.example/v1")?,
            OAuthRefreshDestination::parse("https://issuer.example/token")?,
        );
        selected.credential_format = CredentialFormat::Vault {
            binding: oauth_binding.clone(),
        };
        let oauth = VaultSecret::oauth(grapt_login::chatgpt::Tokens {
            access: "access-fixture".into(),
            refresh: "refresh-fixture".into(),
            account: "fixture".into(),
            expires_at: Timestamp::from_millis(1000),
        })?;
        assert!(store
            .install_key(
                &records,
                &connection,
                oauth,
                &oauth_binding,
                b"{}",
                Timestamp::from_millis(1)
            )
            .is_err());
        assert!(!profile.0.join("connection-install.json").exists());
        assert_eq!(store.load()?, Some(original.clone()));
        records.connections[0].credential_format = CredentialFormat::Vault {
            binding: binding.clone(),
        };
        store.failure = Some(failure);
        let installed = store.install_key_with(
            &records,
            &connection,
            VaultSecret::api_key("key-fixture".to_owned().into())?,
            &binding,
            br#"{"data":[{"id":"retained-model"}]}"#,
            Timestamp::from_millis(2),
            || Ok("prepared live generation"),
        );
        let committed = failure != install::Failure::IntentSync;
        if committed {
            let (prepared, outcome) = installed?;
            assert_eq!(prepared, "prepared live generation");
            assert_eq!(
                outcome.durability_error.is_some(),
                failure == install::Failure::MetadataSync
            );
            assert_eq!(
                outcome.cleanup_error.is_some(),
                failure == install::Failure::Cleanup
            );
        } else {
            assert!(installed.is_err());
        }
        assert!(profile.0.join("connection-install.json").exists());
        let expected = if committed { records } else { original };
        assert_eq!(store.load()?, Some(expected.clone()));
        let vault = SealedFileVault::open(&profile.0)?;
        assert_eq!(
            vault
                .get(&VaultKey::new(connection, &file)?, &binding)?
                .is_some(),
            committed
        );
        drop(store);
        assert_eq!(Store::open(&profile.0)?.load()?, Some(expected));
        assert!(!profile.0.join("connection-install.json").exists());
    }
    Ok(())
}

#[test]
fn migration_preserves_legacy_ids_files_and_selection_and_is_idempotent() -> io::Result<()> {
    for (index, family) in ["chatgpt-codex", "openai-responses", "anthropic-messages"]
        .iter()
        .enumerate()
    {
        let profile = Profile::new(&format!("migration-{index}"))?;
        let raw = legacy(family, None);
        std::fs::write(profile.0.join("grapt.jsonc"), &raw)?;
        std::fs::write(profile.0.join("provider.credential"), b"untouched-secret")?;
        let mut store = Store::open(&profile.0)?;
        let migrated = store.migrate(&raw)?;
        let Some(connection) = migrated.connections.first() else {
            return Err(invalid("missing migration result"));
        };
        assert_eq!(connection.id.as_str(), "openai");
        assert_eq!(connection.credential_file, "provider.credential");
        assert_eq!(connection.account.as_str(), "retained-account");
        assert_eq!(
            migrated
                .default_model
                .as_ref()
                .map(|model| model.provider.as_str()),
            Some("openai")
        );
        assert_eq!(store.migrate(b"invalid replacement config")?, migrated);
        assert_eq!(std::fs::read(profile.0.join("grapt.jsonc"))?, raw);
        assert_eq!(
            std::fs::read(profile.0.join("provider.credential"))?,
            b"untouched-secret"
        );
        assert!(
            !String::from_utf8_lossy(&std::fs::read(profile.0.join(FILE))?)
                .contains("untouched-secret")
        );
    }
    let profile = Profile::new("environment-and-accounts")?;
    let mut raw: serde_json::Value =
        serde_json::from_slice(&legacy("openai-responses", Some("env:OPENAI_API_KEY")))?;
    raw["provider"]["accounts"] = serde_json::json!([{
        "id":"anthropic", "account":"key-account", "route":"messages",
        "credential_version":"v1", "policy_version":"policy-v1",
        "protocol_family":"anthropic-messages", "credential":"anthropic.credential", "discovery":"anthropic-models.json"
    }]);
    let migrated = Store::open(&profile.0)?.migrate(raw.to_string().as_bytes())?;
    assert_eq!(
        migrated.connections[0].method,
        AuthMethod::Env {
            name: "OPENAI_API_KEY".into()
        }
    );
    assert_eq!(migrated.connections[1].id.as_str(), "anthropic");
    assert_eq!(
        migrated.connections[1].credential_file,
        "anthropic.credential"
    );
    Ok(())
}

#[test]
fn chatgpt_and_openai_have_independent_ids_and_storage() -> io::Result<()> {
    let profile = Profile::new("coexist")?;
    let mut store = Store::open(&profile.0)?;
    let mut current = store.migrate(&legacy("chatgpt-codex", None))?;
    let chatgpt = ProviderId::new("chatgpt").ok_or_else(|| invalid("test id"))?;
    current.connections[0].id = chatgpt.clone();
    if let Some(selected) = &mut current.default_model {
        selected.provider = chatgpt;
    }
    let mut api = current.connections[0].clone();
    api.id = ProviderId::new("openai").ok_or_else(|| invalid("test id"))?;
    api.family = Family::OpenaiResponses;
    api.method = AuthMethod::Key;
    api.oauth_file = None;
    api.credential_file = "openai-key.credential".into();
    api.discovery_file = "openai-key-models.json".into();
    current.connections.push(api);
    store.save(&current)?;
    assert_eq!(store.load()?, Some(current.clone()));
    current.connections[1].id = current.connections[0].id.clone();
    assert!(store.save(&current).is_err());
    assert_ne!(store.load()?, Some(current));
    Ok(())
}

#[test]
fn interrupted_publication_retains_complete_generation_and_exclusive_lock() -> io::Result<()> {
    let profile = Profile::new("interruption")?;
    let mut store = Store::open(&profile.0)?;
    let old = store.migrate(&legacy("openai-responses", None))?;
    assert!(
        matches!(Store::open(&profile.0), Err(error) if error.kind() == io::ErrorKind::WouldBlock)
    );
    let staging = profile.0.join(STAGED);
    let mut options = std::fs::OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&staging)?;
    profile_security::protect_new_private_file(&staging, &file)?;
    file.write_all(b"{incomplete")?;
    drop(file);
    drop(store);
    let mut recovered = Store::open(&profile.0)?;
    assert_eq!(recovered.load()?, Some(old));
    assert!(!staging.exists());
    // A complete rename is the commit point. The next owner sees the new
    // generation, and never reimports a changed legacy config over it.
    recovered.save(&Connections {
        connections: vec![],
        default_model: None,
    })?;
    drop(recovered);
    let mut recovered = Store::open(&profile.0)?;
    assert!(recovered
        .migrate(&legacy("chatgpt-codex", None))?
        .connections
        .is_empty());
    Ok(())
}

#[test]
fn corrupt_unreadable_and_foreign_storage_never_become_absent() -> io::Result<()> {
    let profile = Profile::new("refusals")?;
    let mut store = Store::open(&profile.0)?;
    assert!(store.load()?.is_none());
    store.save(&Connections {
        connections: vec![],
        default_model: None,
    })?;
    std::fs::write(profile.0.join(FILE), br#"{"schema":"unknown"}"#)?;
    assert!(store.load().is_err());
    assert!(store.migrate(&legacy("chatgpt-codex", None)).is_err());
    let staged = profile.0.join(STAGED);
    std::fs::create_dir(&staged)?;
    assert!(store
        .save(&Connections {
            connections: vec![],
            default_model: None
        })
        .is_err());
    assert!(staged.is_dir());
    #[cfg(unix)]
    {
        std::fs::remove_dir(&staged)?;
        std::os::unix::fs::symlink(profile.0.join(FILE), &staged)?;
        assert!(store
            .save(&Connections {
                connections: vec![],
                default_model: None
            })
            .is_err());
        assert!(std::fs::symlink_metadata(&staged)?.file_type().is_symlink());
    }
    Ok(())
}

#[path = "tests/oauth_install.rs"]
mod oauth_install;

#[test]
fn metadata_update_prepares_before_commit_and_retains_a_visible_uncertain_commit() -> io::Result<()>
{
    let profile = Profile::new("metadata-update")?;
    let mut store = Store::open(&profile.0)?;
    let before = store.migrate(&legacy("openai-responses", None))?;
    let mut after = before.clone();
    after.connections[0].label = "Renamed connection".into();
    assert!(store
        .update_with(&after, || Err::<(), _>(io::Error::other("prepare refused")))
        .is_err());
    assert_eq!(store.load()?, Some(before));
    store.failure = Some(install::Failure::MetadataSync);
    let (prepared, commit) = store.update_with(&after, || Ok("prepared generation"))?;
    assert_eq!(prepared, "prepared generation");
    assert_eq!(commit.durability_error, Some(io::ErrorKind::Other));
    assert_eq!(store.load()?, Some(after.clone()));
    drop(store);
    assert_eq!(Store::open(&profile.0)?.load()?, Some(after));
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn removal_recovers_both_sides_of_commit_without_restoring_slots_or_staging(
) -> Result<(), Box<dyn std::error::Error>> {
    let _protection = profile_security::ProtectionBatch::begin()?;
    use grapt_login::vault::{
        ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultKey, VaultSecret,
    };
    use grapt_types::Timestamp;
    let profile = Profile::new("credential-removal")?;
    let mut store = Store::open(&profile.0)?;
    let mut records = store.migrate(&legacy("openai-responses", None))?;
    let id = records.connections[0].id.clone();
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    let mut keys = Vec::new();
    for version in 0..2 {
        let (credential, discovery) =
            next_key_files(&id, records.connections.first(), "removal-fixture")?;
        records.connections[0].credential_file = credential.clone();
        records.connections[0].discovery_file = discovery;
        records.connections[0].credential_format = CredentialFormat::Vault {
            binding: binding.clone(),
        };
        store.install_key(
            &records,
            &id,
            VaultSecret::api_key(format!("key-{version}").into())?,
            &binding,
            br#"{"data":[{"id":"retained-model"}]}"#,
            Timestamp::from_millis(version),
        )?;
        keys.push(VaultKey::new(id.clone(), &credential)?);
    }
    let template = records.clone();
    let after = Connections {
        connections: Vec::new(),
        default_model: None,
    };
    let intent = serde_json::to_vec(
        &serde_json::json!({"connection":id.as_str(),"before":String::from_utf8(records.to_vec()?)?,"after":String::from_utf8(after.to_vec()?)?}),
    )?;
    profile_security::replace_private_file(&profile.0, "connection-removal.json", &intent)?;
    drop(store);
    let mut store = Store::open(&profile.0)?;
    assert_eq!(store.load()?, Some(records.clone()));
    let vault = SealedFileVault::open(&profile.0)?;
    for key in &keys {
        assert!(vault.get(key, &binding)?.is_some());
    }
    assert!(store
        .remove_with(&id, &after, || Err::<(), _>(io::Error::other(
            "prepare refused"
        )))
        .is_err());
    assert_eq!(store.load()?, Some(records));
    store.failure = Some(install::Failure::MetadataSync);
    let (prepared, commit) = store.remove_with(&id, &after, || Ok("providerless generation"))?;
    assert_eq!(prepared, "providerless generation");
    assert!(commit.durability_error.is_some());
    for key in &keys {
        assert!(vault.get(key, &binding)?.is_some());
    }
    let name = ".connect-removal-fixture-a.credential.new";
    let stored = std::fs::read(profile.0.join("connect-removal-fixture-a.credential"))?;
    let staged = profile.0.join(name);
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&staged)?;
    profile_security::protect_new_private_file(&staged, &file)?;
    file.write_all(&stored)?;
    file.sync_all()?;
    drop(file);
    drop(store);
    let recovered = Store::open(&profile.0)?;
    assert_eq!(recovered.load()?, Some(after.clone()));
    for key in &keys {
        assert!(vault.get(key, &binding)?.is_none());
    }
    assert!(!profile.0.join(name).exists());
    drop(recovered);
    assert_eq!(Store::open(&profile.0)?.load()?, Some(after.clone()));
    for cycle in 0..4 {
        let mut records = template.clone();
        let (credential, discovery) = next_key_files(&id, None, &format!("removal-cycle-{cycle}"))?;
        records.connections[0].credential_file = credential;
        records.connections[0].discovery_file = discovery;
        let mut store = Store::open(&profile.0)?;
        store.install_key(
            &records,
            &id,
            VaultSecret::api_key("reconnect-key".to_owned().into())?,
            &binding,
            br#"{"data":[{"id":"retained-model"}]}"#,
            Timestamp::from_millis(10 + cycle),
        )?;
        let (_, commit) = store.remove_with(&id, &after, || Ok(()))?;
        assert!(commit.cleanup_error.is_none());
        let mut files = std::fs::read_dir(&profile.0)?;
        for entry in files.by_ref().take(128) {
            let name = entry?.file_name();
            let name = name.to_string_lossy();
            assert!(
                !name.starts_with("connect-") && !name.starts_with(".connect-"),
                "retired namespace left auxiliary files"
            );
        }
        assert!(
            files.next().is_none(),
            "profile fixture exceeded its file bound"
        );
    }
    Ok(())
}

#[test]
fn environment_install_and_removal_recover_without_ever_owning_an_exported_value(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_login::vault::{ApiDestination, DestinationBinding};
    let profile = Profile::new("environment-install")?;
    let mut store = Store::open(&profile.0)?;
    let before = store.migrate(&legacy("openai-responses", Some("env:OPENAI_API_KEY")))?;
    let mut after = before.clone();
    let id = after.connections[0].id.clone();
    let (credential, listing) = next_key_files(&id, after.connections.first(), "env-fixture")?;
    let selected = &mut after.connections[0];
    selected.credential_file = credential.clone();
    selected.discovery_file = listing.clone();
    selected.endpoint = Some("https://api.example/v1".into());
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    let models = br#"{"data":[{"id":"retained-model"}]}"#;
    let wrong = DestinationBinding::key(ApiDestination::parse("https://foreign.example/v1")?);
    assert!(store
        .install_environment_with(&after, &id, models, &wrong, || Ok(()))
        .is_err());
    assert!(!profile.0.join("connection-install.json").exists());
    assert!(store
        .install_environment_with(&after, &id, models, &binding, || Err::<(), _>(
            io::Error::other("live generation refused")
        ))
        .is_err());
    assert_eq!(store.load()?, Some(before));
    assert!(
        !profile.0.join(&credential).exists(),
        "an environment reference created credential material"
    );
    drop(store);
    let mut store = Store::open(&profile.0)?;
    assert!(
        !profile.0.join(&listing).exists(),
        "uncommitted listing was resurrected"
    );
    store.failure = Some(install::Failure::MetadataSync);
    let (_, commit) = store.install_environment_with(&after, &id, models, &binding, || Ok(()))?;
    assert_eq!(commit.durability_error, Some(io::ErrorKind::Other));
    assert_eq!(store.load()?, Some(after.clone()));
    assert!(profile.0.join("connection-install.json").exists());
    assert!(!profile.0.join(&credential).exists());
    drop(store);
    let mut store = Store::open(&profile.0)?;
    assert_eq!(store.load()?, Some(after));
    assert_eq!(
        profile_security::read_owner_only_file(&profile.0.join(&listing), 4096)?,
        models
    );
    assert!(!profile.0.join("connection-install.json").exists());
    // The unused historical legacy credential is not owned by an env reference.
    let mut legacy = store.migrate(&legacy("openai-responses", Some("env:OPENAI_API_KEY")))?;
    legacy.connections[0].credential_file = "provider.credential".into();
    store.save(&legacy)?;
    profile_security::replace_private_file(
        &profile.0,
        "provider.credential",
        b"unrelated legacy key",
    )?;
    let empty = Connections {
        connections: Vec::new(),
        default_model: None,
    };
    store.remove_with(&id, &empty, || Ok(()))?;
    assert_eq!(
        profile_security::read_owner_only_file(&profile.0.join("provider.credential"), 1024)?,
        b"unrelated legacy key"
    );
    assert!(!profile.0.join(listing).exists());
    drop(store);
    assert_eq!(Store::open(&profile.0)?.load()?, Some(empty));
    Ok(())
}

#[test]
fn status_observes_live_environment_without_projecting_values_or_creating_a_key(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_types::Timestamp;
    let profile = Profile::new("environment-status")?;
    let mut store = Store::open(&profile.0)?;
    store.migrate(&legacy("openai-responses", Some("env:OPENAI_API_KEY")))?;
    drop(store);
    let now = Timestamp::from_millis(1);
    let missing = crate::status::read(&profile.0, now, |_| None)?;
    assert_eq!(missing[0].status, "env-missing");
    let present = crate::status::read(&profile.0, now, |name| {
        assert_eq!(name, "OPENAI_API_KEY");
        Some("private-status-fixture".into())
    })?;
    assert_eq!(present[0].status, "ok");
    assert_eq!(present[0].backend, "environment");
    assert!(present[0].last4.is_none());
    assert!(!serde_json::to_string(&present)?.contains("private-status-fixture"));
    assert!(!profile.0.join("provider.credential").exists());
    profile_security::replace_private_file(&profile.0, FILE, b"corrupt")?;
    assert!(crate::status::read(&profile.0, now, |_| None).is_err());
    Ok(())
}

#[test]
fn managed_environment_names_preserve_catalog_case_and_digit_prefixes(
) -> Result<(), Box<dyn std::error::Error>> {
    let profile = Profile::new("environment-names")?;
    let mut store = Store::open(&profile.0)?;
    let mut records = store.migrate(&legacy("openai-responses", Some("env:OPENAI_API_KEY")))?;
    for name in ["302AI_API_KEY", "mixed_case_key"] {
        records.connections[0].method = AuthMethod::Env { name: name.into() };
        store.save(&records)?;
        assert_eq!(store.load()?, Some(records.clone()));
    }
    for name in ["../private", "KEY=value", "", "KEY\nVALUE"] {
        records.connections[0].method = AuthMethod::Env { name: name.into() };
        assert!(
            store.save(&records).is_err(),
            "invalid environment reference accepted"
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn drop_releases_the_lock_for_an_inherited_descriptor() -> io::Result<()> {
    let profile = Profile::new("drop-lock")?;
    let store = Store::open(&profile.0)?;
    // A process spawned mid-operation inherits open descriptors; this clone
    // shares the open file description and outlives the store's own close.
    let inherited = store._lock.try_clone()?;
    drop(store);
    assert!(Store::open(&profile.0).is_ok());
    drop(inherited);
    Ok(())
}

/// A reconnect: the next key slot, with its listing, under the same binding.
#[cfg(not(windows))]
fn reconnect(
    store: &mut Store,
    records: &Connections,
    binding: &grapt_login::vault::DestinationBinding,
    token: &str,
) -> Result<Connections, Box<dyn std::error::Error>> {
    let connection = records.connections[0].id.clone();
    let (file, listing) = next_key_files(&connection, records.connections.first(), token)?;
    let mut next = records.clone();
    next.connections[0].credential_file = file;
    next.connections[0].discovery_file = listing;
    next.connections[0].credential_format = CredentialFormat::Vault {
        binding: binding.clone(),
    };
    let secret = grapt_login::vault::VaultSecret::api_key(format!("key-{token}").into())?;
    let listing = br#"{"data":[{"id":"retained-model"}]}"#;
    store.install_key(
        &next,
        &connection,
        secret,
        binding,
        listing,
        grapt_types::Timestamp::from_millis(1),
    )?;
    Ok(next)
}

/// While the keystore is away, a reconnect never succeeds a sealed key with
/// an unsealed one: it refuses and the published connection stays as it was.
#[cfg(not(windows))]
#[test]
fn a_reconnect_during_a_keystore_outage_keeps_the_sealed_key(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_login::vault::{ApiDestination, DestinationBinding};
    let profile = Profile::new("reconnect-outage")?;
    let mut store = Store::open(&profile.0)?;
    let records = store.migrate(&legacy("openai-responses", None))?;
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    let sealed = reconnect(&mut store, &records, &binding, "sealed")?;
    let stored = std::fs::read(profile.0.join(&sealed.connections[0].credential_file))?;
    assert!(profile_security::is_keystore_sealed(&stored));
    let headless = profile_security::TestKeystoreUnavailable::now();
    assert!(reconnect(&mut store, &sealed, &binding, "outage").is_err());
    drop(headless);
    assert_eq!(store.load()?, Some(sealed));
    Ok(())
}
