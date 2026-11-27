use grapt_login::{profile_security, vault::*};
use grapt_types::{ProviderId, Timestamp};
use std::path::PathBuf;
use zeroize::Zeroizing;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-vault-diagnostic-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&root)?;
        let fixture = Self(root);
        profile_security::protect_directory(&fixture.0)?;
        Ok(fixture)
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn secret(value: &str) -> Result<VaultSecret, VaultError> {
    VaultSecret::api_key(Zeroizing::new(value.into()))
}

fn safe_refusal(error: VaultError, operation: &str, root: &std::path::Path) {
    assert!(matches!(error, VaultError::Unavailable(_)));
    let path = root.to_string_lossy();
    for rendered in [error.to_string(), format!("{error:?}")] {
        assert!(rendered.contains(operation));
        assert!(!rendered.contains(path.as_ref()));
        assert!(!rendered.contains("synthetic-replacement-key"));
        assert!(!rendered.chars().any(char::is_control));
        assert!(rendered.len() < 256);
    }
}

#[test]
fn opening_a_missing_vault_names_the_boundary_without_disclosing_its_path() -> TestResult {
    let root = Fixture::new("missing")?;
    let absent = root.0.join("synthetic-private-path");
    let error = SealedFileVault::open(&absent)
        .err()
        .ok_or("missing vault accepted")?;
    safe_refusal(error, "VerifyDirectory", &root.0);
    assert!(!absent.exists());
    Ok(())
}

#[test]
fn staging_refusal_preserves_the_stored_entry_and_the_foreign_object() -> TestResult {
    let root = Fixture::new("staging")?;
    let mut vault = SealedFileVault::open(&root.0)?;
    let key = VaultKey::new(
        ProviderId::new("fixture").ok_or("fixture id")?,
        "fixture.credential",
    )?;
    let binding = DestinationBinding::key(ApiDestination::parse("https://api.example/v1")?);
    vault.put(
        &key,
        &binding,
        secret("synthetic-stored-key")?,
        Timestamp::from_millis(1),
    )?;
    let entry = root.0.join("fixture.credential");
    let before = std::fs::read(&entry)?;
    let staged = root.0.join(".fixture.credential.new");
    std::fs::create_dir(&staged)?;
    std::fs::write(staged.join("sentinel"), b"foreign staging object")?;
    let error = vault
        .put(
            &key,
            &binding,
            secret("synthetic-replacement-key")?,
            Timestamp::from_millis(2),
        )
        .err()
        .ok_or("foreign staging accepted")?;
    safe_refusal(error, "ReadStaging", &root.0);
    let error = vault.remove(&key).err().ok_or("foreign staging removed")?;
    safe_refusal(error, "ReadStaging", &root.0);
    assert_eq!(std::fs::read(&entry)?, before);
    assert_eq!(
        std::fs::read(staged.join("sentinel"))?,
        b"foreign staging object"
    );
    assert!(matches!(
        vault
            .get(&key, &binding)?
            .ok_or("stored key lost")?
            .expose(),
        CredentialView::ApiKey("synthetic-stored-key")
    ));
    assert_eq!(
        vault
            .describe(&key, &binding)?
            .ok_or("description lost")?
            .updated,
        Timestamp::from_millis(1)
    );
    Ok(())
}
