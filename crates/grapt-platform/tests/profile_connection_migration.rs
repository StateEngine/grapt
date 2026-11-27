//! Sanctioned first-start migration retains exactly the approved profile.
use grapt_platform::{profile_configuration as trust, PublishedConfig};
use std::path::PathBuf;

struct Repository(PathBuf);
impl Drop for Repository {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn approved_first_start_retains_commands_and_refuses_external_changes(
) -> Result<(), Box<dyn std::error::Error>> {
    let temporary =
        std::env::temp_dir().join(format!("grapt-approved-migration-{}", std::process::id()));
    std::fs::create_dir(&temporary)?;
    let repository = Repository(std::fs::canonicalize(temporary)?);
    let profile = repository.0.join(".grapt");
    std::fs::create_dir(&profile)?;
    grapt_login::profile_security::protect_directory(&profile)?;
    let config = serde_json::json!({
        "permission":"ask", "store":{"authority":"open-ready"}, "product":{"build":"approval-migration"},
        "provider":{"id":"openai","model":"fixture","base_model":"fixture","family":"fixture",
            "route":"responses","account":"account","credential_version":"key-v1","policy_version":"policy","variants":[]}
    }).to_string();
    std::fs::write(profile.join("grapt.jsonc"), &config)?;
    let command = profile.join("hello.md");
    std::fs::write(&command, b"Say hello")?;
    let approval = trust::ProfileConfiguration::capture(&profile)?
        .approval()
        .clone();
    trust::install(&profile, &approval)?;
    assert!(trust::repository_is_approved(&repository.0, &command));
    let captured = PublishedConfig::load(&profile)?;
    assert!(captured.ensure_connection_store()?);
    assert!(
        trust::repository_is_approved(&repository.0, &command),
        "first-start migration lost trusted commands"
    );
    assert!(matches!(
        trust::frozen(&profile.join("connections.json"), 64 * 1024)?,
        trust::FrozenRead::Bytes(_)
    ));
    assert_eq!(
        PublishedConfig::load(&profile)?
            .target()?
            .provider()
            .as_str(),
        "openai"
    );
    let migrated = std::fs::read(profile.join("connections.json"))?;
    std::fs::write(profile.join("connections.json"), b"external edit")?;
    assert!(!trust::repository_is_approved(&repository.0, &command));
    std::fs::write(profile.join("connections.json"), migrated)?;
    assert!(trust::repository_is_approved(&repository.0, &command));
    std::fs::write(profile.join("grapt.jsonc"), config.replace("ask", "allow"))?;
    assert!(!trust::repository_is_approved(&repository.0, &command));
    assert!(captured.ensure_connection_store().is_err());
    Ok(())
}
