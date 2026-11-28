//! Explicitly import an existing account configuration into a NEW profile.
//! No credential search, login, provider call or existing-store migration occurs.

use std::collections::BTreeSet;
use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;

use grapt_config::{
    ConfigBounds, ConfigValue, MaxAncestorDepth, MaxDocumentBytes, MaxDocumentNodes, MaxDocuments,
    MaxValueDepth, Spanned,
};
use grapt_types::IdAuthority;

mod sign_in;
mod staging;

const CONFIG_BYTES: usize = 64 * 1_024;
const INVENTORY_BYTES: usize = 16 * 1_024 * 1_024;

/// Publish only service authentication and empty connection metadata. No
/// provider credential or target exists until a successful connect.
pub(crate) fn initialize_empty(requested: &Path) -> Result<String, String> {
    if !requested.is_absolute() {
        return Err("profile initialization requires an absolute new service path".into());
    }
    let _protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| format!("profile protection could not start: {:?}", error.kind()))?;
    let staged = staging::StagedProfile::begin(requested)?;
    let root = staged.root();
    let build = super::executable_build()?;
    // This is the same first-contact authority as explicit import, scoped to
    // this initializer's unpublished private directory. No target is needed.
    match grapt_platform::PlatformFirstContact::inspect(root)
        .open(grapt_platform::StoreDirective::Initialize, &build)
    {
        Ok(store) => drop(store),
        Err(grapt_platform::PlatformStoreRefusal::OwnedOpen(
            grapt_store::OwnedOpenError::MigrationAuthorizationRequired(_),
        )) => {
            drop(
                grapt_platform::PlatformFirstContact::inspect(root)
                    .open(grapt_platform::StoreDirective::Migrate, &build)
                    .map_err(|error| error.to_string())?,
            );
        }
        Err(error) => return Err(error.to_string()),
    }
    let config = serde_json::json!({
        "store":{"authority":"open-ready"},
        "product":{"build":build},
    });
    super::write_new(
        &root.join("grapt.jsonc"),
        &serde_json::to_vec_pretty(&config).map_err(|_| "profile configuration encoding failed")?,
    )?;
    super::write_new(&root.join("service.conf"), b"slot=product\ninstance=terminal\nenvironment=local\nversion=0.1.0\nserve_window_ms=86400000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=1\n")?;
    let ids = grapt_store::OsRandomIds::seed()
        .map_err(|_| "profile authentication identity unavailable")?;
    let authentication = format!("terminal:{}{}", ids.mint().as_str(), ids.mint().as_str());
    super::write_new(&root.join("basic.credential"), authentication.as_bytes())?;
    super::write_new(
        &root.join("model-catalog.json"),
        grapt_connections::catalog::BUNDLED,
    )?;
    grapt_connections::store::Store::open(root)
        .and_then(|mut store| {
            store.save(&grapt_connections::store::Connections {
                connections: Vec::new(),
                default_model: None,
            })
        })
        .map_err(|error| format!("empty connection metadata refused: {:?}", error.kind()))?;
    let root = staged.publish()?;
    Ok(
        serde_json::json!({"schema":"secode.init-profile.v1", "service":root,
        "outcome":"setup-required"})
        .to_string(),
    )
}

