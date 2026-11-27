use super::*;
use grapt_cli::{seat::ProvisionCommand, CliCommand};
use grapt_types::Secret;
use serde_json::json;

fn command() -> TestResult<ProvisionCommand> {
    let identity = json!({"scope":"project","project_id":"fixture","agent_id":"agt_seat","mail_transport_protocol":"remote-seat-mail-transport/v2"}).to_string();
    let packet = json!({"state":"written","path":"/packet.md","bytes":42,"sha256":"a".repeat(64)})
        .to_string();
    let arguments = [
        "session",
        "provision",
        "--service",
        "/profile",
        "--session",
        "ses_se123",
        "--agent",
        "build",
        "--model",
        "vendor/model",
        "--location",
        "/repo",
        "--identity-entry",
        &identity,
        "--packet-entry",
        &packet,
    ];
    let CliCommand::SessionProvision(command) = CliCommand::parse(arguments)? else {
        return Err("wrong command".into());
    };
    Ok(command)
}

fn session_response(id: &str) -> TestResult<String> {
    let response = json!({"data":{
        "id":id,"projectID":"prj_seat","title":"seat",
        "location":{"directory":"/repo"},"agent":"build",
        "model":{"providerID":"vendor","id":"model"},
        "cost":0,"tokens":{"input":0,"output":0,"reasoning":0,
            "cache":{"read":0,"write":0}},
        "time":{"created":1,"updated":1}
    }});
    // A malformed fixture must not masquerade as an identity-check refusal.
    let _: grapt_api::SessionResponseDto = serde_json::from_value(response.clone())?;
    Ok(response.to_string())
}

#[tokio::test]
async fn provisioning_binds_last_and_never_retries_failed_writes() -> TestResult {
    for fail_at in [None, Some(0), Some(1), Some(2), Some(3)] {
        let stages = [
            ("POST", "/api/session"),
            (
                "PUT",
                "/api/session/ses_se123/instructions/entries/stateengine-packet",
            ),
            (
                "PUT",
                "/api/session/ses_se123/instructions/entries/stateengine-identity",
            ),
            ("PUT", "/api/session/ses_se123/stateengine/capability"),
        ];
        let mut scripts = vec![];
        for (index, (method, path)) in stages.iter().enumerate() {
            if fail_at == Some(index) {
                scripts.push(Script::json(
                    *method,
                    *path,
                    500,
                    r#"{"message":"sec1_private"}"#,
                ));
            }
            // A retry at the failed route gets a healthy response. Closing the
            // listener there would hide a retry behind connection refusal.
            let script = if index == 0 {
                Script::json(*method, *path, 200, &session_response("ses_se123")?)
                    .expect_json(json!({"id":"ses_se123","agent":"build","model":{"providerID":"vendor","id":"model"},"location":{"directory":"/repo"}}))
            } else {
                Script::status(*method, *path, 204)
            };
            scripts.push(if index == 3 {
                script.expect_json(json!({"capability":"sec1_private"}))
            } else {
                script
            });
        }
        let (endpoint, server) = spawn_server(scripts).await?;
        let result = command()?
            .provision(&client(&endpoint)?, Secret::new("sec1_private".to_owned()))
            .await;
        assert_eq!(result.is_ok(), fail_at.is_none(), "{fail_at:?}: {result:?}");
        if fail_at == Some(3) {
            assert!(!result
                .err()
                .ok_or("error")?
                .to_string()
                .contains("sec1_private"));
        }
        if fail_at.is_some() {
            // The unused healthy retry and later routes stay available until
            // the client returns, then only this fixture server is cancelled.
            server.abort();
            assert!(server.await.err().is_some_and(|error| error.is_cancelled()));
        } else {
            let requests =
                tokio::time::timeout(std::time::Duration::from_secs(2), server).await???;
            assert_eq!(
                requests,
                stages
                    .iter()
                    .map(|(method, path)| format!("{method} {path}"))
                    .collect::<Vec<_>>()
            );
        }
    }
    Ok(())
}

