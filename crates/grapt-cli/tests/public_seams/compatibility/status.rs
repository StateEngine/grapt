//! Read-only status contracts at the real executable boundary.
use super::*;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::time::Duration;

fn snapshot(root: &Path) -> TestResult<BTreeMap<OsString, Vec<u8>>> {
    std::fs::read_dir(root)?
        .map(|entry| {
            let entry = entry?;
            assert!(entry.file_type()?.is_file(), "status created a directory");
            Ok((entry.file_name(), std::fs::read(entry.path())?))
        })
        .collect()
}

async fn status(root: &Path, arguments: &[&str]) -> TestResult<std::process::Output> {
    let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
    command
        .args(arguments)
        .arg("--service")
        .arg(root)
        .env_clear()
        .env("HOME", root)
        .env("USERPROFILE", root)
        .env("APPDATA", root)
        .env("LOCALAPPDATA", root)
        .env("XDG_CONFIG_HOME", root)
        .env("XDG_DATA_HOME", root)
        .env("PATH", root.join("no-programs"))
        .current_dir(root)
        .stdin(Stdio::null())
        .kill_on_drop(true);
    // Winsock needs the OS installation root even in an isolated child.
    #[cfg(windows)]
    command.env(
        "SystemRoot",
        std::env::var_os("SystemRoot").ok_or("Windows SystemRoot is unavailable")?,
    );
    Ok(tokio::time::timeout(Duration::from_secs(10), command.output()).await??)
}

fn discovery_refusal(output: &std::process::Output, diagnostic: &str) -> TestResult {
    assert_eq!(
        output.status.code(),
        Some(i32::from(ExitClass::Discovery.code()))
    );
    assert!(output.stdout.is_empty());
    let stderr = std::str::from_utf8(&output.stderr)?;
    assert!(stderr.starts_with("grapt: service "), "{stderr}");
    assert!(stderr.contains(diagnostic), "{stderr}");
    assert_eq!(stderr.lines().count(), 1);
    assert!(stderr.len() <= CliLimits::production().values().stderr_bytes);
    Ok(())
}

#[tokio::test]
async fn service_status_prints_stopped_when_no_service_is_registered() -> TestResult {
    let root = Scratch::new("service-status-stopped")?;
    let before = snapshot(root.path())?;
    discovery_refusal(
        &status(root.path(), &["status"]).await?,
        "registration is absent",
    )?;
    assert_eq!(snapshot(root.path())?, before);
    let output = status(root.path(), &["service", "status"]).await?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"stopped\n");
    assert!(output.stderr.is_empty());
    assert_eq!(snapshot(root.path())?, before);
    Ok(())
}

#[tokio::test]
async fn stale_status_refuses_but_compatibility_reports_stopped_without_launching() -> TestResult {
    let root = Scratch::new("service-status-stale")?;
    // An owned endpoint refuses attribution; no recycled port or real daemon.
    let (endpoint, server) = spawn_server(vec![
        Script::status("GET", "/api/health", 503),
        Script::status("GET", "/api/health", 503),
    ])
    .await?;
    write_discovery_fixture(root.path(), std::process::id(), &endpoint, "status-test")?;
    let before = snapshot(root.path())?;
    discovery_refusal(
        &status(root.path(), &["status"]).await?,
        "health could not be attributed",
    )?;
    assert_eq!(snapshot(root.path())?, before);
    let output = status(root.path(), &["service", "status"]).await?;
    assert_eq!(output.status.code(), Some(0));
    assert_eq!(output.stdout, b"stopped\n");
    assert!(output.stderr.is_empty());
    assert_eq!(snapshot(root.path())?, before);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), server).await???,
        ["GET /api/health", "GET /api/health"]
    );
    Ok(())
}

#[tokio::test]
async fn malformed_registration_is_a_named_failure_for_both_status_forms() -> TestResult {
    let root = Scratch::new("service-status-malformed")?;
    std::fs::write(root.path().join("registration"), b"not a registration\n")?;
    let before = snapshot(root.path())?;
    for arguments in [&["status"][..], &["service", "status"][..]] {
        discovery_refusal(
            &status(root.path(), arguments).await?,
            "registration is malformed",
        )?;
        assert_eq!(snapshot(root.path())?, before);
    }
    Ok(())
}

#[tokio::test]
async fn live_status_text_json_jsonl_and_compatibility_share_the_attributed_identity() -> TestResult
{
    let root = Scratch::new("service-status-live")?;
    let pid = std::process::id();
    let build = "status-test";
    let health = format!(
        r#"{{"schema":"secode.api.health.v1","pid":{pid},"build":"{build}","environment":"test","service_instance":"instance-live"}}"#
    );
    let scripts = (0..4)
        .map(|_| Script::json("GET", "/api/health", 200, &health))
        .collect();
    let (endpoint, server) = spawn_server(scripts).await?;
    write_discovery_fixture(root.path(), pid, &endpoint, build)?;
    let before = snapshot(root.path())?;
    let expected = serde_json::json!({
        "schema": "secode.cli.service-status.v1",
        "outcome": "ready",
        "serviceSlot": "grapt",
        "serviceInstance": "instance-live",
        "environment": "test",
        "generation": 7,
        "pid": pid,
        "endpoint": endpoint,
        "registrationVersion": env!("CARGO_PKG_VERSION"),
    });
    for (arguments, text) in [
        (vec!["status"], Some(format!("ready {endpoint}\n"))),
        (vec!["status", "--json"], None),
        (vec!["status", "--jsonl"], None),
        (vec!["service", "status"], Some(format!("{endpoint}\n"))),
    ] {
        let output = status(root.path(), &arguments).await?;
        assert_eq!(
            output.status.code(),
            Some(0),
            "{arguments:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            output.stderr.is_empty(),
            "{arguments:?}: {:?}",
            output.stderr
        );
        let stdout = std::str::from_utf8(&output.stdout)?;
        assert!(!stdout.contains("test:test"));
        assert!(!stdout.contains("dGVzdDp0ZXN0"));
        if let Some(text) = text {
            assert_eq!(stdout, text);
        } else {
            let json = if arguments.contains(&"--jsonl") {
                assert_eq!(stdout.bytes().filter(|byte| *byte == b'\n').count(), 1);
                stdout.strip_suffix('\n').ok_or("JSONL terminator absent")?
            } else {
                assert!(!stdout.contains('\n'), "JSON is one unterminated document");
                stdout
            };
            assert_eq!(serde_json::from_str::<serde_json::Value>(json)?, expected);
        }
        assert_eq!(snapshot(root.path())?, before);
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), server).await???,
        ["GET /api/health"; 4]
    );
    Ok(())
}
