use super::*;

fn authorize(command: &TerminalCommand, repository: &Path) -> Result<bool, String> {
    super::authorize_inner(command, repository, false)
        .map(|decision| matches!(decision, super::Authorization::Revoked))
}

struct Fixture(PathBuf);
impl Fixture {
    fn new(label: &str) -> Result<Self, String> {
        let root = std::env::temp_dir().join(format!(
            "grapt-profile-trust-{label}-{}",
            std::process::id()
        ));
        std::fs::create_dir(&root).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(root.join("repository/.grapt"))
            .map_err(|error| error.to_string())?;
        std::fs::write(root.join("repository/.grapt/grapt.jsonc"), b"{}")
            .map_err(|error| error.to_string())?;
        Ok(Self(
            std::fs::canonicalize(root).map_err(|error| error.to_string())?,
        ))
    }
    fn command(&self, args: &[&str]) -> Result<TerminalCommand, String> {
        TerminalCommand::parse(
            &args.iter().map(|arg| (*arg).to_owned()).collect::<Vec<_>>(),
            self.0.join("repository"),
            Some(self.0.join("user/default")),
        )
    }
    fn repository(&self) -> PathBuf {
        self.0.join("repository")
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn repository_profile_requires_a_persisted_path_and_content_decision() -> Result<(), String> {
    let fixture = Fixture::new("remember")?;
    let ordinary = fixture.command(&[])?;
    assert!(authorize(&ordinary, &fixture.repository()).is_err());
    let explicit = fixture.command(&["--profile", ".grapt"])?;
    assert!(!authorize(&explicit, &fixture.repository())?);
    assert!(
        !fixture.0.join("user/profile-trust").exists(),
        "selection is not persistent trust"
    );
    let remember = fixture.command(&["--profile", ".grapt", "--trust-profile"])?;
    assert!(!authorize(&remember, &fixture.repository())?);
    assert!(!authorize(&ordinary, &fixture.repository())?);
    for name in CONFIGURATION {
        let path = ordinary.profile.join(name);
        let previous = std::fs::read(&path).ok();
        std::fs::write(&path, b"changed configuration").map_err(|error| error.to_string())?;
        assert!(
            authorize(&ordinary, &fixture.repository()).is_err(),
            "{name} did not invalidate trust"
        );
        if let Some(bytes) = previous {
            std::fs::write(path, bytes).map_err(|error| error.to_string())?;
        } else {
            std::fs::remove_file(path).map_err(|error| error.to_string())?;
        }
    }
    let revoke = fixture.command(&["--profile", ".grapt", "--revoke-profile-trust"])?;
    assert!(authorize(&revoke, &fixture.repository())?);
    assert!(authorize(&ordinary, &fixture.repository()).is_err());
    Ok(())
}

#[test]
fn revocation_does_not_need_the_changed_configuration_to_be_readable() -> Result<(), String> {
    let fixture = Fixture::new("revoke-changed")?;
    let remember = fixture.command(&["--profile", ".grapt", "--trust-profile"])?;
    authorize(&remember, &fixture.repository())?;
    let original = ProfileSnapshot::capture(&remember.profile)?;
    let root = storage(&remember, &fixture.repository())?;
    let record = original.record(&root)?;
    assert!(record.is_file());

    // Public CLI input shape: a formerly trusted configuration has become an
    // invalid directory. Revoking must remove its persisted grant regardless.
    let config = remember.profile.join("grapt.jsonc");
    std::fs::remove_file(&config).map_err(|error| error.to_string())?;
    std::fs::create_dir(&config).map_err(|error| error.to_string())?;
    assert!(authorize(&fixture.command(&[])?, &fixture.repository()).is_err());
    let revoke = fixture.command(&["--profile", ".grapt", "--revoke-profile-trust"])?;
    assert!(authorize(&revoke, &fixture.repository())?);
    assert!(!record.exists());

    // Restoring old bytes cannot resurrect a decision that was withdrawn.
    std::fs::remove_dir(&config).map_err(|error| error.to_string())?;
    std::fs::write(&config, b"{}").map_err(|error| error.to_string())?;
    assert!(authorize(&fixture.command(&[])?, &fixture.repository()).is_err());
    Ok(())
}

#[test]
fn revocation_does_not_report_success_before_directory_publication() -> Result<(), String> {
    let fixture = Fixture::new("revoke-publication")?;
    let remember = fixture.command(&["--profile", ".grapt", "--trust-profile"])?;
    authorize(&remember, &fixture.repository())?;
    let root = storage(&remember, &fixture.repository())?;
    let record = ProfileSnapshot::capture(&remember.profile)?.record(&root)?;
    let result = revoke_record(&root, &record, |directory| {
        assert_eq!(directory, root);
        assert!(!record.exists(), "cut immediately after namespace removal");
        Err("injected namespace publication failure".into())
    });
    assert_eq!(result, Err("injected namespace publication failure".into()));
    assert!(authorize(&fixture.command(&[])?, &fixture.repository()).is_err());
    Ok(())
}

#[test]
fn a_private_directory_does_not_legitimize_a_foreign_writable_record() -> Result<(), String> {
    let fixture = Fixture::new("record-authority")?;
    let remember = fixture.command(&["--profile", ".grapt", "--trust-profile"])?;
    authorize(&remember, &fixture.repository())?;
    let root = storage(&remember, &fixture.repository())?;
    let record = ProfileSnapshot::capture(&remember.profile)?.record(&root)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&record, std::fs::Permissions::from_mode(0o666))
            .map_err(|error| error.to_string())?;
    }
    #[cfg(windows)]
    {
        let system = std::env::var_os("SystemRoot").ok_or("SystemRoot unavailable")?;
        let status = std::process::Command::new(PathBuf::from(system).join("System32/icacls.exe"))
            .arg(&record)
            .args(["/grant", "*S-1-1-0:(W)"])
            .stdout(std::process::Stdio::null())
            .status()
            .map_err(|error| error.to_string())?;
        assert!(status.success(), "fixture grant failed");
    }
    grapt_login::profile_security::verify_directory(&root).map_err(|error| error.to_string())?;
    let result = authorize(&fixture.command(&[])?, &fixture.repository());
    assert!(matches!(result, Err(message) if message.contains("record authority is invalid")));
    Ok(())
}

