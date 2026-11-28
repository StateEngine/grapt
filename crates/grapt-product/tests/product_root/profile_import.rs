//! Explicit synthetic account imports through the shipped initializer and CLI.
use super::*;
use grapt_connections::store::{
    next_key_files, AuthMethod, Connections, CredentialFormat, Family, ProfileImport, Store,
};
use grapt_login::{
    chatgpt::{self, SignInFiles, Tokens},
    profile_security,
    vault::{
        ApiDestination, CredentialVault, CredentialView, DestinationBinding,
        OAuthRefreshDestination, SealedFileVault, VaultKey, VaultSecret,
    },
};
use grapt_types::{ProviderId, Timestamp};
use serde_json::{json, Value};

#[path = "profile_import/material.rs"]
mod material;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const ACCESS: &str = "SYNTHETIC_IMPORT_ACCESS_NOT_VALID";
const REFRESH: &str = "SYNTHETIC_IMPORT_REFRESH_NOT_VALID";
const KEY: &str = "SYNTHETIC_IMPORT_KEY_NOT_VALID";
const ENVIRONMENT: &str = "SYNTHETIC_IMPORT_ENV_VALUE_NOT_VALID";
const LISTING: &[u8] = br#"{"models":[{"slug":"retained-model","visibility":"list","context_window":128000,"supported_reasoning_levels":[]}]}"#;

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> TestResult<Self> {
        let root = root(label);
        reset(&root);
        let fixture = Self(root);
        let source = fixture.source();
        fs::create_dir(&source)?;
        profile_security::protect_directory(&source)?;
        profile_security::isolate_test_keystore()?;
        fixture.write(
            "grapt.jsonc",
            &serde_json::to_vec(&json!({"provider":{
                "id":"openai","model":"retained-model","base_model":"retained-model",
                "family":"retained-family","route":"test-route","account":"test-account",
                "credential_version":chatgpt::SIGN_IN_CREDENTIAL_VERSION,
                "policy_version":"test-policy","variants":[]
            }}))?,
        )?;
        chatgpt::save(
            &source,
            &Tokens {
                access: ACCESS.into(),
                refresh: REFRESH.into(),
                account: "test-account".into(),
                expires_at: Timestamp::from_millis(2_000_000_000_000),
            },
        )?;
        fixture.write("account-models.json", LISTING)?;
        fixture.write("basic.credential", b"synthetic-source:never-valid")?;
        Ok(fixture)
    }
    fn source(&self) -> PathBuf {
        self.0.join("source")
    }
    fn target(&self) -> PathBuf {
        self.0.join("destination")
    }
    fn write(&self, name: &str, bytes: &[u8]) -> TestResult {
        profile_security::replace_private_file(&self.source(), name, bytes)?;
        Ok(())
    }
    fn managed(&self, vaulted: bool, endpoint: Option<&str>) -> TestResult<Connections> {
        let source = self.source();
        let mut store = Store::open(&source)?;
        let mut records = store.migrate(&fs::read(source.join("grapt.jsonc"))?)?;
        let record = &mut records.connections[0];
        record.endpoint = endpoint.map(str::to_owned);
        if vaulted {
            let binding = DestinationBinding::oauth(
                ApiDestination::parse(endpoint.unwrap_or(chatgpt::CODEX_BACKEND))?,
                OAuthRefreshDestination::parse(chatgpt::ISSUER)?,
            );
            let (credential, listing) = next_key_files(&record.id, None, "source")?;
            record.oauth_file = Some(format!(
                "{}.oauth.json",
                credential.trim_end_matches(".credential")
            ));
            record.credential_file = credential;
            record.discovery_file = listing;
            record.credential_format = CredentialFormat::VaultedOAuth {
                binding: binding.clone(),
            };
            let commit = store
                .install_oauth_with(
                    &records,
                    &records.connections[0].id,
                    VaultSecret::oauth(chatgpt::load(&source)?.ok_or("synthetic tokens absent")?)?,
                    &binding,
                    chatgpt::ISSUER,
                    LISTING,
                    Timestamp::from_millis(123),
                    || Ok(()),
                )?
                .1;
            assert!(commit.cleanup_error.is_none() && commit.durability_error.is_none());
        } else {
            store.save(&records)?;
        }
        // Managed profiles created by connect have no legacy provider block.
        self.write("grapt.jsonc", b"{}")?;
        Ok(records)
    }
    fn initialize(&self) -> TestResult<std::process::Output> {
        Ok(Command::new(shipped_cli::product_binary())
            .args(["init-profile", "--service"])
            .arg(self.target())
            .arg("--from")
            .arg(self.source())
            .env("SYNTHETIC_IMPORT_ENV_KEY", ENVIRONMENT)
            .env(
                profile_security::TEST_KEYSTORE_VARIABLE,
                profile_security::isolate_test_keystore()?,
            )
            .stdin(Stdio::null())
            .output()?)
    }
    fn refused(&self) -> TestResult {
        let output = self.initialize()?;
        assert!(!output.status.success(), "invalid source was imported");
        assert!(!self.target().exists(), "refusal published a destination");
        no_secret(&output)?;
        assert!(!self.source().join("database.sqlite3").exists());
        Ok(())
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = fs::remove_dir_all(&self.0);
    }
}

