//! Public ADC admission uses isolated synthetic sources, never the host account.
use serde_json::json;
use std::{path::PathBuf, process::Command};
type TestResult = Result<(), Box<dyn std::error::Error>>;
struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _cleanup = std::fs::remove_dir_all(&self.0);
    }
}
fn run(name: &str) -> TestResult {
    const CHILD: &str = "GRAPT_ADC_SOURCE_CHILD";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        assert!(
            grapt_login::vertex::adc::Session::open().is_err(),
            "invalid ADC source was admitted"
        );
        return Ok(());
    }
    let path = std::env::temp_dir().join(format!("grapt-adc-source-{}-{name}", std::process::id()));
    std::fs::create_dir(&path)?;
    let root = Fixture(std::fs::canonicalize(path)?);
    let helper = json!({"type":"external_account", "audience":"fixture", "subject_token_type":"urn:ietf:params:oauth:token-type:jwt",
        "token_url":"https://sts.googleapis.com/v1/token", "credential_source":{"executable":{"command":"echo launched > helper-launched", "timeout_millis":5000}}});
    let value = if name.contains("impersonated") {
        json!({"type":"impersonated_service_account", "service_account_impersonation_url":"https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/fixture:generateAccessToken", "source_credentials":helper})
    } else if name.contains("oversized") {
        json!({"type":"authorized_user", "client_id":"fixture", "client_secret":"synthetic-secret", "refresh_token":"synthetic-refresh", "padding":"x".repeat(65536)})
    } else {
        helper
    };
    std::fs::write(root.0.join("explicit.json"), serde_json::to_vec(&value)?)?;
    // A missing explicit source must not fall back to this otherwise valid user file.
    std::fs::write(
        root.0.join("application_default_credentials.json"),
        serde_json::to_vec(&json!({
            "type":"authorized_user", "client_id":"fixture", "client_secret":"synthetic-secret", "refresh_token":"synthetic-refresh"
        }))?,
    )?;
    let mut child = Command::new(std::env::current_exe()?);
    child.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            child.env(key, value);
        }
    }
    let status = child
        .args([
            "--exact",
            &crate::exact(module_path!(), name),
            "--nocapture",
        ])
        .current_dir(&root.0)
        .env(CHILD, name)
        .env("HOME", &root.0)
        .env("USERPROFILE", &root.0)
        .env("CLOUDSDK_CONFIG", &root.0)
        .env(
            "GOOGLE_APPLICATION_CREDENTIALS",
            root.0.join(if name.contains("missing") {
                "missing.json"
            } else {
                "explicit.json"
            }),
        )
        .env("GOOGLE_EXTERNAL_ACCOUNT_ALLOW_EXECUTABLES", "1")
        .status()?;
    assert!(status.success(), "isolated ADC admission control failed");
    assert!(
        !root.0.join("helper-launched").try_exists()?,
        "credential helper ran"
    );
    Ok(())
}
#[test]
fn explicit_missing_adc_does_not_fall_back_to_gcloud() -> TestResult {
    run("explicit_missing_adc_does_not_fall_back_to_gcloud")
}
#[test]
fn oversized_adc_file_is_refused() -> TestResult {
    run("oversized_adc_file_is_refused")
}
#[test]
fn executable_adc_is_refused_without_execution() -> TestResult {
    run("executable_adc_is_refused_without_execution")
}
#[test]
fn impersonated_executable_adc_is_refused_without_execution() -> TestResult {
    run("impersonated_executable_adc_is_refused_without_execution")
}
