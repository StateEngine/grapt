//! Connection diagnostics through the real CLI and its attributed local API.
use super::{CliLimits, Duration, HelpSandbox, Output, Stdio};
use grapt_service::RegistrationRecord;
use grapt_types::authority::Timestamp;
use grapt_types::service::{
    Endpoint, EnvironmentReceipt, OwnerReceipt, ProcessId, ServiceGeneration, ServiceInstanceId,
    ServiceRegistrationVersion, ServiceSlotId,
};
use serde_json::{json, Value};
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;
const KEY: &str = "fixture-submitted-key";
const FIELD: &str = "fixture-untrusted-field";

fn typed(message: &str) -> String {
    json!({"_tag":"InvalidRequestError","message":message,"field":FIELD}).to_string()
}

async fn login(status: u16, body: &str, environment: bool, structured: bool) -> TestResult<Output> {
    login_request(status, body, environment, structured, "google").await
}

#[expect(clippy::too_many_lines, reason = "legacy")]
async fn login_request(
    status: u16,
    body: &str,
    environment: bool,
    structured: bool,
    provider: &str,
) -> TestResult<Output> {
    let aws = provider == "amazon-bedrock";
    let adc = matches!(provider, "google-vertex" | "google-vertex-anthropic");
    let key_route = format!("/api/integration/{provider}/connect/key");
    let root = HelpSandbox::new()?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let endpoint = format!("http://{}", listener.local_addr()?);
    let pid = std::process::id();
    let owner = OwnerReceipt::new(
        ServiceSlotId::new("grapt").ok_or("slot")?,
        ServiceInstanceId::new("instance-live").ok_or("instance")?,
        EnvironmentReceipt::new("test").ok_or("environment")?,
        ServiceGeneration::new(7),
        ProcessId::new(pid).ok_or("pid")?,
        Endpoint::new(&endpoint).ok_or("endpoint")?,
    );
    let registration = RegistrationRecord::new(
        owner,
        ServiceRegistrationVersion::new(env!("CARGO_PKG_VERSION")).ok_or("version")?,
        Timestamp::from_millis(1),
    )?;
    std::fs::write(root.0.join("registration"), registration.encode())?;
    std::fs::write(root.0.join("basic.credential"), b"test:test\n")?;
    std::fs::write(
        root.0.join("serve-ready.json"),
        json!({
            "schema":"secode.product.ready.v2","operation":"serve","service_slot":"grapt",
            "service_instance":"instance-live","environment":"test","service_generation":7,
            "pid":pid,"observed_endpoint":endpoint,"serve_window_ms":1,"build":"diagnostic-test"
        })
        .to_string(),
    )?;
    let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":"diagnostic-test",
        "environment":"test","service_instance":"instance-live"})
    .to_string();
    let peer = async {
        for (method, path, code, response) in [
            ("GET", "/api/health", 200, health.as_str()),
            (
                "POST",
                if environment {
                    "/api/integration/google/connect/env"
                } else {
                    key_route.as_str()
                },
                status,
                body,
            ),
        ] {
            let (stream, _) = listener.accept().await?;
            let mut stream = BufReader::new(stream);
            let mut headers = String::new();
            loop {
                if headers.len() >= 8192 {
                    return Err("fixture headers exceeded bound".into());
                }
                let mut byte = [0];
                stream.read_exact(&mut byte).await?;
                headers.push(char::from(byte[0]));
                if headers.ends_with("\r\n\r\n") {
                    break;
                }
            }
            assert!(headers.starts_with(&format!("{method} {path} HTTP/1.1\r\n")));
            assert!(headers
                .to_ascii_lowercase()
                .contains("authorization: basic dgvzddp0zxn0"));
            let length = headers
                .lines()
                .find_map(|line| {
                    line.split_once(':')
                        .filter(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                        .map(|(_, value)| value.trim().parse::<usize>())
                })
                .transpose()?
                .unwrap_or(0);
            if length > 65536 {
                return Err("fixture body exceeded bound".into());
            }
            let mut request = vec![0; length];
            stream.read_exact(&mut request).await?;
            if method == "POST" {
                assert!(headers
                    .to_ascii_lowercase()
                    .contains("x-grapt-connection-id: work"));
                let request: Value = serde_json::from_slice(&request)?;
                if aws {
                    assert_eq!(request["key"], "");
                    assert_eq!(request["bedrock"]["auth"]["type"], "aws-chain");
                    assert_eq!(request["bedrock"]["region"], "us-east-1");
                    assert_eq!(request["bedrock"]["model"], "fixture-model");
                } else if adc {
                    assert_eq!(request["key"], "");
                    assert_eq!(
                        request["vertex"],
                        json!({"auth":"application-default",
                        "project":"fixture-project","location":"us-central1","model":"fixture-model"})
                    );
                } else {
                    assert_eq!(
                        request,
                        if environment {
                            json!({"name":"GOOGLE_API_KEY"})
                        } else {
                            json!({"key":KEY})
                        }
                    );
                }
            }
            stream.write_all(format!("HTTP/1.1 {code} Fixture\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{response}", response.len()).as_bytes()).await?;
        }
        Ok::<_, Box<dyn std::error::Error>>(())
    };
    let child = async {
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        command
            .args(["login", provider, "--id", "work", "--profile"])
            .arg(&root.0)
            .env_clear()
            .env("HOME", &root.0)
            .env("USERPROFILE", &root.0)
            .env("APPDATA", &root.0)
            .env("LOCALAPPDATA", &root.0)
            .env("XDG_CONFIG_HOME", &root.0)
            .env("XDG_DATA_HOME", &root.0)
            .env("PATH", root.0.join("no-programs"))
            .current_dir(&root.0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        #[cfg(windows)]
        command.env(
            "SystemRoot",
            std::env::var_os("SystemRoot").ok_or("SystemRoot absent")?,
        );
        if structured {
            command.arg("--json");
        }
        if aws {
            command.args(["--aws", "--region", "us-east-1", "--model", "fixture-model"]);
        } else if adc {
            command.args([
                "--google-adc",
                "--google-project",
                "fixture-project",
                "--google-location",
                "us-central1",
                "--model",
                "fixture-model",
            ]);
        } else if environment {
            command.args(["--api-key-env", "GOOGLE_API_KEY"]);
        } else {
            command.arg("--key-stdin");
        }
        let mut child = command.spawn()?;
        let mut stdin = child.stdin.take().ok_or("child stdin absent")?;
        if !environment && !aws && !adc {
            stdin.write_all(format!("{KEY}\n").as_bytes()).await?;
        }
        drop(stdin);
        Ok::<_, Box<dyn std::error::Error>>(child.wait_with_output().await?)
    };
    let (output, served) =
        tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(child, peer) }).await?;
    served?;
    let output = output?;
    for bytes in [&output.stdout, &output.stderr] {
        let text = std::str::from_utf8(bytes)?;
        assert!(!text.contains(KEY), "submitted key exposed");
        assert!(!text.contains(FIELD), "unselected response field exposed");
    }
    assert!(output.stdout.len() <= CliLimits::production().values().stdout_bytes);
    assert!(output.stderr.len() <= CliLimits::production().values().stderr_bytes);
    Ok(output)
}

