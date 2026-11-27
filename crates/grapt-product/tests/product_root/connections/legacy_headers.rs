//! Older clients' named key writes reach the retained connection, not its default.
use super::*;

#[test]
fn legacy_connection_header_writes_the_named_key_without_replacing_the_default() -> TestResult {
    let root = root("legacy-connection-target");
    reset(&root);
    let profile = root.join("profile");
    assert!(Command::new(shipped_cli::product_binary())
        .args(["init-profile", "--service"])
        .arg(&profile)
        .output()?
        .status
        .success());
    let provider = key_provider_with_requests(4, 3000)?;
    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(&profile)?
        .approval()
        .encoded()?;
    let mut process = RestartProcess(
        grapt_service::launcher_environment::command(shipped_cli::product_binary())
            .args([
                "serve-terminal",
                "--profile-approval",
                &approval,
                "--service",
            ])
            .arg(&profile)
            .args(["--port", "0"])
            .env("GRAPT_LIVE_HEADLESS_LOOPBACK", &provider.endpoint)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?,
    );
    let ready = process.ready(&profile);
    let client = Client {
        address: ready["observed_endpoint"]
            .as_str()
            .and_then(|url| url.strip_prefix("http://"))
            .ok_or("service endpoint absent")?
            .parse()?,
        authentication: base64::engine::general_purpose::STANDARD
            .encode(fs::read(profile.join("basic.credential"))?),
    };
    let route = "/api/integration/openai/connect/key";
    client.request("POST", route, r#"{"key":"fixture-valid-default"}"#)?;
    let before = grapt_connections::store::Store::read_snapshot(&profile)?
        .ok_or("default connection absent")?;
    let default = before
        .connections
        .iter()
        .find(|row| row.id.as_str() == "openai")
        .ok_or("default absent")?;
    let default_bytes = fs::read(profile.join(&default.credential_file))?;
    let (status, value) = client.response_with_headers(
        "POST",
        route,
        r#"{"key":"fixture-valid-named"}"#,
        true,
        &[("x-secode-connection-id", "work")], // grapt-rename: keep
    )?;
    assert_eq!(status, 204, "named legacy write refused: {value}");
    let after =
        grapt_connections::store::Store::read_snapshot(&profile)?.ok_or("connections absent")?;
    assert_eq!(after.connections.len(), before.connections.len() + 1);
    assert_eq!(
        after
            .connections
            .iter()
            .find(|row| row.id.as_str() == "openai"),
        Some(default)
    );
    assert_eq!(
        fs::read(profile.join(&default.credential_file))?,
        default_bytes
    );
    let named = after
        .connections
        .iter()
        .find(|row| row.id.as_str() == "work")
        .ok_or("named connection absent")?;
    assert_named_key(&profile, named)?;
    provider
        .task
        .join()
        .map_err(|_| "provider fixture stopped")?
        .map_err(|error| error.to_string())?;
    assert_eq!(provider.calls.load(Ordering::SeqCst), 4);
    process.kill_and_reap();
    fs::remove_dir_all(root)?;
    Ok(())
}

fn assert_named_key(profile: &Path, named: &grapt_connections::store::Connection) -> TestResult {
    use grapt_login::vault::{CredentialVault, CredentialView, SealedFileVault, VaultKey};
    let vault = SealedFileVault::open(profile)?;
    let key = VaultKey::new(named.id.clone(), &named.credential_file)?;
    let secret = vault
        .get(
            &key,
            named
                .credential_format
                .binding()
                .ok_or("named key not bound")?,
        )?
        .ok_or("named credential absent")?;
    match secret.expose() {
        CredentialView::ApiKey(key) => assert_eq!(key, "fixture-valid-named"),
        _ => return Err("named key has the wrong credential kind".into()),
    }
    Ok(())
}
