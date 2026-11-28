//! Public signing selection with synthetic inputs and no provider requests.
use grapt_login::aws::{AwsError, Selection};
use grapt_types::{ClockAuthority, Timestamp};
use std::{path::PathBuf, process::Command};

type TestResult = Result<(), Box<dyn std::error::Error>>;
const CHILD: &str = "GRAPT_AWS_SELECTION_TEST";

#[derive(Debug)]
struct Clock;
impl ClockAuthority for Clock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(1_800_000_000_000)
    }
}

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        let _removed = std::fs::remove_dir_all(&self.0);
    }
}

fn run(name: &str) -> TestResult {
    if std::env::var(CHILD).as_deref() == Ok(name) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()?;
        return runtime.block_on(check(name));
    }
    let path =
        std::env::temp_dir().join(format!("grapt-aws-selection-{}-{name}", std::process::id()));
    std::fs::create_dir(&path)?;
    let fixture = Fixture(std::fs::canonicalize(path)?);
    let helper = name.contains("helper");
    std::fs::write(
        fixture.0.join("credentials"),
        if helper {
            ""
        } else {
            "[selected]\naws_access_key_id=AKIAEXAMPLESELECTED\naws_secret_access_key=synthetic-profile-secret\n\
             [other]\naws_access_key_id=AKIAEXAMPLEOTHER\naws_secret_access_key=synthetic-other-secret\n"
        },
    )?;
    std::fs::write(
        fixture.0.join("config"),
        if helper {
            "[profile selected]\ncredential_process=echo launched > helper-launched\n\
             [profile role]\nrole_arn=arn:aws:iam::000000000000:role/fixture\nsource_profile=selected\n"
        } else {
            ""
        },
    )?;
    let mut command = Command::new(std::env::current_exe()?);
    command.env_clear();
    for key in ["SystemRoot", "WINDIR", "TEMP", "TMP"] {
        if let Some(value) = std::env::var_os(key) {
            command.env(key, value);
        }
    }
    let status = command
        .args([
            "--exact",
            &crate::exact(module_path!(), name),
            "--nocapture",
        ])
        .current_dir(&fixture.0)
        .env(CHILD, name)
        .env("HOME", &fixture.0)
        .env("USERPROFILE", &fixture.0)
        .env("AWS_CONFIG_FILE", fixture.0.join("config"))
        .env("AWS_SHARED_CREDENTIALS_FILE", fixture.0.join("credentials"))
        .env("AWS_EC2_METADATA_DISABLED", "true")
        .env("AWS_PROFILE", "other")
        .env("AWS_ACCESS_KEY_ID", "AKIAEXAMPLEAMBIENT")
        .env("AWS_SECRET_ACCESS_KEY", "synthetic-ambient-secret")
        .status()?;
    assert!(status.success(), "isolated AWS signing control failed");
    assert!(
        !fixture.0.join("helper-launched").try_exists()?,
        "credential helper ran"
    );
    Ok(())
}

async fn check(name: &str) -> TestResult {
    let profile = match name {
        "default_chain_uses_environment_credentials" => None,
        "missing_explicit_profile_cannot_fall_back_to_environment" => Some("missing"),
        "role_source_helper_is_refused_without_execution" => Some("role"),
        _ => Some("selected"),
    };
    let selection = Selection::new("us-east-1".into(), profile.map(str::to_owned))?;
    let mut request = reqwest::Request::new(
        reqwest::Method::POST,
        "https://bedrock-runtime.us-east-1.amazonaws.com/model/fixture/converse".parse()?,
    );
    *request.body_mut() = Some(reqwest::Body::from("{}"));
    request
        .headers_mut()
        .insert("authorization", "stale-authorization".parse()?);
    request
        .headers_mut()
        .insert("x-amz-security-token", "stale-session-token".parse()?);
    let result = selection.sign(request, &Clock).await;
    if name.starts_with("missing_") {
        assert!(matches!(result, Err(AwsError::Unavailable)));
    } else if name.contains("helper") {
        assert!(matches!(result, Err(AwsError::ProfileConfiguration)));
    } else {
        let request = result?;
        let authorization = request
            .headers()
            .get("authorization")
            .ok_or("signature missing")?
            .to_str()?;
        let expected = if profile.is_some() {
            "AKIAEXAMPLESELECTED"
        } else {
            "AKIAEXAMPLEAMBIENT"
        };
        assert!(authorization.contains(&format!("Credential={expected}/")));
        assert!(!authorization.contains("AKIAEXAMPLEOTHER"));
        assert!(!request.headers().contains_key("x-amz-security-token"));
        assert_eq!(
            request.body().and_then(reqwest::Body::as_bytes),
            Some(b"{}".as_slice())
        );
    }
    Ok(())
}

#[test]
fn explicit_profile_outranks_environment_credentials() -> TestResult {
    run("explicit_profile_outranks_environment_credentials")
}
#[test]
fn missing_explicit_profile_cannot_fall_back_to_environment() -> TestResult {
    run("missing_explicit_profile_cannot_fall_back_to_environment")
}
#[test]
fn default_chain_uses_environment_credentials() -> TestResult {
    run("default_chain_uses_environment_credentials")
}
#[test]
fn helper_profile_is_refused_without_execution() -> TestResult {
    run("helper_profile_is_refused_without_execution")
}
#[test]
fn role_source_helper_is_refused_without_execution() -> TestResult {
    run("role_source_helper_is_refused_without_execution")
}
