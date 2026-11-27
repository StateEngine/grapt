use super::*;
use crate::PublishedConfig;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

struct Profile(PathBuf);

#[cfg(unix)]
struct ReplacedRoot {
    current: PathBuf,
    original: PathBuf,
}
#[cfg(unix)]
impl Drop for ReplacedRoot {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.current);
        let _restored = std::fs::rename(&self.original, &self.current);
    }
}
impl Drop for Profile {
    fn drop(&mut self) {
        let _result = std::fs::remove_dir_all(&self.0);
    }
}

fn fixture(label: &str) -> TestResult<(Profile, PublishedConfig, ReloadableCredentialAuthority)> {
    let root =
        std::env::temp_dir().join(format!("grapt-reloadable-{label}-{}", std::process::id()));
    if root.exists() {
        std::fs::remove_dir_all(&root)?;
    }
    std::fs::create_dir(&root)?;
    std::fs::write(
        root.join("grapt.jsonc"),
        serde_json::json!({
            "store":{"authority":"initialize"},"product":{"build":"reloadable-tests"},
            "provider":{"id":"openai","model":"gpt-5.6","base_model":"gpt-5.6",
                "family":"gpt-5","protocol_family":"openai-responses","route":"responses","account":"account-a",
                "credential_version":"credential-v1","policy_version":"policy-v1","variants":[]}
        })
        .to_string(),
    )?;
    std::fs::write(
        root.join("account-models.json"),
        r#"{"data":[{"id":"gpt-5.6","max_model_len":200000}]}"#,
    )?;
    std::fs::write(root.join(CREDENTIAL_FILE), "first-source-key\n")?;
    let published = PublishedConfig::load(&root)?;
    let authority =
        CredentialAuthority::load(&root, published.credentials(), published.issue_target()?)?;
    let target = published.issue_target()?;
    let request = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::OpenAiResponses,
        CredentialPlacement::BearerAuthorization,
    );
    let (_initial, source) = authority.into_refreshable(request)?;
    Ok((Profile(root), published, source))
}

#[test]
fn rotation_changes_only_the_volatile_revision_and_retains_the_grant_coordinates() -> TestResult {
    let (profile, published, mut source) = fixture("rotation")?;
    let first = source.current()?;
    std::fs::write(profile.0.join(CREDENTIAL_FILE), "first-source-key\r\n")?;
    assert_eq!(
        source.current()?.revision,
        first.revision,
        "line endings changed no credential"
    );
    std::fs::write(profile.0.join(CREDENTIAL_FILE), "second-source-key\n")?;
    let next = source.current()?;
    assert_ne!(next.revision, first.revision);
    assert_eq!(next.credential.account(), published.credentials().account());
    assert_eq!(
        next.credential.credential_version(),
        published.credentials().version()
    );
    assert_eq!(
        next.credential.placement(),
        published.credentials().placement()
    );
    Ok(())
}

#[test]
fn an_absent_and_an_invalid_source_remain_distinct_during_renewal() -> TestResult {
    let (profile, _published, mut source) = fixture("source-refusals")?;
    std::fs::remove_file(profile.0.join(CREDENTIAL_FILE))?;
    assert!(matches!(source.current(), Err(CredentialRefusal::Absent)));
    std::fs::create_dir(profile.0.join(CREDENTIAL_FILE))?;
    assert!(matches!(source.current(), Err(CredentialRefusal::NotAFile)));
    std::fs::remove_dir(profile.0.join(CREDENTIAL_FILE))?;
    std::fs::write(
        profile.0.join(CREDENTIAL_FILE),
        vec![b'x'; MAX_CREDENTIAL_BYTES + 1],
    )?;
    assert!(matches!(
        source.current(),
        Err(CredentialRefusal::TooLarge { .. })
    ));
    Ok(())
}

