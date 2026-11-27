use super::*;
use grapt_cli::SessionSelector;
use grapt_connections::store::{Connections, Store};
use serde_json::{json, Value};

#[tokio::test]
async fn binary_logout_requires_a_signed_in_name_in_text_and_json() -> TestResult {
    for (present, named) in [(false, false), (true, false), (true, true)] {
        for structured in [false, true] {
            let root = Scratch::new("logout")?;
            let profile = root.path().join("profile");
            let mut peer = None;
            let mut metadata = Vec::new();
            if present {
                std::fs::create_dir(&profile)?;
                grapt_login::profile_security::protect_directory(&profile)?;
                let records = if named {
                    json!([{"id":"work-key","catalog_provider":"openai","family":"openai-responses",
                            "method":{"kind":"key"},"label":"Work","account":"fixture-account",
                            "route":"responses","credential_version":"key-v1","policy_version":"v1",
                            "credential_file":"work.credential","discovery_file":"work.json"}])
                } else {
                    json!([])
                };
                metadata = serde_json::to_vec(
                    &json!({"schema":"secode.connections.v1","connections":records}),
                )?;
                Store::open(&profile)?.save(&Connections::from_slice(&metadata)?)?;
                metadata = std::fs::read(profile.join("connections.json"))?;
                let pid = std::process::id();
                let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":"logout-test",
                        "environment":"test","service_instance":"instance-live"});
                let (endpoint, server) = spawn_server(vec![Script::json(
                    "GET",
                    "/api/health",
                    200,
                    &health.to_string(),
                )])
                .await?;
                write_discovery_fixture(&profile, pid, &endpoint, "logout-test")?;
                peer = Some(server);
            }
            let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
            command
                .args(["logout", "missing-key", "--profile"])
                .arg(&profile)
                .kill_on_drop(true);
            if structured {
                command.arg("--json");
            }
            let output = tokio::time::timeout(std::time::Duration::from_secs(10), command.output())
                .await??;
            assert_eq!(output.status.code(), Some(3), "{output:?}");
            let reason = if structured {
                assert!(output.stderr.is_empty());
                let value: Value = serde_json::from_slice(&output.stdout)?;
                assert_eq!(value["status"], "failed");
                assert_eq!(value["exit"], "not_found");
                value["reason"].as_str().ok_or("missing reason")?.to_owned()
            } else {
                assert!(output.stdout.is_empty());
                String::from_utf8(output.stderr)?
            };
            assert!(reason.contains("missing-key") && reason.contains("not signed in"));
            assert!(reason.contains(if named {
                "Signed-in connections: work-key"
            } else {
                "Signed-in connections: none"
            }));
            if let Some(peer) = peer {
                assert_eq!(peer.await??.len(), 1);
                assert_eq!(std::fs::read(profile.join("connections.json"))?, metadata);
            } else {
                assert!(!profile.exists());
            }
        }
    }
    Ok(())
}

#[tokio::test]
async fn public_runner_preserves_http_failure_classes() -> TestResult {
    for (status, code) in [(404, 3), (403, 4), (503, 12)] {
        let (endpoint, peer) =
            spawn_server(vec![Script::status("POST", "/api/session", status)]).await?;
        let mut output = BoundedOutput::new(
            Vec::new(),
            Vec::new(),
            OutputMode::Text,
            CliLimits::production(),
        );
        let result = Runner::new(client(&endpoint)?)
            .attach(SessionSelector::New, &mut output)
            .await;
        let error = result.err().ok_or("failed session creation succeeded")?;
        assert_eq!(error.exit_class().code(), code);
        peer.await??;
    }
    Ok(())
}