fn no_secret(output: &std::process::Output) -> TestResult {
    for bytes in [&output.stdout, &output.stderr] {
        assert!(
            bytes.len() <= 8192,
            "initializer feedback exceeded its bound"
        );
        let text = std::str::from_utf8(bytes)?;
        for secret in [ACCESS, REFRESH, KEY, ENVIRONMENT] {
            assert!(!text.contains(secret));
        }
    }
    Ok(())
}

fn files(record: &grapt_connections::store::Connection) -> TestResult<SignInFiles> {
    Ok(SignInFiles::new(
        record.oauth_file.as_deref().ok_or("OAuth file absent")?,
        &record.credential_file,
    )?
    .with_vault(
        record.id.clone(),
        record
            .credential_format
            .binding()
            .ok_or("binding absent")?
            .clone(),
    )?
    .with_generation(record.credential_version.clone())?)
}

fn released(source: &Path, record: &grapt_connections::store::Connection) -> TestResult {
    drop(Store::open(source)?);
    drop(files(record)?.try_lock(source)?);
    Ok(())
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn managed_import_retains_bound_oauth_keys_environment_and_selected_model() -> TestResult {
    let fixture = Fixture::new("managed-import-complete")?;
    let mut records = fixture.managed(true, Some("https://api.example/v1"))?;
    let source = fixture.source();
    let mut secondary = records.connections[0].clone();
    secondary.id = ProviderId::new("secondary-key").ok_or("fixture id")?;
    secondary.family = Family::OpenaiResponses;
    secondary.method = AuthMethod::Key;
    secondary.oauth_file = None;
    let binding = DestinationBinding::key(ApiDestination::parse("https://key.example/v1")?);
    secondary.endpoint = Some("https://key.example/v1".into());
    secondary.credential_format = CredentialFormat::Vault {
        binding: binding.clone(),
    };
    (secondary.credential_file, secondary.discovery_file) =
        next_key_files(&secondary.id, None, "secondary")?;
    records.connections.push(secondary.clone());
    let mut store = Store::open(&source)?;
    let commit = store.install_key(
        &records,
        &secondary.id,
        VaultSecret::api_key(KEY.to_owned().into())?,
        &binding,
        br#"{"data":[{"id":"secondary-model"}]}"#,
        Timestamp::from_millis(456),
    )?;
    assert!(commit.cleanup_error.is_none() && commit.durability_error.is_none());
    let mut environment = secondary.clone();
    environment.id = ProviderId::new("environment-key").ok_or("fixture id")?;
    environment.method = AuthMethod::Env {
        name: "SYNTHETIC_IMPORT_ENV_KEY".into(),
    };
    environment.credential_format = CredentialFormat::LegacyFile;
    environment.credential_file = "environment.credential".into();
    environment.discovery_file = "environment-models.json".into();
    fixture.write(
        &environment.discovery_file,
        br#"{"data":[{"id":"environment-model"}]}"#,
    )?;
    records.connections.push(environment);
    store.save(&records)?;
    drop(store);
    let original = fs::read(source.join("connections.json"))?;
    let original_oauth = fs::read(
        source.join(
            records.connections[0]
                .oauth_file
                .as_deref()
                .ok_or("OAuth absent")?,
        ),
    )?;
    let output = fixture.initialize()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    no_secret(&output)?;
    let receipt: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(receipt["provider_calls"], 0);
    assert_eq!(receipt["outcome"], "configured");
    let target = fixture.target();
    assert_ne!(
        fs::read(target.join("basic.credential"))?,
        fs::read(source.join("basic.credential"))?
    );
    profile_security::verify_directory(&target)?;
    assert!(matches!(
        preflight_read_only(&target.join("database.sqlite3")),
        StoreReadiness::Ready
    ));
    let imported = Store::read_snapshot(&target)?.ok_or("destination records absent")?;
    assert_eq!(imported.default_model, records.default_model);
    assert_eq!(imported.connections.len(), 3);
    let vault = SealedFileVault::open(&target)?;
    for (before, after) in records.connections.iter().zip(&imported.connections) {
        let mut normalized = after.clone();
        normalized.credential_file = before.credential_file.clone();
        normalized.discovery_file = before.discovery_file.clone();
        normalized.oauth_file = before.oauth_file.clone();
        assert_eq!(&normalized, before);
        assert_ne!(after.credential_file, before.credential_file);
        assert_eq!(
            fs::read(source.join(&before.discovery_file))?,
            fs::read(target.join(&after.discovery_file))?
        );
        if after.method == AuthMethod::OauthDevice {
            let tokens = files(after)?.load(&target)?.ok_or("OAuth material lost")?;
            assert!(tokens.access == ACCESS && tokens.refresh == REFRESH);
            assert_eq!(tokens.account, "test-account");
            assert_eq!(tokens.expires_at.as_millis(), 2_000_000_000_000);
            assert!(
                !target.join(&after.credential_file).exists(),
                "raw access projection created"
            );
            let state = after.oauth_file.as_deref().ok_or("state absent")?;
            let description = vault
                .describe(
                    &VaultKey::new(after.id.clone(), state)?,
                    after.credential_format.binding().ok_or("binding absent")?,
                )?
                .ok_or("description absent")?;
            assert_eq!(description.updated.as_millis(), 123);
            #[cfg(windows)]
            {
                let stored = fs::read(target.join(state))?;
                assert!(profile_security::is_protected(&stored));
                assert_ne!(stored, original_oauth);
            }
        } else if after.method == AuthMethod::Key {
            let secret = vault
                .get(
                    &VaultKey::new(after.id.clone(), &after.credential_file)?,
                    &binding,
                )?
                .ok_or("key lost")?;
            assert!(matches!(secret.expose(), CredentialView::ApiKey(value) if value == KEY));
        } else {
            assert!(
                !target.join(&after.credential_file).exists(),
                "environment value captured"
            );
        }
    }
    assert_eq!(fs::read(source.join("connections.json"))?, original);
    assert_eq!(
        fs::read(
            source.join(
                records.connections[0]
                    .oauth_file
                    .as_deref()
                    .ok_or("OAuth absent")?
            )
        )?,
        original_oauth
    );
    assert!(!source.join("database.sqlite3").exists());
    released(&source, &records.connections[0])?;
    drop(Store::open(&target)?);
    Ok(())
}

#[test]
fn managed_legacy_oauth_import_retains_configured_or_default_api_origin() -> TestResult {
    for (label, endpoint) in [
        ("explicit", Some("https://api.example/custom")),
        ("default", None),
    ] {
        let fixture = Fixture::new(&format!("managed-import-origin-{label}"))?;
        let records = fixture.managed(false, endpoint)?;
        let output = fixture.initialize()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        no_secret(&output)?;
        let imported = Store::read_snapshot(&fixture.target())?.ok_or("records absent")?;
        let record = &imported.connections[0];
        let binding = record
            .credential_format
            .binding()
            .ok_or("OAuth not vaulted")?;
        binding.check_api(endpoint.unwrap_or(chatgpt::CODEX_BACKEND))?;
        binding.check_refresh(chatgpt::ISSUER)?;
        assert!(binding.check_api("https://other.example").is_err());
        assert_eq!(
            files(record)?.load(&fixture.target())?,
            chatgpt::load(&fixture.source())?
        );
        assert!(!fixture.target().join(&record.credential_file).exists());
        assert_eq!(records.connections[0].endpoint.as_deref(), endpoint);
    }
    Ok(())
}

#[test]
fn managed_import_refuses_pending_or_malformed_metadata_without_recovery() -> TestResult {
    for name in [
        ".connections.json.new",
        "connection-install.json",
        ".connection-install.json.new",
        "connection-removal.json",
        ".connection-removal.json.new",
        ".chatgpt-switch",
        "connections.json",
    ] {
        let fixture = Fixture::new(&format!(
            "managed-import-pending-{}",
            name.trim_start_matches('.')
        ))?;
        fixture.managed(true, None)?;
        let original = fs::read(fixture.source().join("grapt.jsonc"))?;
        if name.starts_with('.') {
            let path = fixture.source().join(name);
            let mut options = fs::OpenOptions::new();
            options.write(true).create_new(true);
            #[cfg(unix)]
            std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
            let mut file = options.open(&path)?;
            profile_security::protect_new_private_file(&path, &file)?;
            file.write_all(b"SYNTHETIC_IMPORT_KEY_NOT_VALID {")?;
            file.sync_all()?;
        } else {
            fixture.write(name, b"SYNTHETIC_IMPORT_KEY_NOT_VALID {")?;
        }
        fixture.refused()?;
        assert_eq!(
            fs::read(fixture.source().join(name))?,
            b"SYNTHETIC_IMPORT_KEY_NOT_VALID {"
        );
        assert_eq!(fs::read(fixture.source().join("grapt.jsonc"))?, original);
    }
    Ok(())
}

#[test]
fn managed_import_refuses_busy_writers_and_renewals_then_releases_ownership() -> TestResult {
    let fixture = Fixture::new("managed-import-busy")?;
    let records = fixture.managed(true, None)?;
    let record = &records.connections[0];
    let writer = Store::open(&fixture.source())?;
    fixture.refused()?;
    drop(writer);
    let renewal = files(record)?.try_lock(&fixture.source())?;
    fixture.refused()?;
    drop(renewal);
    released(&fixture.source(), record)?;
    let output = fixture.initialize()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    released(&fixture.source(), record)?;
    let before = fs::read(fixture.target().join("basic.credential"))?;
    let output = Command::new(shipped_cli::cli_binary())
        .env(
            profile_security::TEST_KEYSTORE_VARIABLE,
            profile_security::isolate_test_keystore()?,
        )
        .args(["code", "--profile"])
        .arg(fixture.target())
        .arg("--setup")
        .arg(fixture.source())
        .arg(&fixture.0)
        .stdin(Stdio::null())
        .output()?;
    assert!(
        !output.status.success(),
        "first-use overwrote an existing profile"
    );
    no_secret(&output)?;
    assert!(String::from_utf8_lossy(&output.stderr).contains("already published"));
    assert_eq!(fs::read(fixture.target().join("basic.credential"))?, before);
    released(&fixture.source(), record)?;
    Ok(())
}

#[test]
fn managed_import_refuses_account_generation_destination_and_missing_listing() -> TestResult {
    for fault in [
        "account",
        "generation",
        "destination",
        "listing",
        "credential",
    ] {
        let fixture = Fixture::new(&format!("managed-import-invalid-{fault}"))?;
        let mut records = fixture.managed(true, Some("https://api.example/v1"))?;
        let record = &mut records.connections[0];
        match fault {
            "account" => {
                record.account =
                    grapt_types::AccountId::new("different-account").ok_or("account")?
            }
            "destination" => record.endpoint = Some("https://different.example/v1".into()),
            "generation" => {
                fixture.write(&files(record)?.generation_file(), b"retired-generation")?
            }
            "listing" => fs::remove_file(fixture.source().join(&record.discovery_file))?,
            _ => fixture.write(
                record.oauth_file.as_deref().ok_or("state absent")?,
                b"SYNTHETIC_IMPORT_KEY_NOT_VALID {",
            )?,
        }
        if matches!(fault, "account" | "destination") {
            Store::open(&fixture.source())?.save(&records)?;
        }
        let before = fs::read(fixture.source().join("connections.json"))?;
        fixture.refused()?;
        assert_eq!(fs::read(fixture.source().join("connections.json"))?, before);
        drop(Store::open(&fixture.source())?);
        drop(files(&records.connections[0])?.try_lock(&fixture.source())?);
    }
    Ok(())
}

#[test]
fn captured_import_checks_refuse_drift_before_destination_install() -> TestResult {
    for fault in ["listing", "credential", "generation", "metadata", "pending"] {
        let fixture = Fixture::new(&format!("managed-import-drift-{fault}"))?;
        let records = fixture.managed(true, None)?;
        let record = &records.connections[0];
        let mut captured = ProfileImport::capture(&fixture.source())?.ok_or("capture absent")?;
        assert!(Store::open(&fixture.source()).is_err());
        assert!(files(record)?.try_lock(&fixture.source()).is_err());
        let name = match fault {
            "listing" => record.discovery_file.clone(),
            "credential" => record.oauth_file.clone().ok_or("state absent")?,
            "generation" => files(record)?.generation_file(),
            "metadata" => "connections.json".into(),
            _ => "connection-install.json".into(),
        };
        fixture.write(&name, b"SYNTHETIC_IMPORT_KEY_NOT_VALID {")?;
        assert!(
            captured.check().is_err(),
            "captured {fault} drift was accepted"
        );
        let destination = fixture.target();
        fs::create_dir(&destination)?;
        profile_security::protect_directory(&destination)?;
        assert!(captured.install(&destination, "drift-check").is_err());
        assert!(fs::read_dir(&destination)?.next().is_none());
        drop(captured);
        if fault == "pending" {
            fs::remove_file(fixture.source().join(&name))?;
        }
        released(&fixture.source(), record)?;
    }
    Ok(())
}

#[test]
fn legacy_api_key_and_first_use_refusal_keep_source_and_destination_ownership() -> TestResult {
    let fixture = Fixture::new("managed-import-legacy-key")?;
    let mut config: Value =
        serde_json::from_slice(&fs::read(fixture.source().join("grapt.jsonc"))?)?;
    config["provider"]["credential_version"] = "key-v1".into();
    fixture.write("grapt.jsonc", &serde_json::to_vec(&config)?)?;
    fixture.write("provider.credential", KEY.as_bytes())?;
    let output = fixture.initialize()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    no_secret(&output)?;
    assert_eq!(
        profile_security::open(
            &fs::read(fixture.target().join("provider.credential"))?,
            4096,
        )?,
        KEY.as_bytes()
    );
    assert!(!fixture.target().join(chatgpt::STATE_FILE).exists());
    assert!(!fixture.source().join("database.sqlite3").exists());
    let target = fixture.0.join("refused-first-use");
    fixture.write("connections.json", b"SYNTHETIC_IMPORT_KEY_NOT_VALID {")?;
    let output = Command::new(shipped_cli::cli_binary())
        .env(
            profile_security::TEST_KEYSTORE_VARIABLE,
            profile_security::isolate_test_keystore()?,
        )
        .args(["code", "--profile"])
        .arg(&target)
        .arg("--setup")
        .arg(fixture.source())
        .arg(&fixture.0)
        .stdin(Stdio::null())
        .output()?;
    assert!(!output.status.success());
    no_secret(&output)?;
    assert!(!target.exists());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("account connections cannot be imported")
    );
    assert_eq!(
        fs::read(fixture.source().join("provider.credential"))?,
        KEY.as_bytes()
    );
    Ok(())
}
