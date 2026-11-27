use super::*;
use serde_json::json;

#[tokio::test]
async fn binary_terminal_and_provision_keep_http_exit_classes() -> TestResult {
    for (status, code) in [(404, 3), (403, 4), (503, 12), (0, 12)] {
        for provision in [false, true] {
            let root = Scratch::new("command-failure")?;
            std::fs::write(root.path().join("service.conf"), b"fixture")?;
            let pid = std::process::id();
            let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":"failure-test",
                "environment":"test","service_instance":"instance-live"});
            let (endpoint, peer) = spawn_server(vec![
                Script::json("GET", "/api/health", 200, &health.to_string()),
                Script::status("POST", "/api/session", status),
            ])
            .await?;
            write_discovery_fixture(root.path(), pid, &endpoint, "failure-test")?;
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
            command
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            if provision {
                command.args(["session", "provision", "--service"]).arg(root.path())
                    .args(["--session", "ses_launch", "--agent", "build", "--model", "vendor/model", "--location"])
                    .arg(root.path()).args(["--identity-entry",
                        r#"{"scope":"project","project_id":"fixture","agent_id":"fixture-agent","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
                        "--packet-entry", r#"{"state":"written"}"#]);
            } else {
                command
                    .args(["code", "--profile"])
                    .arg(root.path())
                    .args(["--model", "vendor/model"])
                    .arg(root.path());
            }
            let mut child = command.spawn()?;
            child
                .stdin
                .take()
                .ok_or("stdin absent")?
                .write_all(b"synthetic-capability\n")
                .await?;
            let output =
                tokio::time::timeout(std::time::Duration::from_secs(10), child.wait_with_output())
                    .await??;
            assert_eq!(output.status.code(), Some(code), "{provision}: {output:?}");
            assert_eq!(peer.await??.len(), 2);
        }
    }
    Ok(())
}
