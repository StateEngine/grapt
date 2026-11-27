//! A real serve-terminal launch with the CLI's approval argument must refuse
//! replaced configuration before touching the store or starting any service.
use grapt_platform::profile_configuration::ProfileConfiguration;

struct Fixture(std::path::PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn replacement_between_authorization_and_terminal_consumption_refuses(
) -> Result<(), Box<dyn std::error::Error>> {
    let path = std::env::temp_dir().join(format!("grapt-terminal-approved-{}", std::process::id()));
    std::fs::create_dir(&path)?;
    let fixture = Fixture(std::fs::canonicalize(path)?);
    std::fs::write(fixture.0.join("grapt.jsonc"), b"{}")?;
    let approval = ProfileConfiguration::capture(&fixture.0)?
        .approval()
        .encoded()?;
    // Deterministic replacement at the process boundary, not a timing race.
    std::fs::write(fixture.0.join("grapt.jsonc"), b"{\"plugin\":[\"foreign\"]}")?;
    for notify_ready in [false, true] {
        let mut command =
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
        command
            .arg("serve-terminal")
            .arg("--profile-approval")
            .arg(&approval)
            .arg("--service")
            .arg(&fixture.0)
            .arg("--port")
            .arg("0");
        if notify_ready {
            command.arg("--notify-ready");
        }
        let output = command.stdin(std::process::Stdio::null()).output()?;
        assert!(!output.status.success());
        let text = format!(
            "{}{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            text.contains("profile changed between authorization and consumption"),
            "{text}"
        );
    }
    let mut entries = std::fs::read_dir(&fixture.0)?.collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        entries.len(),
        1,
        "refused launch must not initialize store, credentials or election files"
    );
    assert_eq!(
        entries.pop().ok_or("missing input")?.file_name(),
        "grapt.jsonc"
    );
    Ok(())
}