fn failure(output: &Output, code: i32) -> TestResult<String> {
    assert_eq!(output.status.code(), Some(code), "{output:?}");
    assert!(output.stderr.is_empty());
    let response: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(response["status"], "failed");
    Ok(response["reason"]
        .as_str()
        .ok_or("failure reason absent")?
        .to_owned())
}

#[tokio::test]
async fn typed_connection_cause_is_feature_scoped_and_redacts_each_submitted_key() -> TestResult {
    let body = typed(&format!("fixture-local-cause {KEY} and {KEY}"));
    for structured in [true, false] {
        let output = login(400, &body, false, structured).await?;
        assert_eq!(output.status.code(), Some(4));
        let reason = if structured {
            failure(&output, 4)?
        } else {
            assert!(output.stdout.is_empty());
            String::from_utf8(output.stderr)?
        };
        if cfg!(feature = "loopback-testkit") {
            assert!(reason.contains("fixture-local-cause"));
            assert_eq!(reason.matches("[redacted]").count(), 2);
        } else {
            assert!(!reason.contains("fixture-local-cause"));
        }
    }
    Ok(())
}

#[tokio::test]
async fn invalid_connection_causes_are_refused_without_partial_contents() -> TestResult {
    for message in [
        String::new(),
        format!(
            "private-prefix{}",
            "x".repeat(1025 - "private-prefix".len())
        ),
        format!("private-prefix{}", "é".repeat(513)),
        "private-prefix\nline".into(),
        "private-prefix\u{1b}[31m".into(),
        "private-prefix\u{85}line".into(),
    ] {
        let reason = failure(&login(400, &typed(&message), false, true).await?, 4)?;
        assert!(!reason.contains("private-prefix"));
        if cfg!(feature = "loopback-testkit") {
            assert!(reason.contains("local connection diagnostic"));
        }
        assert!(reason.len() < 256);
    }
    if cfg!(feature = "loopback-testkit") {
        let allowed = "a".repeat(1024);
        assert!(failure(&login(400, &typed(&allowed), false, true).await?, 4)?.contains(&allowed));
    }
    Ok(())
}