#[expect(clippy::too_many_lines, reason = "legacy")]
pub(crate) fn initialize(requested: &Path, source: &Path) -> Result<String, String> {
    if !requested.is_absolute() || !source.is_absolute() {
        return Err("setup needs absolute profile and account directory paths".into());
    }
    let _protection = grapt_login::profile_security::ProtectionBatch::begin()
        .map_err(|error| format!("profile protection could not start: {:?}", error.kind()))?;
    let source_identity =
        same_file::Handle::from_path(source).map_err(|_| "account directory cannot be retained")?;
    // A profile from before the rename is imported from its legacy file; the
    // NEW profile is always written under the current name.
    let source_spelling = super::super::legacy_names::config_spelling(source);
    let source_config = source.join(source_spelling.config_jsonc());
    let original = capture(&source_config, CONFIG_BYTES)?;
    let parsed = decode(&original)?;
    let mut managed = grapt_connections::store::ProfileImport::capture(source)
        .map_err(|error| format!("account connections cannot be imported: {error}; finish or repair the source connection operation before setup"))?;
    let (captured, sign_in) = if managed.is_some() {
        let catalog = match std::fs::symlink_metadata(source.join("model-catalog.json")) {
            Ok(_) => vec![(
                "model-catalog.json".to_owned(),
                capture(&source.join("model-catalog.json"), INVENTORY_BYTES)?,
            )],
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(error) => {
                return Err(format!(
                    "account model catalog is unreadable: {:?}",
                    error.kind()
                ))
            }
        };
        (catalog, None)
    } else {
        capture_legacy(source, &parsed)?
    };
    let build = super::executable_build()?;
    let initial = bootstrap(&original, &parsed, "initialize", &build)?;
    let staged = staging::StagedProfile::begin(requested)?;
    let root = staged.root();
    #[cfg(test)]
    tests::fresh_crash::cut(requested, "directory")?;
    super::write_new(&root.join("grapt.jsonc"), &initial)?;
    #[cfg(test)]
    tests::fresh_crash::cut(requested, "config")?;
    for (name, bytes) in &captured {
        super::write_new(&root.join(name), bytes)?;
    }
    if let Some(tokens) = &sign_in {
        grapt_login::chatgpt::save(root, tokens)
            .map_err(|error| format!("new profile sign-in could not be stored: {error}"))?;
    }
    super::write_new(&root.join("service.conf"), b"slot=product\ninstance=terminal\nenvironment=local\nversion=0.1.0\nserve_window_ms=86400000\nprovider_mode=production\nprovider_connect_ms=10000\nprovider_response_head_ms=60000\nprovider_active_attempts=1\n")?;
    let ids = grapt_store::OsRandomIds::seed()
        .map_err(|_| "profile authentication identity unavailable")?;
    let authentication = format!("terminal:{}{}", ids.mint().as_str(), ids.mint().as_str());
    super::write_new(&root.join("basic.credential"), authentication.as_bytes())?;
    if let Some(managed) = &mut managed {
        managed
            .install(root, ids.mint().as_str())
            .map_err(|error| {
                format!("new profile connection import refused: {error}; repair the source connection inputs and rerun setup")
            })?;
    }
    #[cfg(test)]
    tests::fresh_crash::cut(requested, "inputs")?;
    match grapt_platform::PlatformOwner::prepare(root) {
        Ok(platform) => drop(platform.settle()),
        Err(grapt_platform::PlatformRefusal::Store(
            grapt_platform::PlatformStoreRefusal::OwnedOpen(
                grapt_store::OwnedOpenError::MigrationAuthorizationRequired(_),
            ),
        )) => {
            publish(root, &bootstrap(&original, &parsed, "migrate", &build)?)?;
            let platform = grapt_platform::PlatformOwner::prepare(root).map_err(feedback)?;
            drop(platform.settle());
        }
        Err(error) => return Err(feedback(error)),
    }
    #[cfg(test)]
    tests::fresh_crash::cut(requested, "store")?;
    publish(root, &bootstrap(&original, &parsed, "open-ready", &build)?)?;
    #[cfg(test)]
    tests::fresh_crash::cut(requested, "final-config")?;
    // First run and explicit import publish the managed connection generation
    // within the same unpublished profile; no extra live-profile write path.
    grapt_platform::PublishedConfig::load(root)
        .map_err(|error| error.to_string())?
        .ensure_connection_store()
        .map_err(|error| error.to_string())?;
    if let Some(managed) = &managed {
        managed
            .check()
            .map_err(|_| "account changed during profile import")?;
    } else {
        let (current_files, current_sign_in) = capture_legacy(source, &parsed)?;
        if current_files != captured || current_sign_in != sign_in {
            return Err("account inputs changed during profile import; retry with a stable account directory".into());
        }
        grapt_connections::store::ProfileImport::require_absent(source)
            .map_err(|_| "account connections changed during profile import; retry with a stable account directory")?;
    }
    if same_file::Handle::from_path(source)
        .map_err(|_| "account directory changed during profile import")?
        != source_identity
        || super::super::legacy_names::config_resolution(source).spelling != source_spelling
        || capture(&source_config, CONFIG_BYTES)? != original
        || managed.is_some()
            && captured.iter().any(|(name, bytes)| {
                capture(&source.join(name), INVENTORY_BYTES).as_ref() != Ok(bytes)
            })
        || managed.is_some()
            && captured.is_empty()
            && std::fs::symlink_metadata(source.join("model-catalog.json"))
                .err()
                .is_none_or(|error| error.kind() != std::io::ErrorKind::NotFound)
    {
        return Err(
            "account inputs changed during profile import; retry with a stable account directory"
                .into(),
        );
    }
    let root = staged.publish()?;
    Ok(serde_json::json!({"schema":"secode.init-profile.v1", "service":root, "outcome":"configured", "provider_calls":0, "next":"grapt code --profile <profile> [repository]"}).to_string())
}

