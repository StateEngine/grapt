use super::*;
use std::path::PathBuf;
type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
struct Profile(PathBuf);
impl Profile {
    fn new(label: &str) -> TestResult<Self> {
        let path = std::env::temp_dir().join(format!("grapt-vault-{label}-{}", std::process::id()));
        std::fs::create_dir(&path)?;
        crate::profile_security::protect_directory(&path)?;
        Ok(Self(path))
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn key(id: &str, file: &str) -> TestResult<VaultKey> {
    Ok(VaultKey::new(
        ProviderId::new(id).ok_or("fixture connection id")?,
        file,
    )?)
}
fn secret(value: &str) -> Result<VaultSecret, VaultError> {
    VaultSecret::api_key(Zeroizing::new(value.into()))
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
fn stored_credentials_keep_connection_destination_and_payload_purpose() -> TestResult {
    let profile = Profile::new("bindings")?;
    let mut vault = SealedFileVault::open(&profile.0)?;
    let key = key("openai", "openai.credential")?;
    let api = DestinationBinding::key(ApiDestination::parse("https://api.openai.com/v1")?);
    let foreign = DestinationBinding::key(ApiDestination::parse("https://foreign.example/v1")?);
    assert!(vault.get(&key, &api)?.is_none());
    let material = secret("fixture-private-key-1234")?;
    assert!(!format!("{material:?}").contains("fixture-private"));
    vault.put(&key, &api, material, Timestamp::from_millis(42))?;
    assert!(matches!(
        vault.get(&key, &foreign),
        Err(VaultError::BindingMismatch)
    ));
    assert!(matches!(
        vault.get(
            &super::VaultKey::new(ProviderId::new("foreign").ok_or("id")?, "openai.credential")?,
            &api
        ),
        Err(VaultError::BindingMismatch)
    ));
    let loaded = vault.get(&key, &api)?.ok_or("missing credential")?;
    assert!(matches!(
        loaded.expose(),
        CredentialView::ApiKey("fixture-private-key-1234")
    ));
    api.check_api("https://api.openai.com/v1/models?limit=20")?;
    assert!(api.check_api("https://foreign.example/v1").is_err());
    assert!(api.check_refresh("https://api.openai.com/v1").is_err());
    let description = vault.describe(&key, &api)?.ok_or("missing description")?;
    assert_eq!(
        description.backend,
        if cfg!(windows) {
            BackendKind::WindowsCurrentUserEncryption
        } else {
            BackendKind::OwnerOnlyLegacyPlaintext
        }
    );
    assert_eq!(description.last4.as_deref(), Some("1234"));
    assert_eq!(description.updated, Timestamp::from_millis(42));
    assert!(vault.remove(&key)?);
    assert!(!vault.remove(&key)?);
    assert!(vault.get(&key, &api)?.is_none());

    let oauth = DestinationBinding::oauth(
        ApiDestination::parse("https://chatgpt.com/backend-api")?,
        OAuthRefreshDestination::parse("https://auth.openai.com/oauth/token")?,
    );
    let oauth_key = super::VaultKey::new(
        ProviderId::new("chatgpt").ok_or("id")?,
        "chatgpt.oauth.json",
    )?;
    vault.put(
        &oauth_key,
        &oauth,
        VaultSecret::oauth(crate::chatgpt::Tokens {
            access: "access-token".into(),
            refresh: "refresh-token".into(),
            account: "workspace-a".into(),
            expires_at: Timestamp::from_millis(1000),
        })?,
        Timestamp::from_millis(43),
    )?;
    assert!(matches!(
        vault
            .get(&oauth_key, &oauth)?
            .ok_or("missing OAuth")?
            .expose(),
        CredentialView::OAuth {
            access: "access-token",
            refresh: "refresh-token",
            account: "workspace-a",
            ..
        }
    ));
    oauth.check_refresh("https://auth.openai.com/oauth/token")?;
    assert!(oauth
        .check_refresh("https://chatgpt.com/oauth/token")
        .is_err());
    assert!(matches!(
        vault.put(
            &key,
            &oauth,
            secret("new-api-key")?,
            Timestamp::from_millis(43)
        ),
        Err(VaultError::InvalidInput)
    ));
    assert!(ApiDestination::parse("http://foreign.example").is_err());
    assert!(ApiDestination::parse("http://127.0.0.1:1234/v1").is_ok());
    assert!(ApiDestination::parse("http://127.0.0.1:1234@foreign.example/v1").is_err());
    Ok(())
}

#[test]
fn private_publication_recovers_staging_and_refuses_corruption_or_contention() -> TestResult {
    use std::io::Write;
    let profile = Profile::new("recovery")?;
    let mut vault = SealedFileVault::open(&profile.0)?;
    let key = key("fixture", "fixture.credential")?;
    let api = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    vault.put(&key, &api, secret("old-key")?, Timestamp::from_millis(1))?;
    let path = profile.0.join(".fixture.credential.new");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    let mut file = options.open(&path)?;
    crate::profile_security::protect_new_private_file(&path, &file)?;
    file.write_all(b"incomplete uncommitted publication")?;
    drop(file);
    vault.put(&key, &api, secret("new-key")?, Timestamp::from_millis(2))?;
    assert!(matches!(
        vault.get(&key, &api)?.ok_or("missing key")?.expose(),
        CredentialView::ApiKey("new-key")
    ));
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    std::os::windows::fs::OpenOptionsExt::share_mode(&mut options, 3);
    let held_lock = options.open(profile.0.join("credential-vault.lock"))?;
    held_lock.try_lock()?;
    assert!(matches!(
        vault.put(
            &key,
            &api,
            secret("blocked-key")?,
            Timestamp::from_millis(3)
        ),
        Err(VaultError::Busy)
    ));
    drop(held_lock);
    std::fs::write(profile.0.join("fixture.credential"), b"legacy-unbound-key")?;
    assert!(matches!(
        vault.get(&key, &api),
        Err(VaultError::LegacyImportRequired)
    ));
    assert!(matches!(
        vault.put(
            &key,
            &api,
            secret("replacement-key")?,
            Timestamp::from_millis(4)
        ),
        Err(VaultError::LegacyImportRequired)
    ));
    let damaged = crate::profile_security::seal(b"secode.credential-vault.v1\ninvalid JSON")?;
    crate::profile_security::replace_private_file(&profile.0, "fixture.credential", &damaged)?;
    assert!(matches!(vault.get(&key, &api), Err(VaultError::Corrupt)));
    Ok(())
}

#[test]
fn deletion_clears_owned_staging_and_preserves_foreign_links() -> TestResult {
    let profile = Profile::new("deletion")?;
    let mut vault = SealedFileVault::open(&profile.0)?;
    let key = key("fixture", "fixture.credential")?;
    let api = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    let staged = profile.0.join(".fixture.credential.new");
    let entry = profile.0.join("fixture.credential");
    for present in [true, false] {
        vault.put(
            &key,
            &api,
            secret("retained-private-key")?,
            Timestamp::from_millis(1),
        )?;
        std::fs::copy(&entry, &staged)?;
        let copied = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&staged)?;
        crate::profile_security::protect_new_private_file(&staged, &copied)?;
        drop(copied);
        if !present {
            std::fs::remove_file(&entry)?;
        }
        assert_eq!(vault.remove(&key)?, present);
        assert!(
            !staged.exists(),
            "deletion retained staged credential bytes"
        );
        assert!(vault.get(&key, &api)?.is_none());
    }
    vault.put(
        &key,
        &api,
        secret("foreign-link-key")?,
        Timestamp::from_millis(2),
    )?;
    std::fs::hard_link(&entry, &staged)?;
    assert!(vault.remove(&key).is_err());
    assert!(entry.exists());
    assert!(staged.exists());
    Ok(())
}