#[tokio::test]
async fn malformed_and_oversized_connection_errors_do_not_expose_bodies() -> TestResult {
    for body in [
        format!("not-json {KEY} private-body"),
        json!({"error":format!("{KEY} private-body")}).to_string(),
        json!({"_tag":"InvalidRequestError","message":42,"field":KEY}).to_string(),
    ] {
        let reason = failure(&login(400, &body, false, true).await?, 4)?;
        assert!(!reason.contains("private-body"));
        assert!(!reason.contains("local connection diagnostic"));
        assert!(reason.len() < 256);
    }
    let body = typed(&format!("{KEY} private-body{}", "x".repeat(8192)));
    let reason = failure(&login(400, &body, false, true).await?, 4)?;
    assert!(reason.contains("8192"));
    assert!(!reason.contains("private-body"));
    Ok(())
}

#[tokio::test]
async fn key_rejection_precedes_diagnostics_and_other_statuses_stay_remote() -> TestResult {
    let body = json!({"_tag":"InvalidRequestError","kind":"integration_key_rejected",
        "message":format!("{KEY} private-body\n")})
    .to_string();
    assert_eq!(
        failure(&login(400, &body, false, true).await?, 13)?,
        "the provider rejected this key"
    );
    let reason = failure(&login(500, &typed("private-body"), false, true).await?, 12)?;
    assert!(!reason.contains("private-body"));
    Ok(())
}

#[tokio::test]
async fn environment_connection_keeps_status_only_errors_and_success() -> TestResult {
    let reason = failure(
        &login(400, &typed("private-env-cause"), true, true).await?,
        4,
    )?;
    assert!(!reason.contains("private-env-cause"));
    assert!(!reason.contains("local connection diagnostic"));
    for environment in [false, true] {
        let body = json!({"_tag":"InvalidRequestError", "kind":"integration_authorization",
            "message":format!("private-verification-body {KEY}")})
        .to_string();
        let reason = failure(&login(400, &body, environment, true).await?, 12)?;
        assert_eq!(reason, "provider verification did not complete");
        let output = login(204, "", environment, true).await?;
        assert_eq!(output.status.code(), Some(0));
        assert!(output.stderr.is_empty());
        let value: Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(value["status"], "signed_in");
        assert_eq!(value["provider"], "google");
    }
    Ok(())
}

#[tokio::test]
async fn binary_http_failures_keep_distinct_exit_classes() -> TestResult {
    for (status, code) in [
        (404, 3),
        (401, 13),
        (403, 4),
        (409, 4),
        (500, 12),
        (503, 12),
    ] {
        for structured in [false, true] {
            let output = login(status, "", false, structured).await?;
            assert_eq!(output.status.code(), Some(code), "{output:?}");
        }
    }
    Ok(())
}

#[tokio::test]
async fn typed_cloud_refusals_keep_safe_causes_and_authentication_exit_status() -> TestResult {
    for (provider, kind, expected) in [
        (
            "amazon-bedrock",
            grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_KIND,
            grapt_api::u3a::INTEGRATION_AWS_CREDENTIALS_MESSAGE,
        ),
        (
            "amazon-bedrock",
            grapt_api::u3a::INTEGRATION_AWS_PROFILE_KIND,
            grapt_api::u3a::INTEGRATION_AWS_PROFILE_MESSAGE,
        ),
        (
            "google-vertex",
            grapt_api::u3a::INTEGRATION_GOOGLE_ADC_KIND,
            grapt_api::u3a::INTEGRATION_GOOGLE_ADC_MESSAGE,
        ),
        (
            "google-vertex-anthropic",
            grapt_api::u3a::INTEGRATION_GOOGLE_ADC_KIND,
            grapt_api::u3a::INTEGRATION_GOOGLE_ADC_MESSAGE,
        ),
    ] {
        let body = json!({"_tag":"InvalidRequestError", "kind":kind, "message":format!("private-backend-body {KEY}")}).to_string();
        for structured in [true, false] {
            let output = login_request(400, &body, false, structured, provider).await?;
            assert_eq!(output.status.code(), Some(13));
            let reason = if structured {
                failure(&output, 13)?
            } else {
                assert!(output.stdout.is_empty());
                String::from_utf8(output.stderr)?
            };
            assert!(reason.contains(expected), "{reason}");
            assert!(!reason.contains("private-backend-body"));
        }
    }
    Ok(())
}
