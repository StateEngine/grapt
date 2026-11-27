//! Native launch witnesses: build grapt-server from this candidate before running.
//! No account or model call is involved; malformed local configuration refuses.
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};
use std::time::Duration;

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

struct Profile(PathBuf);

impl Profile {
    async fn new(label: &str, product: &Path) -> TestResult<Self> {
        let root =
            std::env::temp_dir().join(format!("grapt-native-log-{label}-{}", std::process::id()));
        assert!(!root.exists(), "native witness profile already exists");
        let profile = Self(root);
        let mut command = tokio::process::Command::new(product);
        command.args(["init-profile", "--service"]).arg(&profile.0);
        let initialized = output(command).await?;
        assert!(initialized.status.success(), "native init-profile failed");
        std::fs::write(
            profile.0.join("service.conf"),
            b"slot=product\napi_key sk-test-123456\n",
        )?;
        Ok(profile)
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}

fn product() -> TestResult<PathBuf> {
    let directory = Path::new(env!("CARGO_BIN_EXE_grapt"))
        .parent()
        .ok_or("CLI binary has no directory")?;
    let binary = directory.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    assert!(binary.is_file(), "build grapt-product before this witness");
    Ok(binary)
}

async fn output(mut command: tokio::process::Command) -> TestResult<Output> {
    command.stdin(Stdio::null()).kill_on_drop(true);
    Ok(tokio::time::timeout(Duration::from_secs(35), command.output()).await??)
}

#[tokio::test]
#[ignore = "requires the same-candidate native grapt-server build beside grapt"]
async fn native_service_run_and_code_capture_server_stderr() -> TestResult {
    let product = product()?;
    for route in ["service", "run", "code"] {
        let profile = Profile::new(route, &product).await?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        match route {
            "code" => {
                command.arg("code").arg("--profile").arg(&profile.0);
            }
            "service" => {
                command.args(["service", "start"]);
                command.arg("--service").arg(&profile.0);
                command.arg("--product").arg(&product);
            }
            _ => {
                command
                    .args(["run", "--embedded"])
                    .arg("--service")
                    .arg(&profile.0);
                command.arg("--product").arg(&product);
                command.args(["--prompt", "must never reach a model"]);
            }
        }
        let refusal = output(command).await?;
        assert!(
            !refusal.status.success(),
            "{route} admitted malformed config"
        );
        let log = profile.0.join("logs/server.log");
        let stderr = String::from_utf8(std::fs::read(&log)?)?;
        let expected = if matches!(route, "code" | "run") {
            "product launch refused: service configuration was rejected"
        } else {
            "line 2"
        };
        assert!(stderr.contains(expected), "{route}: {stderr}");
        assert!(!stderr.contains("sk-test") && !stderr.contains("123456"));
        let message = String::from_utf8(refusal.stderr)?;
        assert!(message.contains(&format!("server log: {}", log.display())));
    }
    Ok(())
}

#[tokio::test]
#[ignore = "requires the same-candidate native grapt-server build beside grapt"]
async fn native_log_open_failure_refuses_without_citing_an_absent_log() -> TestResult {
    let product = product()?;
    let profile = Profile::new("unwritable", &product).await?;
    std::fs::write(profile.0.join("logs"), b"preserved")?;
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
    command.args(["service", "start"]);
    command.arg("--service").arg(&profile.0);
    command.arg("--product").arg(&product);
    let refusal = output(command).await?;
    assert!(!refusal.status.success());
    let message = String::from_utf8(refusal.stderr)?;
    assert!(!message.contains("; server log:"), "{message}");
    assert!(!profile.0.join("logs/server.log").exists());
    assert_eq!(std::fs::read(profile.0.join("logs"))?, b"preserved");
    assert!(!message.contains("line 2"), "product unexpectedly started");
    Ok(())
}