type CapturedLegacy = (Vec<(String, Vec<u8>)>, Option<grapt_login::chatgpt::Tokens>);

fn capture_legacy(source: &Path, parsed: &Spanned<ConfigValue>) -> Result<CapturedLegacy, String> {
    let provider = parsed
        .value()
        .get("provider")
        .ok_or("account configuration must declare provider")?;
    let provider_id = provider
        .value()
        .get("id")
        .and_then(|value| value.value().as_str())
        .ok_or("account configuration must declare a string provider.id")?;
    if provider_id == "deterministic" {
        return Err(grapt_platform::ConfigPublicationError::LocalModeRemoved.to_string());
    }
    let inventory = parsed
        .value()
        .get("catalog")
        .and_then(|catalog| catalog.value().get("source"))
        .is_some();
    let provider_listing = provider
        .value()
        .get("protocol_family")
        .and_then(|value| value.value().as_str())
        .is_some_and(|family| matches!(family, "openai-compatible" | "openai-responses"));
    // A primary whose credential is the user's exported variable
    // (`provider.credential: "env:NAME"`, J01) has no credential file to
    // capture; the platform reads the variable and validates the name.
    let referenced = provider
        .value()
        .get("credential")
        .and_then(|value| value.value().as_str())
        .is_some_and(|value| value.starts_with("env:"));
    let mut files = BTreeSet::new();
    if !referenced {
        files.insert(("provider.credential".to_owned(), 4 * 1_024));
    }
    if inventory {
        files.insert(("model-catalog.json".to_owned(), INVENTORY_BYTES));
    } else if provider_listing {
        // A local provider supplies its own listing without a composed public
        // catalog. Capture optional metadata through the same regular-file
        // and byte checks; unreadable metadata must not become absence.
        match std::fs::symlink_metadata(source.join("model-catalog.json")) {
            Ok(_) => {
                files.insert(("model-catalog.json".to_owned(), INVENTORY_BYTES));
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "account input model-catalog.json is unreadable: {:?}",
                    error.kind()
                ))
            }
        }
    }
    if inventory || provider_listing {
        files.insert(("account-models.json".to_owned(), INVENTORY_BYTES));
    }
    if let Some(accounts) = provider.value().get("accounts") {
        let accounts = accounts
            .value()
            .as_array()
            .ok_or("provider.accounts must be an array")?;
        if accounts.len() > 7 {
            return Err("provider.accounts exceeds its supported bound".into());
        }
        for account in accounts {
            for (field, suffix, maximum) in [
                ("credential", ".credential", 4 * 1_024),
                ("discovery", ".json", INVENTORY_BYTES),
            ] {
                let name = account
                    .value()
                    .get(field)
                    .and_then(|value| value.value().as_str())
                    .ok_or("each account must name its credential and discovery inputs")?;
                if name.len() > 64
                    || name.starts_with('.')
                    || name.contains("..")
                    || !name.ends_with(suffix)
                    || !name.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
                {
                    return Err("account input must be a supported plain file name".into());
                }
                if field == "credential" || inventory || provider_listing {
                    files.insert((name.to_owned(), maximum));
                }
            }
        }
    }
    // Capture only the primary and explicitly configured account inputs. Never
    // enumerate a home/profile directory looking for credentials or databases.
    let mut captured = Vec::with_capacity(files.len());
    for (name, maximum) in files {
        captured.push((name.clone(), capture(&source.join(name), maximum)?));
    }
    let sign_in = sign_in::capture(source, provider, &captured)?;
    Ok((captured, sign_in))
}

