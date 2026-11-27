//! Public Azure authorization boundaries with an isolated synthetic CLI.
#![cfg(unix)]
use grapt_login::azure::{authorize, validate_endpoint, AzureError};
use std::{os::unix::fs::PermissionsExt, path::PathBuf, process::Command};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD: &str = "GRAPT_TEST_AZURE_CLI_BOUNDARY";
const TOKEN: &str = "synthetic-azure-review-token";

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn azure_cli_rejects_foreign_or_credential_bearing_destinations() {
    for url in [
        "http://resource.openai.azure.com/openai/v1",
        "https://resource.openai.azure.com.evil.example/openai/v1",
        "https://user:synthetic-password@resource.openai.azure.com/openai/v1",
        "https://resource.openai.azure.com:444/openai/v1",
        "https://127.0.0.1/openai/v1",
    ] {
        assert!(matches!(
            validate_endpoint(url),
            Err(AzureError::Destination)
        ));
    }
    for url in [
        "https://resource.openai.azure.com/openai/v1",
        "https://resource.services.ai.azure.com/openai/v1",
        "https://resource.cognitiveservices.azure.com/openai/v1",
    ] {
        assert!(validate_endpoint(url).is_ok());
    }
}

fn isolated(name: &str) -> TestResult {
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?
            .block_on(check(name));
    }
    let path = std::env::temp_dir().join(format!(
        "grapt-azure-boundary-{}-{name}",
        std::process::id()
    ));
    std::fs::create_dir(&path)?;
    let fixture = Fixture(std::fs::canonicalize(path)?);
    let cli = fixture.0.join("az");
    std::fs::write(&cli, format!(
        "#!/bin/sh\nprintf 'used' > \"$GRAPT_TEST_AZURE_RECEIPT\"\n\
         case \"$*\" in\n\
         *https://cognitiveservices.azure.com/.default*) ;;\n\
         *) exit 8 ;;\n\
         esac\n\
         if [ \"${CHILD}\" = 'azure_cli_failure_does_not_expose_helper_output' ]; then\n\
         printf '{TOKEN}' >&2\nexit 7\nfi\n\
         printf '%s' '{{\"accessToken\":\"{TOKEN}\",\"expires_on\":2145916800,\"tokenType\":\"Bearer\"}}'\n"
    ))?;
    std::fs::set_permissions(&cli, std::fs::Permissions::from_mode(0o700))?;
    let status = Command::new(std::env::current_exe()?)
        .args(["--exact", name, "--nocapture"])
        .env_clear()
        .env(CHILD, name)
        .env("PATH", &fixture.0)
        .env("HOME", &fixture.0)
        .env("GRAPT_TEST_AZURE_RECEIPT", fixture.0.join("used"))
        .status()?;
    assert!(status.success(), "isolated Azure credential control failed");
    assert_eq!(std::fs::read(fixture.0.join("used"))?, b"used");
    Ok(())
}

async fn check(name: &str) -> TestResult {
    let mut request = reqwest::Request::new(
        reqwest::Method::POST,
        "https://resource.openai.azure.com/openai/v1/responses".parse()?,
    );
    request
        .headers_mut()
        .insert("api-key", "synthetic-old-key".parse()?);
    request
        .headers_mut()
        .insert("x-api-key", "synthetic-old-key".parse()?);
    if name == "azure_cli_failure_does_not_expose_helper_output" {
        let error = authorize(request)
            .await
            .err()
            .ok_or("helper failure was accepted")?;
        assert!(matches!(error, AzureError::Unavailable));
        assert!(!format!("{error:?}: {error}").contains(TOKEN));
    } else {
        let request = authorize(request).await?;
        let value = request
            .headers()
            .get("authorization")
            .ok_or("bearer header absent")?;
        assert_eq!(value.to_str()?, format!("Bearer {TOKEN}"));
        assert!(value.is_sensitive());
        assert!(!format!("{:?}", request.headers()).contains(TOKEN));
        assert!(!request.headers().contains_key("api-key"));
        assert!(!request.headers().contains_key("x-api-key"));
    }
    Ok(())
}

#[test]
fn azure_cli_success_keeps_bearer_sensitive_and_replaces_key_headers() -> TestResult {
    isolated("azure_cli_success_keeps_bearer_sensitive_and_replaces_key_headers")
}

#[test]
fn azure_cli_failure_does_not_expose_helper_output() -> TestResult {
    isolated("azure_cli_failure_does_not_expose_helper_output")
}
