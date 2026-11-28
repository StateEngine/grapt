//! IR: crashes leave the profile absent; only the claimed stage is redone.
use super::*;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

pub(in super::super) fn cut(target: &Path, phase: &str) -> Result<(), String> {
    let Some(selected) = std::env::var_os("GRAPT_IR_PROFILE") else {
        return Ok(());
    };
    if std::env::var("GRAPT_IR_CUT").ok().as_deref() != Some(phase) {
        return Ok(());
    }
    // Windows canonicalization adds a verbatim prefix. Compare canonical
    // parents because the requested profile deliberately does not exist yet.
    let resolve = |path: &Path| -> Result<std::path::PathBuf, String> {
        let parent = path.parent().ok_or("IR target parent missing")?;
        let name = path.file_name().ok_or("IR target name missing")?;
        Ok(std::fs::canonicalize(parent)
            .map_err(|error| error.to_string())?
            .join(name))
    };
    if resolve(Path::new(&selected))? != resolve(target)? {
        return Ok(());
    }
    let marker = std::env::var_os("GRAPT_IR_MARKER").ok_or("IR marker missing")?;
    // Publish the complete marker before the parent can observe its name.
    let pending = Path::new(&marker).with_extension("pending");
    std::fs::write(&pending, phase.as_bytes()).map_err(|error| error.to_string())?;
    std::fs::rename(pending, marker).map_err(|error| error.to_string())?;
    for _ in 0..1000 {
        std::thread::sleep(Duration::from_millis(25));
    }
    Err("IR parent did not terminate the interrupted initializer".into())
}

struct ChildGuard(Child);
impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _kill = self.0.kill();
        let _reap = self.0.wait();
    }
}

#[test]
fn ir_crash_during_sign_in_import_keeps_one_complete_generation_on_retry() -> Result<(), String> {
    let scratch = Scratch::new("sign-in-crash")?;
    let source = super::sign_in::signed_source(&scratch)?;
    let original = capture(&source.join(grapt_login::chatgpt::STATE_FILE), 16 * 1024)?;
    for phase in ["inputs", "final-config", "publish"] {
        let target = scratch.0.join(phase);
        let mut child = paused_child(
            &target,
            &source,
            &scratch.0.join(format!("{phase}-cut")),
            phase,
        )?;
        assert!(
            !target.exists(),
            "a partial sign-in profile was published at {phase}"
        );
        child.0.kill().map_err(|error| error.to_string())?;
        assert!(!child.0.wait().map_err(|error| error.to_string())?.success());
        initialize(&target, &source)?;
        assert!(grapt_login::chatgpt::Session::open(&target, "test-account")
            .map_err(|error| error.to_string())?
            .is_some());
        let stored = capture(&target.join(grapt_login::chatgpt::STATE_FILE), 16 * 1024)?;
        #[cfg(windows)]
        assert!(
            grapt_login::profile_security::is_protected(&stored),
            "import published an unprotected generation"
        );
        // DPAPI uses fresh randomness on each save; compare the complete
        // generation while retaining the raw source-preservation check below.
        let plaintext = |bytes: &[u8]| {
            grapt_login::profile_security::open(bytes, 16 * 1024).map_err(|error| error.to_string())
        };
        assert_eq!(plaintext(&stored)?, plaintext(&original)?);
        assert_eq!(
            capture(&source.join(grapt_login::chatgpt::STATE_FILE), 16 * 1024)?,
            original
        );
        assert!(
            initialize(&target, &source).is_err(),
            "retry overwrote published sign-in"
        );
    }
    Ok(())
}

#[test]
#[ignore = "IR subprocess fixture; parent supplies exact paths and cut"]
fn ir_profile_crash_child() -> Result<(), String> {
    let target = std::env::var_os("GRAPT_IR_PROFILE").ok_or("IR profile path missing")?;
    let source = std::env::var_os("GRAPT_IR_SOURCE").ok_or("IR source path missing")?;
    let result = initialize(Path::new(&target), Path::new(&source));
    panic!("IR parent did not terminate child at publication cut: {result:?}");
}