fn feedback(error: grapt_platform::PlatformRefusal) -> String {
    use grapt_platform::{ConfigPublicationError, CredentialRefusal, PlatformRefusal};
    match error {
        PlatformRefusal::Config(ConfigPublicationError::Missing { path }) => format!("account configuration is missing {path}; repair the source configuration"),
        PlatformRefusal::Config(ConfigPublicationError::WrongType { path, expected }) => format!("account configuration field {path} must be {expected}"),
        PlatformRefusal::Credential(CredentialRefusal::Absent) => "configured account credential is missing; supply the named credential file".into(),
        PlatformRefusal::Credential(CredentialRefusal::Read(_) | CredentialRefusal::Metadata(_)) => "configured account credential is unreadable; repair its access".into(),
        PlatformRefusal::Credential(_) => "configured account credential was rejected; repair the named credential file".into(),
        PlatformRefusal::Config(_) => "account/model configuration was rejected; repair the source configuration and model inventory".into(),
        PlatformRefusal::Store(_) => "new profile store could not be prepared; check storage access and rerun setup".into(),
        PlatformRefusal::HandoverBuildChanged => "product changed during setup; extract a consistent installation".into(),
    }
}

fn capture(path: &Path, maximum: usize) -> Result<Vec<u8>, String> {
    let credential = path
        .extension()
        .is_some_and(|extension| extension == "credential");
    let stored_maximum = if credential {
        grapt_login::profile_security::stored_bound(maximum)
    } else {
        maximum
    };
    let metadata = std::fs::symlink_metadata(path).map_err(|error| match error.kind() {
        std::io::ErrorKind::NotFound => format!(
            "account input {} is missing; supply the explicitly named file",
            path.display()
        ),
        kind => format!("account input {} is unreadable: {kind:?}", path.display()),
    })?;
    if !metadata.is_file() || metadata.len() > stored_maximum as u64 {
        return Err("account input must be a regular file within its supported size bound".into());
    }
    let mut bytes = Vec::new();
    File::open(path)
        .and_then(|file| file.take(stored_maximum as u64 + 1).read_to_end(&mut bytes))
        .map_err(|error| format!("account input cannot be read: {:?}", error.kind()))?;
    if bytes.len() > stored_maximum {
        return Err("account input exceeds its capture bound".into());
    }
    if credential {
        bytes = grapt_login::profile_security::open(&bytes, maximum).map_err(|error| {
            format!(
                "account credential protection failed: {}",
                grapt_login::profile_security::failure_diagnostic(&error)
            )
        })?;
    }
    Ok(bytes)
}

fn decode(raw: &[u8]) -> Result<Spanned<ConfigValue>, String> {
    let invalid = |_| "configuration decoding bounds are invalid".to_owned();
    let bounds = ConfigBounds::new(
        MaxDocumentBytes::try_new(CONFIG_BYTES).map_err(invalid)?,
        MaxAncestorDepth::try_new(1).map_err(invalid)?,
        MaxDocuments::try_new(1).map_err(invalid)?,
        MaxValueDepth::try_new(16).map_err(invalid)?,
        MaxDocumentNodes::try_new(4_096).map_err(invalid)?,
    );
    grapt_config::decode(raw, bounds)
        .map_err(|_| "account configuration is invalid JSONC; repair grapt.jsonc".into())
}

/// Replace only this new profile's store authority and executable attribution;
/// provider selection and every other configuration byte keep their meaning.
fn bootstrap(
    raw: &[u8],
    parsed: &Spanned<ConfigValue>,
    directive: &str,
    build: &str,
) -> Result<Vec<u8>, String> {
    let mut changes = Vec::new();
    for (object, field, value) in [
        ("store", "authority", directive),
        ("product", "build", build),
    ] {
        let encoded =
            serde_json::to_string(value).map_err(|_| "bootstrap input encoding failed")?;
        if let Some(parent) = parsed.value().get(object) {
            if parent.value().as_object().is_none() {
                return Err("store and product configuration must be objects".into());
            }
            if let Some(old) = parent.value().get(field) {
                changes.push((old.span().start(), old.span().end(), encoded));
            } else {
                changes.push((
                    parent.span().start() + 1,
                    parent.span().start() + 1,
                    format!("\"{field}\":{encoded},"),
                ));
            }
        } else {
            if parsed.value().as_object().is_none() {
                return Err("account configuration must be an object".into());
            }
            changes.push((
                parsed.span().start() + 1,
                parsed.span().start() + 1,
                format!("\"{object}\":{{\"{field}\":{encoded}}},"),
            ));
        }
    }
    let mut text =
        String::from_utf8(raw.to_vec()).map_err(|_| "account configuration must be UTF-8")?;
    changes.sort_by_key(|change| std::cmp::Reverse(change.0));
    for (start, end, replacement) in changes {
        if !text.is_char_boundary(start) || !text.is_char_boundary(end) || end > text.len() {
            return Err("configuration span is invalid".into());
        }
        if text
            .len()
            .saturating_sub(end - start)
            .saturating_add(replacement.len())
            > CONFIG_BYTES
        {
            return Err("profile configuration exceeds its byte bound".into());
        }
        text.replace_range(start..end, &replacement);
    }
    Ok(text.into_bytes())
}

