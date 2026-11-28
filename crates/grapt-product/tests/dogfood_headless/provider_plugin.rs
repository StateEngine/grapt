//! Declared and withheld plugin generations at the ordinary provider boundary.
use super::*;

fn write_echo_plugin(path: &Path) {
    fs::write(
        path,
        r#"export default {
  id: "cpk.fixture.echo",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "cpk_fixture_echo",
      description: "Fixture echo",
      args: {type:"object",additionalProperties:false,properties:{text:{type:"string"}},required:["text"]},
      async execute(input) {
        return {title:"cpk witness",output:`cpk:${input.text}`,metadata:{}}
      }
    }))
  }
}
"#,
    )
    .unwrap_or_else(|error| panic!("plugin fixture write failed: {error}"));
}

fn declaration(root: &Path) -> PathBuf {
    let plugin = root.join("echo-plugin").join("echo.mjs");
    fs::create_dir_all(plugin.parent().unwrap_or(root))
        .unwrap_or_else(|error| panic!("plugin directory: {error}"));
    write_echo_plugin(&plugin);
    let config = root.join("declared-plugin.jsonc");
    fs::write(
        &config,
        serde_json::json!({"plugins":[{"package":plugin}]}).to_string(),
    )
    .unwrap_or_else(|error| panic!("plugin configuration: {error}"));
    config
}

#[test]
fn provider_calls_a_declared_plugin_tool() -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("provider-plugin");
    prepare(&root, 120_000);
    fs::create_dir_all(root.join("global-config"))?;
    fs::write(
        root.join("global-config/grapt.json"),
        r#"{"permission":{"cpk_fixture_echo":"allow"}}"#,
    )?;
    let config = declaration(&root);
    let environment = plugin_tool::plugin_environment(&root, &plugin_tool::node(), &config);
    let pairs = environment
        .iter()
        .map(|(key, value)| (key.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_echo",
            tool: "cpk_fixture_echo",
            arguments: serde_json::json!({"text":"fixture"}),
        },
        Script::Success,
    ]);
    let mut child = spawn_with(&root, provider.address, &pairs);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("session absent")?
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Call the fixture echo tool."}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message?order=asc"),
        "",
    );
    assert!(
        history["data"]
            .as_array()
            .is_some_and(|messages| messages.iter().any(|message| message["content"]
                .as_array()
                .is_some_and(|parts| parts.iter().any(|part| part["type"] == "tool"
                    && part["name"] == "cpk_fixture_echo"
                    && part["state"]["status"] == "completed")))),
        "plugin call did not complete: {history}"
    );
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
    Ok(())
}

#[test]
fn provider_completes_with_a_withheld_plugin_generation() {
    let _journey = process_journey_custody();
    let root = root("provider-plugin-withheld");
    prepare(&root, 120_000);
    let config = declaration(&root);
    let pairs = [(
        "GRAPT_CONFIG",
        config
            .to_str()
            .unwrap_or_else(|| panic!("configuration path is not UTF-8")),
    )];
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut child = spawn_with(&root, provider.address, &pairs);
    let address = await_ready(&root, &mut child);
    let (status, plugins) = request(address, "GET", "/api/plugin", "");
    assert_eq!(status, 400, "an unconfigured plugin runtime was admitted");
    assert!(
        String::from_utf8_lossy(&plugins).contains("packaged plugin-runtime/runtime.json"),
        "unavailable runtime refusal omitted its cause"
    );
    assert!(!root.join("plugin-runtime").exists());
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session absent"))
        .to_owned();
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"Hello"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    child.stop_after_witness(&root);
    process_fixture::cleanup(&root);
}