fn paused_child(
    target: &Path,
    source: &Path,
    marker: &Path,
    phase: &str,
) -> Result<ChildGuard, String> {
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().map_err(|error| error.to_string())?)
            .args([
                "--exact",
                "dogfood::first_use::import::tests::fresh_crash::ir_profile_crash_child",
                "--ignored",
                "--nocapture",
            ])
            .env("GRAPT_IR_PROFILE", target)
            .env("GRAPT_IR_SOURCE", source)
            .env("GRAPT_IR_MARKER", marker)
            .env("GRAPT_IR_CUT", phase)
            .env(
                grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
                grapt_login::profile_security::isolate_test_keystore()
                    .map_err(|error| error.to_string())?,
            )
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|error| error.to_string())?,
    );
    for _ in 0..2000 {
        if marker.exists() {
            assert_eq!(
                std::fs::read(marker).map_err(|error| error.to_string())?,
                phase.as_bytes()
            );
            return Ok(child);
        }
        if let Some(status) = child.0.try_wait().map_err(|error| error.to_string())? {
            return Err(format!("IR child exited before {phase} boundary: {status}"));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    Err(format!("IR child never reached required {phase} boundary"))
}

#[test]
fn ir_interrupted_large_managed_oauth_import_retries_without_losing_accounts() -> Result<(), String>
{
    use grapt_connections::store::{next_key_files, CredentialFormat, Store, CONNECTION_CAP};
    use grapt_login::vault::{
        ApiDestination, DestinationBinding, OAuthRefreshDestination, VaultSecret,
    };
    use grapt_types::{ProviderId, Timestamp};

    let scratch = Scratch::new("large-managed-crash")?;
    let source = super::sign_in::signed_source(&scratch)?;
    grapt_login::profile_security::protect_directory(&source).map_err(|error| error.to_string())?;
    let tokens = grapt_login::chatgpt::load(&source)
        .map_err(|error| error.to_string())?
        .ok_or("source sign-in absent")?;
    let mut store = Store::open(&source).map_err(|error| error.to_string())?;
    let mut records = store
        .migrate(&capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?)
        .map_err(|error| error.to_string())?;
    let template = records.connections[0].clone();
    let binding = DestinationBinding::oauth(
        ApiDestination::parse(grapt_login::chatgpt::CODEX_BACKEND)
            .map_err(|error| error.to_string())?,
        OAuthRefreshDestination::parse(grapt_login::chatgpt::ISSUER)
            .map_err(|error| error.to_string())?,
    );
    for index in 0..CONNECTION_CAP {
        let mut record = template.clone();
        if index > 0 {
            record.id = ProviderId::new(format!("account-{index}")).ok_or("account identity")?;
        }
        let (credential, listing) = next_key_files(&record.id, None, &format!("source-{index}"))
            .map_err(|error| error.to_string())?;
        record.oauth_file = Some(format!(
            "{}.oauth.json",
            credential.trim_end_matches(".credential")
        ));
        record.credential_file = credential;
        record.discovery_file = listing;
        record.credential_format = CredentialFormat::VaultedOAuth {
            binding: binding.clone(),
        };
        if index == 0 {
            records.connections[0] = record.clone();
        } else {
            records.connections.push(record.clone());
        }
        let commit = store.install_oauth_with(
            &records, &record.id, VaultSecret::oauth(tokens.clone()).map_err(|error| error.to_string())?,
            &binding, grapt_login::chatgpt::ISSUER,
            br#"{"models":[{"slug":"test-model","context_window":128000,"supported_reasoning_levels":[]}]}"#,
            Timestamp::from_millis(1), || Ok(()),
        ).map_err(|error| error.to_string())?.1;
        assert!(commit.cleanup_error.is_none() && commit.durability_error.is_none());
    }
    drop(store);
    let original = capture(&source.join("connections.json"), CONFIG_BYTES)?;
    let target = scratch.0.join("profile");
    let mut child = paused_child(&target, &source, &scratch.0.join("cut"), "final-config")?;
    assert!(!target.exists());
    child.0.kill().map_err(|error| error.to_string())?;
    assert!(!child.0.wait().map_err(|error| error.to_string())?.success());
    initialize(&target, &source)?;
    let imported = Store::read_snapshot(&target)
        .map_err(|error| error.to_string())?
        .ok_or("imported accounts absent")?;
    assert_eq!(imported.connections.len(), CONNECTION_CAP);
    assert_eq!(imported.default_model, records.default_model);
    for record in &imported.connections {
        let files = grapt_login::chatgpt::SignInFiles::new(
            record.oauth_file.as_deref().ok_or("OAuth state absent")?,
            &record.credential_file,
        )
        .and_then(|files| files.with_vault(record.id.clone(), binding.clone()))
        .and_then(|files| files.with_generation(record.credential_version.clone()))
        .map_err(|error| error.to_string())?;
        assert_eq!(
            files.load(&target).map_err(|error| error.to_string())?,
            Some(tokens.clone())
        );
        assert!(!target.join(&record.credential_file).exists());
    }
    assert_eq!(
        capture(&source.join("connections.json"), CONFIG_BYTES)?,
        original
    );
    drop(Store::open(&source).map_err(|error| error.to_string())?);
    assert!(matches!(
        grapt_store::preflight_read_only(&target.join("database.sqlite3")),
        grapt_store::StoreReadiness::Ready
    ));
    Ok(())
}

#[test]
fn ir_crash_at_each_staging_boundary_redoes_without_publishing_half_a_profile() -> Result<(), String>
{
    for phase in [
        "directory",
        "config",
        "inputs",
        "store",
        "final-config",
        "publish",
    ] {
        let scratch = Scratch::new(phase)?;
        let source = scratch.account()?;
        let config = capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?;
        let credential = capture(&source.join("provider.credential"), CONFIG_BYTES)?;
        let target = scratch.0.join("new-profile");
        let unrelated = scratch.0.join("another-init");
        std::fs::create_dir(&unrelated).map_err(|error| error.to_string())?;
        std::fs::write(unrelated.join("keep"), b"IR_FOREIGN_DATA")
            .map_err(|error| error.to_string())?;
        let mut child = paused_child(&target, &source, &scratch.0.join("cut"), phase)?;
        assert!(
            !target.exists(),
            "IR half profile became visible at {phase}"
        );
        child.0.kill().map_err(|error| error.to_string())?;
        assert!(!child.0.wait().map_err(|error| error.to_string())?.success());
        initialize(&target, &source)?;
        assert!(matches!(
            grapt_store::preflight_read_only(&target.join("database.sqlite3")),
            grapt_store::StoreReadiness::Ready
        ));
        let platform =
            grapt_platform::PlatformOwner::prepare(&target).map_err(|error| error.to_string())?;
        drop(platform.settle());
        assert_eq!(capture(&source.join("grapt.jsonc"), CONFIG_BYTES)?, config);
        assert_eq!(
            capture(&source.join("provider.credential"), CONFIG_BYTES)?,
            credential
        );
        assert!(!source.join("database.sqlite3").exists());
        assert_eq!(
            std::fs::read(unrelated.join("keep")).map_err(|error| error.to_string())?,
            b"IR_FOREIGN_DATA"
        );
        let published = capture(&target.join("grapt.jsonc"), CONFIG_BYTES)?;
        assert!(
            initialize(&target, &source).is_err(),
            "IR repeated setup overwrote a published profile"
        );
        assert_eq!(
            capture(&target.join("grapt.jsonc"), CONFIG_BYTES)?,
            published
        );
    }
    Ok(())
}

#[test]
fn ir_exclusion_is_held_through_directory_publication() -> Result<(), String> {
    let scratch = Scratch::new("publish-race")?;
    let source = scratch.account()?;
    let target = scratch.0.join("new-profile");
    let mut child = paused_child(&target, &source, &scratch.0.join("cut"), "publish")?;
    let refusal = initialize(&target, &source)
        .err()
        .ok_or("IR second initializer escaped publish exclusion")?;
    assert!(
        refusal.contains("being initialized"),
        "IR contender must refuse on exclusion: {refusal}"
    );
    assert!(
        !target.exists(),
        "IR contender published during another initializer's held exclusion"
    );
    child.0.kill().map_err(|error| error.to_string())?;
    assert!(!child.0.wait().map_err(|error| error.to_string())?.success());
    initialize(&target, &source)?;
    assert!(matches!(
        grapt_store::preflight_read_only(&target.join("database.sqlite3")),
        grapt_store::StoreReadiness::Ready
    ));
    Ok(())
}

/// The rename is the last publication step: a crash after it, before the
/// receipt, leaves a COMPLETE profile. The retry must say "already published;
/// use it", never "move it aside".
#[test]
fn ir_rename_complete_crash_before_receipt_says_already_published() -> Result<(), String> {
    let scratch = Scratch::new("receipt")?;
    let source = scratch.account()?;
    let target = scratch.0.join("new-profile");
    let mut child = paused_child(&target, &source, &scratch.0.join("cut"), "receipt")?;
    child.0.kill().map_err(|error| error.to_string())?;
    assert!(!child.0.wait().map_err(|error| error.to_string())?.success());
    assert!(
        target.exists(),
        "IR the renamed profile must be visible after the kill"
    );
    assert!(matches!(
        grapt_store::preflight_read_only(&target.join("database.sqlite3")),
        grapt_store::StoreReadiness::Ready
    ));
    let refusal = initialize(&target, &source)
        .err()
        .ok_or("IR a published target must refuse re-setup")?;
    assert!(
        refusal.contains("already published") && refusal.contains("use it"),
        "IR published retry must name the published profile: {refusal}"
    );
    assert!(
        !refusal.contains("move it aside"),
        "IR a complete profile must not be advised away: {refusal}"
    );
    Ok(())
}

#[test]
fn ir_existing_target_non_file_config_refuses_without_publishing() -> Result<(), String> {
    let scratch = Scratch::new("nonfile-config")?;
    let source = scratch.account()?;
    let target = scratch.0.join("new-profile");
    std::fs::create_dir(&target).map_err(|error| error.to_string())?;
    std::fs::create_dir(target.join("grapt.jsonc")).map_err(|error| error.to_string())?;
    let refusal = initialize(&target, &source)
        .err()
        .ok_or("IR a non-file existing configuration was admitted")?;
    assert!(
        refusal.contains("already exists"),
        "IR a non-file configuration must keep the generic existing refusal: {refusal}"
    );
    assert!(
        !refusal.contains("already published"),
        "IR a non-file configuration must not be claimed as published: {refusal}"
    );
    assert!(
        target.join("grapt.jsonc").is_dir(),
        "IR the non-file entry must be preserved"
    );
    assert!(
        !target.join("database.sqlite3").exists(),
        "IR a refused target must not gain a store"
    );
    Ok(())
}
