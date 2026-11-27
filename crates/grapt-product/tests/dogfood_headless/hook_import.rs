//! Authenticated import previews and writes through the ordinary product.
use super::*;
use serde_json::json as value;

#[test]
fn hook_import_previews_validates_and_keeps_existing_declarations(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("hook-import");
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
        "{\n// retain comment\n\"custom\":true,\"hooks\":{\"PreToolUse\":[]}}",
    )?;
    let before = fs::read(&path)?;
    let provider = ProviderFixture::start(vec![]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let hooks = value!({"PreToolUse": [{"hooks":[{"type":"command","command":"printf PRIVATE_HOOK_BODY"}]}], "Stop":[{"hooks":[{"type":"command","command":"printf PRIVATE_HOOK_BODY","timeout":2}]}]});
    let route = "/api/config/import-hooks";
    let payload = |apply| value!({"hooks":hooks.to_string(),"apply":apply}).to_string();
    let preview = json(address, "POST", route, &payload(false));
    assert_eq!(preview["imported"], value!(["Stop"]));
    assert_eq!(preview["skipped"], value!(["PreToolUse"]));
    assert_eq!(fs::read(&path)?, before);
    let applied = json(address, "POST", route, &payload(true));
    assert_eq!(applied["imported"], value!(["Stop"]));
    let text = fs::read_to_string(&path)?;
    assert!(text.contains("retain comment"));
    assert!(text.contains("\"custom\":true"));
    let repeated = json(address, "POST", route, &payload(true));
    assert_eq!(repeated["imported"], value!([]));
    assert_eq!(fs::read_to_string(&path)?, text);
    let malformed = value!({"hooks":value!({"PostToolUse":[{"hooks":[{"type":"prompt","prompt":"PRIVATE_HOOK_BODY"}]}]}).to_string(),"apply":true});
    let (status, body) = request(address, "POST", route, &malformed.to_string());
    assert_eq!(status, 400);
    assert!(!String::from_utf8_lossy(&body).contains("PRIVATE_HOOK_BODY"));
    assert_eq!(fs::read_to_string(&path)?, text);
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(config.join(".mcp-config.lock"))?;
    lock.try_lock()?;
    let fresh = value!({"hooks":value!({"PostToolUse":[{"hooks":[{"type":"command","command":"printf PRIVATE_HOOK_BODY"}]}]}).to_string(),"apply":true});
    assert_eq!(request(address, "POST", route, &fresh.to_string()).0, 400);
    assert_eq!(fs::read_to_string(&path)?, text);
    drop(lock);
    // A repository created after startup cannot turn its configuration into
    // operator-owned executable hooks, even without a captured profile.
    let git = root.join(".git");
    fs::create_dir_all(git.join("objects"))?;
    fs::create_dir_all(git.join("refs"))?;
    fs::write(git.join("HEAD"), "ref: refs/heads/main\n")?;
    fs::write(
        git.join("config"),
        "[core]\nrepositoryformatversion=0\nbare=false\n",
    )?;
    let (status, refusal) = request(address, "POST", route, &fresh.to_string());
    assert_eq!(status, 400);
    assert!(String::from_utf8(refusal)?.contains(
        "hook imports require user configuration outside the project and its repositories"
    ));
    assert_eq!(fs::read_to_string(&path)?, text);
    fs::remove_dir_all(git)?;
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn hook_import_cannot_promote_a_captured_repository_profile(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("hook-import-repository");
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
    let payload = value!({"hooks": value!({"Stop":[{"hooks":[{"type":"command","command":"printf PRIVATE_HOOK_BODY"}]}]}).to_string(), "apply":true});
    let (status, body) = request(
        address,
        "POST",
        "/api/config/import-hooks",
        &payload.to_string(),
    );
    assert_eq!(status, 400);
    assert!(!String::from_utf8_lossy(&body).contains("PRIVATE_HOOK_BODY"));
    assert_eq!(fs::read(root.join("grapt.jsonc"))?, before);
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
    Ok(())
}