fn publish(root: &Path, bytes: &[u8]) -> Result<(), String> {
    // Only the exclusive NEW profile's file can be rewritten during bootstrap.
    let path = root.join("grapt.jsonc");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .truncate(true)
        .open(path)
        .map_err(|_| "new profile configuration cannot be finalized")?;
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|_| "new profile configuration could not be finalized".into())
}

#[cfg(test)]
mod tests {
    use super::*;
    pub(super) mod fresh_crash;
    mod local_inventory;
    mod sign_in;

    pub(super) struct Scratch(pub(super) std::path::PathBuf);

    impl Scratch {
        pub(super) fn new(name: &str) -> Result<Self, String> {
            let ids = grapt_store::OsRandomIds::seed().map_err(|error| error.to_string())?;
            let root = std::env::temp_dir().join(format!("frc-{name}-{}", ids.mint().as_str()));
            std::fs::create_dir(&root).map_err(|error| error.to_string())?;
            grapt_login::profile_security::isolate_test_keystore()
                .map_err(|error| error.to_string())?;
            Ok(Self(root))
        }

        pub(super) fn account(&self) -> Result<std::path::PathBuf, String> {
            let source = self.0.join("account");
            std::fs::create_dir(&source).map_err(|error| error.to_string())?;
            super::super::write_new(&source.join("grapt.jsonc"), br#"{
                // Existing configuration is kept; setup owns only store/build.
                "provider":{"id":"openai","model":"test-model","base_model":"test-model","family":"test-model",
                    "route":"test-route","account":"test-account","credential_version":"test-v1","policy_version":"test-policy","variants":[]},
                "precise":9007199254740993,"explicit_null":null,
            }"#)?;
            super::super::write_new(
                &source.join("provider.credential"),
                b"FRC_DUMMY_CREDENTIAL_NO_PROVIDER_CALL",
            )?;
            Ok(source)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _owned_fixture_cleanup = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn frc_setup_imports_existing_account_without_a_source_store_or_provider_call(
    ) -> Result<(), String> {
        let scratch = Scratch::new("import")?;
        let source = scratch.account()?;
        let before = capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?;
        let target = scratch.0.join("new-profile");
        let receipt: serde_json::Value = serde_json::from_str(&initialize(&target, &source)?)
            .map_err(|error| error.to_string())?;
        assert_eq!(receipt["provider_calls"], 0);
        assert!(!source.join("database.sqlite3").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |path: &Path| {
                std::fs::metadata(path)
                    .map(|metadata| metadata.permissions().mode() & 0o777)
                    .map_err(|error| error.to_string())
            };
            assert_eq!(mode(&target)?, 0o700, "the profile holds credentials");
            assert_eq!(mode(&target.join("provider.credential"))?, 0o600);
        }
        assert_eq!(capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?, before);
        let after = capture(&target.join("grapt.jsonc"), CONFIG_BYTES)?;
        let parsed = decode(&after)?;
        assert_eq!(
            parsed
                .value()
                .get("store")
                .and_then(|value| value.value().get("authority"))
                .and_then(|value| value.value().as_str()),
            Some("open-ready")
        );
        assert!(String::from_utf8(after)
            .map_err(|error| error.to_string())?
            .contains("\"precise\":9007199254740993,\"explicit_null\":null"));
        let platform =
            grapt_platform::PlatformOwner::prepare(&target).map_err(|error| error.to_string())?;
        assert_eq!(
            platform
                .target()
                .map_err(|error| error.to_string())?
                .provider()
                .as_str(),
            "openai"
        );
        drop(platform.settle());
        let already_configured = initialize(&target, &source)
            .err()
            .ok_or("setup overwrote an existing profile")?;
        assert!(
            already_configured.contains("already published")
                && already_configured.contains("use it"),
            "a verified complete own profile must be named as published: {already_configured}"
        );
        Ok(())
    }

    #[test]
    fn frc_setup_keeps_the_public_catalog_and_connected_model_inventory() -> Result<(), String> {
        let scratch = Scratch::new("inventory")?;
        let source = scratch.account()?;
        let mut config: serde_json::Value = serde_json::json!({
            "provider":{"id":"openai","model":"test-model","base_model":"test-model","family":"test-model",
                "route":"test-route","account":"test-account","credential_version":"test-v1","policy_version":"test-policy","variants":[]},
            "catalog":{"source":"models.dev-chatgpt"}
        });
        config["store"] = serde_json::json!({"authority":"open-ready"});
        std::fs::write(
            source.join("grapt.jsonc"),
            serde_json::to_vec(&config).map_err(|error| error.to_string())?,
        )
        .map_err(|error| error.to_string())?;
        let catalog = serde_json::to_vec(&serde_json::json!({"openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai",
            "models":{"test-model":{"id":"test-model","name":"Test Model","family":"test-model","tool_call":true,
                "modalities":{"input":["text"],"output":["text"]},"limit":{"output":32000}}}}}))
            .map_err(|error| error.to_string())?;
        let connected = serde_json::to_vec(&serde_json::json!({"models":[{"slug":"test-model","visibility":"list","context_window":128000,"supported_reasoning_levels":[]}]}))
            .map_err(|error| error.to_string())?;
        super::super::write_new(&source.join("account-models.json"), &connected)?;
        let target = scratch.0.join("new-profile");
        let missing = initialize(&target, &source)
            .err()
            .ok_or("missing public catalog accepted")?;
        assert!(missing.contains("model-catalog.json"));
        assert!(!target.exists());
        super::super::write_new(&source.join("model-catalog.json"), &catalog)?;
        initialize(&target, &source)?;
        assert_eq!(
            capture(&target.join("model-catalog.json"), INVENTORY_BYTES)?,
            catalog
        );
        assert_eq!(
            capture(&target.join("account-models.json"), INVENTORY_BYTES)?,
            connected
        );
        Ok(())
    }

