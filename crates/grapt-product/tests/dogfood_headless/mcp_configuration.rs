//! Operator edits persist through the ordinary catalog and refuse unsafe files.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn mcp_user_configuration_edits_preserve_comments_and_refuse_invalid_or_busy_inputs(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-user-config");
    prepare(&root, 120_000);
    let config = root.join("global-config");
    fs::create_dir_all(&config)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
    }
    let path = config.join("grapt.jsonc");
    fs::write(
        &path,
        r#"{
        // keep this user comment
        "custom": {"keep": true},
        "mcp": {"servers": {
            "old": {"type":"local","command":["not-started"],"disabled":true,"enabled":false},
        }},
    }"#,
    )?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let route = "/api/mcp/docs/configuration";
    for body in [
        r#"{"action":"add","config":{"type":"local","command":["not-started"],"disabled":true}}"#,
        r#"{"action":"add","config":{"type":"remote","url":"https://example.invalid/mcp","disabled":true}}"#,
    ] {
        let (status, bytes) = request(address, "POST", route, body);
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&bytes));
    }
    let text = fs::read_to_string(&path)?;
    assert!(text.contains("keep this user comment"));
    let bounds = grapt_config::ConfigBounds::new(
        grapt_config::MaxDocumentBytes::try_new(65536)?,
        grapt_config::MaxAncestorDepth::try_new(64)?,
        grapt_config::MaxDocuments::try_new(256)?,
        grapt_config::MaxValueDepth::try_new(16)?,
        grapt_config::MaxDocumentNodes::try_new(8192)?,
    );
    let value = grapt_config::decode(text.as_bytes(), bounds)?;
    assert!(value.value().get("custom").is_some());
    let catalog = json(address, "GET", "/api/mcp", "");
    assert!(catalog["data"]
        .as_array()
        .ok_or("catalog rows")?
        .iter()
        .any(|row| row["name"] == "docs" && row["status"]["status"] == "disabled"));
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("created session")?
        .to_owned();
    for enabled in [true, false] {
        let (status, body) = request(
            address,
            "POST",
            &format!("/api/mcp/old/configuration?session={session}"),
            &serde_json::json!({"action":"set_enabled", "enabled":enabled}).to_string(),
        );
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
        let catalog = json(address, "GET", "/api/mcp", "");
        let expected = if enabled { "failed" } else { "disabled" };
        assert!(
            catalog["data"]
                .as_array()
                .ok_or("catalog rows")?
                .iter()
                .any(|row| row["name"] == "old" && row["status"]["status"] == expected),
            "{catalog}"
        );
    }
    let before = fs::read(&path)?;
    let (status, _) = request(
        address,
        "POST",
        "/api/mcp/missing/configuration",
        r#"{"action":"set_enabled","enabled":true}"#,
    );
    assert_eq!(status, 400);
    assert_eq!(fs::read(&path)?, before);
    for name in ["old", "docs", "docs"] {
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/mcp/{name}/configuration"),
            r#"{"action":"remove"}"#,
        );
        assert_eq!(status, 204);
    }
    let (status, _) = request(
        address,
        "POST",
        route,
        r#"{"action":"add","config":{"type":"remote","url":"file:///secret"}}"#,
    );
    assert_eq!(status, 400);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let before = fs::read(&path)?;
        fs::set_permissions(&config, fs::Permissions::from_mode(0o500))?;
        let (status, bytes) = request(
            address,
            "POST",
            route,
            r#"{"action":"add","config":{"type":"local","command":["not-started"],"disabled":true}}"#,
        );
        fs::set_permissions(&config, fs::Permissions::from_mode(0o700))?;
        assert_eq!(status, 400);
        assert!(String::from_utf8_lossy(&bytes).contains("PermissionDenied"));
        assert_eq!(fs::read(&path)?, before);
    }
    let before = fs::read(&path)?;
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(config.join(".mcp-config.lock"))?;
    lock.try_lock()?;
    let (status, _) = request(address, "POST", route, r#"{"action":"remove"}"#);
    assert_eq!(status, 400);
    assert_eq!(fs::read(&path)?, before);
    drop(lock);
    let malformed = br#"{"mcp":{},"mcp":{}}"#;
    fs::write(&path, malformed)?;
    let (status, _) = request(address, "POST", route, r#"{"action":"remove"}"#);
    assert_eq!(status, 400);
    assert_eq!(fs::read(&path)?, malformed);
    #[cfg(unix)]
    {
        let outside = root.join("outside.jsonc");
        fs::write(&outside, b"{}")?;
        fs::remove_file(&path)?;
        std::os::unix::fs::symlink(&outside, &path)?;
        let (status, _) = request(address, "POST", route, r#"{"action":"remove"}"#);
        assert_eq!(status, 400);
        assert_eq!(fs::read(&outside)?, b"{}");
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn mcp_configuration_refuses_the_process_captured_repository_profile(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("mcp-captured-profile");
    prepare(&root, 120_000);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700))?;
    }
    let approval = grapt_platform::profile_configuration::ProfileConfiguration::capture(&root)?
        .approval()
        .encoded()?;
    let before = fs::read(root.join("grapt.jsonc"))?;
    let provider = ProviderFixture::start(vec![]);
    let home = root.join("home");
    fs::create_dir_all(&home)?;
    let mut command = grapt_service::launcher_environment::command(shipped_cli::product_binary());
    command
        .args([
            "serve-terminal",
            "--profile-approval",
            &approval,
            "--service",
        ])
        .arg(&root)
        .args(["--port", "0"])
        .stdin(Stdio::piped())
        .env(
            "GRAPT_LIVE_HEADLESS_LOOPBACK",
            format!("http://{}", provider.address),
        )
        .env("GRAPT_CONFIG_DIR", &root)
        .env("GRAPT_TEST_HOME", &home)
        .env("GRAPT_TEST_CATALOG_ROOT", &root);
    grapt_service::launcher_environment::scrub(&mut command);
    let mut child = process_fixture::spawn_captured(&root, &mut command);
    let address = await_ready(&root, &mut child);
    let (status, bytes) = request(
        address,
        "POST",
        "/api/mcp/docs/configuration",
        r#"{"action":"remove"}"#,
    );
    assert_eq!(status, 400);
    assert!(String::from_utf8_lossy(&bytes).contains("approved repository profile"));
    assert_eq!(fs::read(root.join("grapt.jsonc"))?, before);
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
