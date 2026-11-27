//! Raw service configuration must not become a process diagnostic.
use std::fs;

#[cfg(any(unix, windows))]
#[test]
fn non_unicode_argument_is_refused_without_panic() -> Result<(), Box<dyn std::error::Error>> {
    #[cfg(unix)]
    let argument = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0xff])
    };
    #[cfg(windows)]
    let argument = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0xd800])
    };
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .arg(argument)
        .output()?;
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert_eq!(output.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("command arguments must be valid Unicode"));
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(receipt["outcome"], "refused");
    Ok(())
}

#[test]
fn malformed_config_secret_is_absent_from_stderr_and_stdout_receipt(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-config-stderr-{}", std::process::id()));
    let initialized =
        grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
            .args(["init-profile", "--service"])
            .arg(&root)
            .output()?;
    assert!(
        initialized.status.success(),
        "empty profile initialization failed: {}",
        String::from_utf8_lossy(&initialized.stderr)
    );
    fs::write(
        root.join("service.conf"),
        b"slot=product\napi_key sk-test-123456\n",
    )?;
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["serve", "--service"])
        .arg(&root)
        .args(["--port", "0"])
        .output()?;
    fs::remove_dir_all(&root)?;
    assert!(
        !output.status.success(),
        "malformed configuration was admitted"
    );
    for bytes in [&output.stderr, &output.stdout] {
        let text = String::from_utf8_lossy(bytes);
        assert!(
            text.contains("line 2"),
            "refusal omitted line number: {text}"
        );
        assert!(
            !text.contains("sk-test") && !text.contains("123456") && !text.contains("api_key"),
            "refusal exposed authored input: {text}"
        );
    }
    let receipt: serde_json::Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(receipt["schema"], "grapt.product.launch.v1");
    assert_eq!(receipt["outcome"], "refused");
    Ok(())
}

#[test]
fn removed_local_initializer_names_connect_without_creating_a_profile(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = std::env::temp_dir().join(format!("grapt-removed-local-{}", std::process::id()));
    assert!(!root.exists(), "fixture profile already exists");
    let output = grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
        .args(["init-local", "--service"])
        .arg(&root)
        .output()?;
    assert!(!output.status.success());
    let message = String::from_utf8_lossy(&output.stderr);
    assert!(
        message.contains("removed") && message.contains("Connect (/connect)"),
        "{message}"
    );
    assert!(message.contains("--profile <new-profile>"), "{message}");
    assert!(
        !root.exists(),
        "refused initialization created profile data"
    );
    Ok(())
}

#[test]
fn legacy_local_profile_refuses_before_store_open_and_points_to_connect(
) -> Result<(), Box<dyn std::error::Error>> {
    let root = tempfile::tempdir()?;
    let config = serde_json::to_vec(&serde_json::json!({
        "store":{"authority":"open-ready"}, "product":{"build":"fixture"},
        "provider":{"id":"deterministic","model":"local-script",
            "base_model":"local-script","family":"local-script","route":"local-script",
            "account":"local","credential_version":"none",
            "policy_version":"first-use-v1","variants":[]}
    }))?;
    fs::write(root.path().join("grapt.jsonc"), &config)?;
    let store = b"preserved legacy store; opening this as SQLite must not precede refusal";
    fs::write(root.path().join("database.sqlite3"), store)?;
    for verb in ["serve", "serve-terminal"] {
        let output =
            grapt_service::launcher_environment::command(env!("CARGO_BIN_EXE_grapt-server"))
                .args([verb, "--service"])
                .arg(root.path())
                .args(["--port", "0"])
                .output()?;
        assert!(!output.status.success(), "legacy profile was admitted");
        let message = String::from_utf8_lossy(&output.stderr);
        assert!(
            message.contains("Local script mode has been removed"),
            "{verb}: {message}"
        );
        assert!(
            message.contains("--profile <new-profile>") && message.contains("Connect (/connect)"),
            "{verb}: {message}"
        );
        assert_eq!(fs::read(root.path().join("grapt.jsonc"))?, config);
        assert_eq!(fs::read(root.path().join("database.sqlite3"))?, store);
        assert!(!root.path().join("registration").exists());
        assert!(!root.path().join("provider.credential").exists());
    }
    Ok(())
}