#[tokio::test]
async fn provisioning_refuses_a_different_created_session_before_binding() -> TestResult {
    let scripts = vec![
        Script::json(
            "POST",
            "/api/session",
            200,
            &session_response("ses_foreign")?,
        ),
        Script::status(
            "PUT",
            "/api/session/ses_se123/instructions/entries/stateengine-packet",
            204,
        ),
        Script::status(
            "PUT",
            "/api/session/ses_se123/instructions/entries/stateengine-identity",
            204,
        ),
        Script::status("PUT", "/api/session/ses_se123/stateengine/capability", 204),
    ];
    let (endpoint, server) = spawn_server(scripts).await?;
    let result = command()?
        .provision(&client(&endpoint)?, Secret::new("sec1_private".to_owned()))
        .await;
    assert_eq!(
        result,
        Err("created session differs from the requested launch identity".into())
    );
    // All binding routes remain available, so omitting the identity check
    // produces success instead of an unrelated connection-refused error.
    server.abort();
    assert!(server.await.err().is_some_and(|error| error.is_cancelled()));
    Ok(())
}

#[tokio::test]
async fn binary_provision_selects_current_then_legacy_then_foreign_config() -> TestResult {
    for (current, legacy, expected) in [
        (true, true, "current"),
        (false, true, "legacy"),
        (false, false, "foreign"),
    ] {
        let root = Scratch::new("provision-config")?;
        std::fs::write(root.path().join("service.conf"), b"fixture")?;
        let paths: Vec<_> = ["current", "legacy", "foreign"]
            .iter()
            .map(|name| root.path().join(format!("{name}.jsonc")))
            .collect();
        for path in &paths {
            std::fs::write(path, b"{}")?;
        }
        let selected = root.path().join(format!("{expected}.jsonc"));
        let pid = std::process::id();
        let health = json!({"schema":"secode.api.health.v1","pid":pid,"build":"selector-test",
            "environment":"test","service_instance":"instance-live"});
        let (endpoint, peer) = spawn_server(vec![
            Script::json("GET", "/api/health", 200, &health.to_string()),
            Script::json("POST", "/api/session", 200, &session_response("ses_se123")?),
            Script::status(
                "PUT",
                "/api/session/ses_se123/instructions/entries/stateengine-packet",
                204,
            ),
            Script::status(
                "PUT",
                "/api/session/ses_se123/instructions/entries/stateengine-identity",
                204,
            ),
            Script::status("PUT", "/api/session/ses_se123/stateengine/capability", 204)
                .expect_json(json!({"capability":"synthetic-capability","configPath":selected})),
        ])
        .await?;
        write_discovery_fixture(root.path(), pid, &endpoint, "selector-test")?;
        let mut command = tokio::process::Command::new(env!("CARGO_BIN_EXE_grapt"));
        command.args(["session", "provision", "--service"]).arg(root.path())
            .args(["--session", "ses_se123", "--agent", "build", "--model", "vendor/model", "--location", "/repo",
                "--identity-entry", r#"{"scope":"project","project_id":"fixture","agent_id":"fixture-agent","mail_transport_protocol":"remote-seat-mail-transport/v2"}"#,
                "--packet-entry", r#"{"state":"written"}"#])
            .env_remove("GRAPT_CONFIG")
            .env_remove("SECODE_CONFIG") // grapt-rename: keep
            .env(concat!("OPEN", "CODE_CONFIG"), &paths[2])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).kill_on_drop(true);
        if current {
            command.env("GRAPT_CONFIG", &paths[0]);
        }
        if legacy {
            command.env("SECODE_CONFIG", &paths[1]); // grapt-rename: keep
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
        assert!(output.status.success(), "selector {expected}: {output:?}");
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(2), peer)
                .await???
                .len(),
            5
        );
    }
    Ok(())
}
