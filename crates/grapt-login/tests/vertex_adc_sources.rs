//! Indirect ADC sources are refused before an SDK can read unbounded inputs.
use grapt_login::vertex::adc::Session;
use serde_json::{json, Value};
use std::{path::PathBuf, process::Command};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD: &str = "GRAPT_TEST_ADC_SOURCE_CHILD";

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn refusal(source: &str, impersonated: bool) -> TestResult {
    let root = std::env::temp_dir().join(format!(
        "grapt-adc-source-{source}-{impersonated}-{}",
        std::process::id()
    ));
    std::fs::create_dir(&root)?;
    let fixture = Fixture(root);
    let token = fixture.0.join("subject-token");
    std::fs::write(&token, b"synthetic-subject-token")?;
    let mut document = json!({
        "type":"external_account",
        "audience":"//iam.googleapis.com/projects/123/locations/global/workloadIdentityPools/fixture/providers/fixture",
        "subject_token_type":"urn:ietf:params:oauth:token-type:jwt",
        "token_url":"http://127.0.0.1:9/token",
        "credential_source": if source == "file" {
            json!({"file":token})
        } else {
            json!({"url":"http://127.0.0.1:9/subject-token"})
        }
    });
    if impersonated {
        document = json!({
            "type":"impersonated_service_account",
            "service_account_impersonation_url":"https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/fixture:generateAccessToken",
            "source_credentials":document
        });
    }
    run_child(&fixture, &document, "refuse")
}

fn run_child(fixture: &Fixture, document: &Value, mode: &str) -> TestResult {
    let adc = fixture.0.join("adc.json");
    std::fs::write(&adc, serde_json::to_vec(document)?)?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args([
            "--exact",
            &crate::exact(module_path!(), "indirect_source_child"),
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD, mode)
        .env("GOOGLE_CLOUD_QUOTA_PROJECT", "fixture-override-quota")
        .env("GOOGLE_APPLICATION_CREDENTIALS", &adc)
        .env("HOME", &fixture.0)
        .env("USERPROFILE", &fixture.0)
        .env("APPDATA", &fixture.0)
        .env("PATH", fixture.0.join("no-programs"))
        .current_dir(&fixture.0);
    #[cfg(windows)]
    command.env(
        "SystemRoot",
        std::env::var_os("SystemRoot").ok_or("SystemRoot absent")?,
    );
    let output = command.output()?;
    assert!(
        output.status.success(),
        "ADC source contract refused: {output:?}"
    );
    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn indirect_source_child() -> TestResult {
    match std::env::var(CHILD).ok().as_deref() {
        Some("refuse") => assert!(
            Session::open().is_err(),
            "unbounded indirect ADC source accepted"
        ),
        Some("quota") => {
            let request = reqwest::Request::new(
                reqwest::Method::POST,
                "https://vertex.example/v1/generate".parse()?,
            );
            let request = Session::open()?.authorize(request).await?;
            assert_eq!(
                request.headers().get("x-goog-user-project"),
                Some(&reqwest::header::HeaderValue::from_static(
                    "fixture-override-quota"
                ))
            );
            assert!(request.headers().get("authorization").is_some());
        }
        _ => {}
    }
    Ok(())
}

#[test]
fn direct_file_source_is_refused() -> TestResult {
    refusal("file", false)
}
#[test]
fn direct_url_source_is_refused() -> TestResult {
    refusal("url", false)
}
#[test]
fn impersonated_file_source_is_refused() -> TestResult {
    refusal("file", true)
}
#[test]
fn impersonated_url_source_is_refused() -> TestResult {
    refusal("url", true)
}

#[test]
fn service_account_quota_override_reaches_authorized_request() -> TestResult {
    let root = std::env::temp_dir().join(format!("grapt-adc-quota-{}", std::process::id()));
    std::fs::create_dir(&root)?;
    let fixture = Fixture(root);
    // Reuse the repository's generated test-only key; no cloud account is involved.
    let document = json!({
        "type":"service_account",
        "private_key":include_str!("../../grapt-product/tests/product_root/connections/vertex-test-only.pem"),
        "private_key_id":"fixture-test-key",
        "client_email":"fixture@fixture-project.iam.gserviceaccount.com",
        "project_id":"fixture-project",
        "quota_project_id":"fixture-file-quota"
    });
    run_child(&fixture, &document, "quota")
}
