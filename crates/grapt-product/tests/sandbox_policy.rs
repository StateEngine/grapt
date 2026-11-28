use std::{fs, path::PathBuf, process::Command};

struct Fixture(PathBuf);

impl Fixture {
    fn new(label: &str) -> Result<Self, Box<dyn std::error::Error>> {
        let root = std::env::temp_dir().join(format!(
            "grapt-sandbox-policy-{}-{label}",
            std::process::id()
        ));
        fs::create_dir(&root)?;
        Ok(Self(fs::canonicalize(root)?))
    }

    fn command(&self) -> Command {
        let mut command =
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"));
        command
            .arg("sandbox-policy")
            .current_dir(&self.0)
            .env_remove("GRAPT_CONFIG")
            .env_remove("SECODE_CONFIG") // grapt-rename: keep
            .env_remove("GRAPT_CONFIG_CONTENT")
            .env_remove("SECODE_CONFIG_CONTENT") // grapt-rename: keep
            .env_remove("GRAPT_TEST_CATALOG_ROOT")
            .env("GRAPT_TEST_HOME", self.0.join("home"))
            .env("GRAPT_CONFIG_DIR", self.0.join("config"))
            .env("XDG_DATA_HOME", self.0.join("data"));
        command
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn capability_probe_needs_no_profile_and_creates_no_lifecycle_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("capability")?;
    let output = fixture.command().arg("--capabilities").output()?;
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["schema"], "grapt.sandbox-policy.v1");
    assert!(report["capabilities"]
        .as_array()
        .ok_or("missing capabilities")?
        .iter()
        .any(|item| item == "git_metadata_read_only/v1"));
    assert_eq!(fs::read_dir(&fixture.0)?.count(), 0);
    Ok(())
}

#[test]
fn policy_preflight_reads_effective_config_without_opening_service_state(
) -> Result<(), Box<dyn std::error::Error>> {
    let fixture = Fixture::new("inspection")?;
    let service = fixture.0.join("profile");
    let workspace = fixture.0.join("workspace");
    for directory in [&service, &workspace, &workspace.join(".git")] {
        fs::create_dir(directory)?;
    }
    let states = [
        "database.sqlite3",
        "registration",
        "serve-ready.json",
        "service.conf",
    ];
    for name in states {
        fs::write(service.join(name), b"unopened lifecycle sentinel")?;
    }
    fs::write(
        workspace.join("grapt.jsonc"),
        r#"{"sandbox":{"enabled":false,"git_metadata_read_only":true}}"#,
    )?;
    let inspect = || {
        fixture
            .command()
            .arg("--service")
            .arg(&service)
            .arg("--directory")
            .arg(&workspace)
            .output()
    };
    let output = inspect()?;
    assert!(output.status.success(), "{output:?}");
    let report: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["realization"], "not_started");
    assert_eq!(report["policy"]["git_metadata_read_only"], true);
    let protected: Vec<PathBuf> =
        serde_json::from_value(report["policy"]["protected_paths"].clone())?;
    assert!(protected.contains(&fs::canonicalize(workspace.join(".git"))?));
    let denied: Vec<PathBuf> = serde_json::from_value(report["policy"]["denied_paths"].clone())?;
    assert!(
        denied.iter().any(|path| path.ends_with(".ssh")),
        "sandbox-policy omitted denied_paths .ssh: {denied:?}"
    );
    for name in states {
        assert_eq!(
            fs::read(service.join(name))?,
            b"unopened lifecycle sentinel"
        );
    }
    assert_eq!(fs::read_dir(&service)?.count(), states.len());
    fs::write(workspace.join(".git/commondir"), "missing-common-directory")?;
    let refused = inspect()?;
    assert!(!refused.status.success(), "{refused:?}");
    let report: serde_json::Value = serde_json::from_slice(&refused.stdout)?;
    assert_eq!(report["schema"], "grapt.sandbox-policy.v1");
    assert_eq!(report["outcome"], "refused");
    assert!(report["reason"]
        .as_str()
        .ok_or("missing reason")?
        .contains("sandbox"));
    for name in states {
        assert_eq!(
            fs::read(service.join(name))?,
            b"unopened lifecycle sentinel"
        );
    }
    assert_eq!(fs::read_dir(&service)?.count(), states.len());
    for name in ["home", "config", "data"] {
        assert!(!fixture.0.join(name).exists());
    }
    Ok(())
}
