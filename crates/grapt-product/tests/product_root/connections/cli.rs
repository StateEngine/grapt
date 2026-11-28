//! Copied CLI operators share the running product's generation and durable store.
use super::*;
use grapt_connections::store::Store;

fn cli(
    binary: &Path,
    profile: &Path,
    provider: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> TestResult<(bool, Value, String)> {
    let (status, response, error) = cli_status(binary, profile, provider, args, input)?;
    Ok((status.success(), response, error))
}

fn cli_status(
    binary: &Path,
    profile: &Path,
    provider: &str,
    args: &[&str],
    input: Option<&[u8]>,
) -> TestResult<(std::process::ExitStatus, Value, String)> {
    let mut command = grapt_service::launcher_environment::command(binary);
    command
        .args(args)
        .arg("--profile")
        .arg(profile)
        .env("GRAPT_LIVE_HEADLESS_LOOPBACK", provider)
        .env(
            grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
            grapt_login::profile_security::isolate_test_keystore()?,
        )
        .env("OPENAI_API_KEY", "fixture-valid-export-only")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut process = RestartProcess(command.spawn()?);
    let (status, out, error) = output::finish(&mut process, input, 18000)?;
    assert!(
        out.len() <= 65536 && error.len() <= 65536,
        "CLI output exceeded its fixture bound"
    );
    assert!(
        !out.contains("fixture-valid-") && !error.contains("fixture-valid-"),
        "CLI exposed a key"
    );
    let response: Value = serde_json::from_str(out.trim())?;
    if matches!(
        input,
        Some(b"fixture-invalid\n" | b"fixture-valid-forbidden\n")
    ) {
        assert_eq!(status.code(), Some(13));
        assert_eq!(response["reason"], "the provider rejected this key");
        assert!(!out.contains("fixture-invalid"));
        assert!(!error.contains("fixture-invalid"));
    }
    if args.starts_with(&["auth", "status"]) {
        assert_eq!(
            status.code(),
            Some(if response["status"] == "signed_out" {
                1
            } else {
                0
            })
        );
    }
    Ok((status, response, error))
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn copied_cli_connects_an_absent_profile_then_changes_and_removes_the_live_connection() -> TestResult
{
    let root = root("connection-cli");
    reset(&root);
    let bin = root.join("installed");
    fs::create_dir(&bin)?;
    let product = bin.join(if cfg!(windows) {
        "grapt-server.exe"
    } else {
        "grapt-server"
    });
    let binary = bin.join(if cfg!(windows) { "grapt.exe" } else { "grapt" });
    fs::copy(shipped_cli::product_binary(), &product)?;
    fs::copy(shipped_cli::cli_binary(), &binary)?;
    let profile = root.join("new-parent").join("profile");
    let KeyProviderFixture {
        endpoint,
        task,
        calls,
    } = key_provider_with_requests(11, 12000)?;
    let (listed, catalog, _) = cli(&binary, &profile, &endpoint, &["login", "--json"], None)?;
    assert!(listed);
    assert!(catalog["providers"].as_array().is_some_and(|rows| rows
        .iter()
        .any(|row| row["id"] == "chatgpt" && row["supported"] == true)));
    assert!(!profile.exists(), "provider listing created a profile");
    let (usable, status, _) = cli(
        &binary,
        &profile,
        &endpoint,
        &["auth", "status", "--json"],
        None,
    )?;
    assert!(!usable);
    assert_eq!(status["connections"], json!([]));
    assert!(!profile.exists(), "status created a profile");
    let (logout_status, absent, _) = cli_status(
        &binary,
        &profile,
        &endpoint,
        &["logout", "work", "--json"],
        None,
    )?;
    assert_eq!(logout_status.code(), Some(3));
    assert_eq!(absent["exit"], "not_found");
    assert!(!profile.exists(), "logout created a profile");
    let (ok, response, error) = cli(
        &binary,
        &profile,
        &endpoint,
        &["login", "openai", "--id", "work", "--key-stdin", "--json"],
        Some(b"fixture-valid-stdin\n"),
    )?;
    assert!(ok, "first CLI login refused: {response} {error}");
    assert_eq!(response["status"], "signed_in");
    let original = Store::read_snapshot(&profile)?.ok_or("connection absent")?;
    let (ok, response, _) = cli(
        &binary,
        &profile,
        &endpoint,
        &["auth", "status", "--json"],
        None,
    )?;
    assert!(ok);
    assert_eq!(response["connections"][0]["provider"], "openai");
    assert_eq!(response["connections"][0]["id"], "work");
    assert_eq!(response["connections"][0]["last4"], "tdin");
    assert_eq!(response["connections"][0]["status"], "ok");
    assert_eq!(
        response["connections"][0]["backend"],
        "system-keystore-encryption"
    );
    let (ok, response, error) = cli(
        &binary,
        &profile,
        &endpoint,
        &[
            "login",
            "openai",
            "--id",
            "work",
            "--api-key-env",
            "OPENAI_API_KEY",
            "--json",
        ],
        None,
    )?;
    assert!(ok, "env login refused: {response} {error}");
    let environment = Store::read_snapshot(&profile)?.ok_or("env connection absent")?;
    assert_eq!(
        environment.connections[0].method,
        grapt_connections::store::AuthMethod::Env {
            name: "OPENAI_API_KEY".into()
        }
    );
    assert!(
        !profile
            .join(&environment.connections[0].credential_file)
            .exists(),
        "env value was persisted as credential material"
    );
    for path in [
        profile.join("connections.json"),
        profile.join("grapt.jsonc"),
        profile.join("logs/server.log"),
    ] {
        let bytes = fs::read(path)?;
        assert!(!bytes
            .windows(b"fixture-valid-export-only".len())
            .any(|part| part == b"fixture-valid-export-only"));
    }
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(&product)
            .env(
                grapt_login::profile_security::TEST_KEYSTORE_VARIABLE,
                grapt_login::profile_security::isolate_test_keystore()?,
            )
            .args(["serve-terminal", "--service"])
            .arg(&profile)
            .args(["--port", "0"])
            .env("OPENAI_API_KEY", "fixture-valid-export-only")
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", &endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    // Windows validates the managed profile and its ACLs before readiness.
    let ready = process.ready_with_polls(&profile, 12000);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|value| value.strip_prefix("http://"))
            .ok_or("endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let methods = client.request("GET", "/api/integration/openai", "")?;
    assert!(methods["data"]["methods"]
        .as_array()
        .is_some_and(|methods| {
            methods.iter().any(|method| {
                method["type"] == "env" && method["names"] == json!(["OPENAI_API_KEY"])
            })
        }));
    assert!(!methods.to_string().contains("fixture-valid-export-only"));
    let session = client.request("POST", "/api/session", &json!({"id":null,"location":{"directory":root.to_string_lossy().replace('\\', "/")},"model":{"providerID":"work","id":"gpt-5.6"}}).to_string())?;
    let session = session["data"]["id"]
        .as_str()
        .ok_or("session absent")?
        .to_owned();
    let (ok, response, error) = cli(
        &binary,
        &profile,
        &endpoint,
        &["login", "openai", "--id", "work", "--key-stdin", "--json"],
        Some(b"fixture-valid-live\n"),
    )?;
    assert!(ok, "live login refused: {response} {error}");
    let live = Store::read_snapshot(&profile)?.ok_or("live connection absent")?;
    assert_ne!(
        live.connections[0].credential_version,
        original.connections[0].credential_version
    );
    let (ok, response, _) = cli(
        &binary,
        &profile,
        &endpoint,
        &["login", "openai", "--id", "work", "--key-stdin", "--json"],
        Some(b"fixture-invalid\n"),
    )?;
    assert!(!ok);
    assert_eq!(response["status"], "failed");
    assert!(response["reason"]
        .as_str()
        .is_some_and(|reason| !reason.is_empty()));
    assert_eq!(Store::read_snapshot(&profile)?, Some(live.clone()));
    let (ok, response, _) = cli(
        &binary,
        &profile,
        &endpoint,
        &["login", "openai", "--id", "work", "--key-stdin", "--json"],
        Some(b"fixture-valid-forbidden\n"),
    )?;
    assert!(!ok);
    assert_eq!(response["status"], "failed");
    assert_eq!(Store::read_snapshot(&profile)?, Some(live));
    let (ok, response, error) = cli(
        &binary,
        &profile,
        &endpoint,
        &["login", "openai", "--id", "work", "--key-stdin", "--json"],
        Some(b"fixture-valid-last\n"),
    )?;
    assert!(ok, "last login refused: {response} {error}");
    let (ok, response, error) = cli(
        &binary,
        &profile,
        &endpoint,
        &["logout", "work", "--json"],
        None,
    )?;
    assert!(ok, "logout refused: {response} {error}");
    assert_eq!(response["revoked"], false);
    assert_eq!(response["status"], "signed_out");
    assert_eq!(
        client.request("GET", &format!("/api/session/{session}"), "")?["data"]["id"],
        session
    );
    assert_eq!(
        client.request("GET", "/api/model/default", "")?["data"],
        Value::Null
    );
    assert!(
        matches!(process.0.try_wait(), Ok(None)),
        "CLI replaced the running service"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 11);
    task.join().map_err(|_| "provider fixture stopped")??;
    process.kill_and_reap();
    let (ok, response, _) = cli(
        &binary,
        &profile,
        &endpoint,
        &["auth", "status", "--json"],
        None,
    )?;
    assert!(!ok);
    assert_eq!(response["status"], "signed_out");
    fs::remove_dir_all(root)?;
    Ok(())
}

#[path = "cli/oauth.rs"]
mod oauth;

#[path = "cli/output.rs"]
mod output;

#[path = "cli/verification.rs"]
mod verification;
