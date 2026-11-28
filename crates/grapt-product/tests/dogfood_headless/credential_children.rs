//! J01 S1b: Grapt never injects a secret it holds into
//! a child it spawns. With a FILE-stored distinctive key, a shell tool's full
//! environment dump reaches the model without the key under any name.
use super::*;

const FILE_KEY: &str = "sk-J01-S1b-FILE-STORED-KEY-4e1d9a";

#[test]
fn a_file_stored_key_never_reaches_a_shell_tools_environment(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("credential-children");
    prepare(&root, 120_000);
    unattended(&root);
    fs::write(root.join("provider.credential"), FILE_KEY)?;
    let provider = ProviderFixture::start(vec![
        Script::ToolShell {
            call: "call_shell_env",
            command: "env",
        },
        Script::Success,
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_credential_children","text":"show the environment","delivery":"queue","resume":true}"#,
    );
    compaction_continuity::approve(address, &session, "call_shell_env");
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        2,
        "the shell result must continue to the model"
    );
    // The tool's output is in the second request. It must be the real dump
    // (PATH is always there), and it must not carry the key.
    assert!(
        bodies[1].contains("PATH="),
        "the environment dump did not reach the model"
    );
    assert!(
        !bodies[1].contains(FILE_KEY),
        "a spawned shell tool's environment carried the file-stored key"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}

const API_KEY: &str = "sk-ant-J01-T1-PRIMARY-API-KEY-62bf";

/// J01 T1: the profile an Anthropic API-key first run creates (primary
/// `anthropic-messages`, `models.dev-anthropic`, a `/v1/models` listing, the
/// key in `provider.credential`) runs a turn: the first request reaches the
/// loopback provider as Anthropic Messages, authenticated with that key.
#[test]
fn an_anthropic_api_key_profile_runs_a_turn_with_its_key() -> Result<(), Box<dyn std::error::Error>>
{
    let _journey = process_journey_custody();
    let root = root("api-key-primary-turn");
    prepare(&root, 120_000);
    fs::write(
        root.join("grapt.jsonc"),
        r#"{
          "store": { "authority": "open-ready" },
          "product": { "build": "live-headless1-build" },
          "catalog": { "source": "models.dev-anthropic" },
          "provider": {
            "id": "anthropic", "protocol_family": "anthropic-messages",
            "model": "claude-live", "base_model": "claude-live", "family": "claude",
            "route": "messages", "account": "api-key",
            "credential_version": "api-key-v1", "policy_version": "grapt-setup-v1",
            "variants": []
          },
          "compaction": {}
        }"#,
    )?;
    fs::write(
        root.join("model-catalog.json"),
        br#"{"anthropic":{"id":"anthropic","name":"Anthropic","npm":"@ai-sdk/anthropic","models":{"claude-live":{"id":"claude-live","name":"claude-live","family":"claude","tool_call":true,"modalities":{"input":["text"],"output":["text"]},"limit":{"context":200000,"output":32000}}}}}"#,
    )?;
    fs::write(
        root.join("account-models.json"),
        br#"{"data":[{"type":"model","id":"claude-live"},{"type":"model","id":"claude-undescribed"}],"has_more":false}"#,
    )?;
    fs::write(root.join("provider.credential"), API_KEY)?;
    let provider = ProviderFixture::start(vec![context_quality::anthropic_text(
        "done",
        serde_json::json!({"input_tokens": 5, "output_tokens": 1}),
    )]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_api_key_turn","text":"hello","delivery":"queue","resume":true}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let heads = locked(&provider.heads).clone();
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 1, "one turn, one request");
    let head = heads[0].to_ascii_lowercase();
    assert!(
        head.contains("/messages"),
        "not an Anthropic Messages request"
    );
    assert!(
        head.contains("anthropic-version:"),
        "no anthropic-version header"
    );
    assert!(
        heads[0].contains(&format!("x-api-key: {API_KEY}")),
        "the request did not carry the profile's key as x-api-key"
    );
    let request: serde_json::Value = serde_json::from_str(&bodies[0])?;
    assert_eq!(request["model"], "claude-live");
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