#[test]
fn equal_content_at_another_path_cannot_reuse_a_decision() -> Result<(), String> {
    let fixture = Fixture::new("path")?;
    let snapshot = ProfileSnapshot::capture(&fixture.repository().join(".grapt"))?;
    let store = fixture.0.join("user/profile-trust");
    snapshot.remember(&store)?;
    let second = fixture.0.join("second/.grapt");
    std::fs::create_dir_all(&second).map_err(|error| error.to_string())?;
    std::fs::write(second.join("grapt.jsonc"), b"{}").map_err(|error| error.to_string())?;
    assert!(!ProfileSnapshot::capture(&second)?.trusted(&store)?);
    let alias = fixture.repository().join("../repository/.grapt");
    assert!(ProfileSnapshot::capture(&alias)?.trusted(&store)?);
    Ok(())
}

#[test]
fn repository_controlled_or_malformed_trust_records_never_authorize() -> Result<(), String> {
    let fixture = Fixture::new("records")?;
    let mut command = fixture.command(&["--profile", ".grapt", "--trust-profile"])?;
    command.user_profile = Some(fixture.repository().join("user/default"));
    assert!(authorize(&command, &fixture.repository()).is_err());
    command.user_profile = Some(fixture.0.join("user/default"));
    authorize(&command, &fixture.repository())?;
    let snapshot = ProfileSnapshot::capture(&command.profile)?;
    let record = snapshot.record(&storage(&command, &fixture.repository())?)?;
    std::fs::write(record, b"not json").map_err(|error| error.to_string())?;
    assert!(authorize(&fixture.command(&[])?, &fixture.repository()).is_err());
    Ok(())
}

#[test]
fn trust_flags_need_an_explicit_profile_and_cannot_conflict() -> Result<(), String> {
    let fixture = Fixture::new("flags")?;
    for args in [
        vec!["--trust-profile"],
        vec!["--revoke-profile-trust"],
        vec![
            "--profile",
            ".grapt",
            "--trust-profile",
            "--revoke-profile-trust",
        ],
        vec!["--profile", ".grapt", "--trust-profile", "--trust-profile"],
    ] {
        assert!(fixture.command(&args).is_err());
    }
    Ok(())
}