#[test]
fn a_superseded_grant_refuses_before_reading_the_credential_source() -> TestResult {
    let (profile, published, mut source) = fixture("stale-before-source")?;
    let _initial = source.current()?;
    let path = profile.0.join("grapt.jsonc");
    std::fs::write(
        &path,
        std::fs::read_to_string(&path)?.replace("policy-v1", "policy-v2"),
    )?;
    let (_current, _reload) = published.reload(&grapt_config::StdSourceReader)?;
    std::fs::remove_file(profile.0.join(CREDENTIAL_FILE))?;
    assert!(
        matches!(
            source.current(),
            Err(CredentialRefusal::StaleCapability { .. })
        ),
        "a stale grant read the missing source before refusing its issuance"
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn a_running_source_refuses_a_replacement_directory_at_the_same_path() -> TestResult {
    let (profile, _published, mut source) = fixture("root-replacement")?;
    let _initial = source.current()?;
    let original = profile.0.with_extension("displaced");
    std::fs::rename(&profile.0, &original)?;
    let _root = ReplacedRoot {
        current: profile.0.clone(),
        original,
    };
    std::fs::create_dir(&profile.0)?;
    std::fs::write(profile.0.join(CREDENTIAL_FILE), "foreign-root-key")?;
    assert!(matches!(
        source.current(),
        Err(CredentialRefusal::RootChanged)
    ));
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn vaulted_credentials_keep_their_binding_on_initial_and_renewed_capabilities() -> TestResult {
    use grapt_connections::store::{CredentialFormat, Store};
    use grapt_login::vault::{
        ApiDestination, CredentialVault, DestinationBinding, SealedFileVault, VaultError, VaultKey,
        VaultSecret,
    };
    use grapt_types::Timestamp;
    let (profile, legacy, old_source) = fixture("bound-vault-key")?;
    drop(old_source);
    assert!(legacy.ensure_connection_store()?);
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.openai.com")?);
    let key = VaultKey::new(
        ProviderId::new("openai").ok_or("provider")?,
        "openai-vault.credential",
    )?;
    let mut vault = SealedFileVault::open(&profile.0)?;
    vault.put(
        &key,
        &binding,
        VaultSecret::api_key(String::from("first-bound-key").into())?,
        Timestamp::from_millis(10),
    )?;
    let mut records = Store::read_snapshot(&profile.0)?.ok_or("connections")?;
    let connection = records.connections.first_mut().ok_or("connection")?;
    connection.credential_file = "openai-vault.credential".into();
    connection.credential_format = CredentialFormat::Vault {
        binding: binding.clone(),
    };
    Store::open(&profile.0)?.save(&records)?;
    let published = PublishedConfig::load(&profile.0)?;
    let authority = CredentialAuthority::load(
        &profile.0,
        published.credentials(),
        published.issue_target()?,
    )?;
    let target = published.issue_target()?;
    let request = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::OpenAiResponses,
        CredentialPlacement::BearerAuthorization,
    );
    let (initial, mut source) = authority.into_refreshable(request)?;
    assert!(initial.bind_destination("https://foreign.example").is_err());
    let first = source.current()?;
    assert!(first
        .credential
        .bind_destination("https://foreign.example")
        .is_err());
    vault.put(
        &key,
        &binding,
        VaultSecret::api_key(String::from("rotated-bound-key").into())?,
        Timestamp::from_millis(20),
    )?;
    assert_ne!(source.current()?.revision, first.revision);
    // An endpoint edit cannot rebind the existing vault document.
    let connection = records.connections.first_mut().ok_or("connection")?;
    connection.endpoint = Some("https://foreign.example/v1".into());
    Store::open(&profile.0)?.save(&records)?;
    assert!(PublishedConfig::load(&profile.0).is_err());
    // An explicit vault format never guesses that raw bytes are an API key.
    vault.remove(&key)?;
    assert!(matches!(source.current(), Err(CredentialRefusal::Absent)));
    grapt_login::profile_security::replace_private_file(
        &profile.0,
        "openai-vault.credential",
        b"legacy-raw-key",
    )?;
    assert!(matches!(
        source.current(),
        Err(CredentialRefusal::Vault(VaultError::LegacyImportRequired))
    ));
    // OAuth grants read their bound state, never the obsolete access file.
    use grapt_connections::store::{AuthMethod, Family};
    use grapt_login::vault::OAuthRefreshDestination;
    let oauth_binding = DestinationBinding::oauth(
        ApiDestination::parse("https://chatgpt.com")?,
        OAuthRefreshDestination::parse(grapt_login::chatgpt::ISSUER)?,
    );
    let oauth_key = VaultKey::new(
        ProviderId::new("openai").ok_or("provider")?,
        "subscription.vault.oauth.json",
    )?;
    let oauth = |account: &str, access: &str| grapt_login::chatgpt::Tokens {
        access: access.into(),
        refresh: "refresh-fixture".into(),
        account: account.into(),
        expires_at: Timestamp::from_millis(i64::MAX),
    };
    vault.put(
        &oauth_key,
        &oauth_binding,
        VaultSecret::oauth(oauth("account-a", "oauth-first"))?,
        Timestamp::from_millis(30),
    )?;
    let connection = records.connections.first_mut().ok_or("connection")?;
    connection.endpoint = None;
    connection.family = Family::ChatgptCodex;
    connection.method = AuthMethod::OauthDevice;
    connection.oauth_file = Some("subscription.vault.oauth.json".into());
    connection.credential_format = CredentialFormat::Vault {
        binding: oauth_binding.clone(),
    };
    Store::open(&profile.0)?.save(&records)?;
    std::fs::write(
        profile.0.join("account-models.json"),
        serde_json::json!({"models":[{
            "slug":"gpt-5.6", "context_window":200000, "supported_reasoning_levels":[]
        }]})
        .to_string(),
    )?;
    let published = PublishedConfig::load(&profile.0)?;
    let authority = CredentialAuthority::load(
        &profile.0,
        published.credentials(),
        published.issue_target()?,
    )?;
    let target = published.issue_target()?;
    let request = CredentialResolutionRequest::new(
        &target,
        authority.record().clone(),
        ProtocolFamily::ChatGptCodex,
        CredentialPlacement::BearerAuthorization,
    );
    let (initial, mut source) = authority.into_refreshable(request)?;
    assert!(initial.bind_destination("https://foreign.example").is_err());
    let first = source.current()?;
    vault.put(
        &oauth_key,
        &oauth_binding,
        VaultSecret::oauth(oauth("account-a", "oauth-rotated"))?,
        Timestamp::from_millis(40),
    )?;
    let next = source.current()?;
    assert_ne!(next.revision, first.revision);
    assert!(next
        .credential
        .bind_destination("https://foreign.example")
        .is_err());
    vault.put(
        &oauth_key,
        &oauth_binding,
        VaultSecret::oauth(oauth("foreign-account", "oauth-replaced"))?,
        Timestamp::from_millis(50),
    )?;
    assert!(matches!(
        source.current(),
        Err(CredentialRefusal::AccountMismatch)
    ));
    Ok(())
}