    #[test]
    fn frc_setup_missing_and_invalid_credentials_do_not_substitute_a_script() -> Result<(), String>
    {
        let scratch = Scratch::new("credentials")?;
        let source = scratch.account()?;
        std::fs::remove_file(source.join("provider.credential"))
            .map_err(|error| error.to_string())?;
        let target = scratch.0.join("missing-profile");
        let missing = initialize(&target, &source)
            .err()
            .ok_or("missing credential was accepted")?;
        assert!(missing.contains("missing"));
        assert!(
            !target.exists(),
            "missing input must fail before profile creation"
        );
        super::super::write_new(&source.join("provider.credential"), &[0xff])?;
        let invalid = initialize(&target, &source)
            .err()
            .ok_or("invalid credential was accepted")?;
        assert!(invalid.contains("credential was rejected"));
        assert!(
            !target.exists(),
            "a refused staged profile must never be published"
        );
        assert!(!target.join("local-plan.json").exists());
        Ok(())
    }

    #[test]
    fn frc_setup_invalid_config_reports_no_secret_and_creates_no_profile() -> Result<(), String> {
        let scratch = Scratch::new("invalid-config")?;
        let source = scratch.account()?;
        std::fs::write(
            source.join("grapt.jsonc"),
            b"{\"secret\":\"FRC_SECRET_SENTINEL\",broken}",
        )
        .map_err(|error| error.to_string())?;
        let target = scratch.0.join("new-profile");
        let error = initialize(&target, &source)
            .err()
            .ok_or("invalid config was accepted")?;
        assert!(error.contains("invalid JSONC"));
        assert!(!error.contains("FRC_SECRET_SENTINEL"));
        assert!(!target.exists());
        Ok(())
    }
}
