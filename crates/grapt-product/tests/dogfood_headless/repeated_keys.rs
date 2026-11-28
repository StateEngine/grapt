//! A builtin call that repeats a JSON key is refused before a plugin's
//! `execute.before` callback reads it as a value and keeps one repeat
//! (here `b.txt`, which a rewritten copy would read).
use super::*;

const ARGUMENTS: &str = r#"{"path":"a.txt","path":"b.txt","PRIVATE_KEY_51734":1}"#;
const PLUGIN: &str = r#"export default {
  id: "fixture.pass_through",
  async setup(ctx) {
    await ctx.tool.hook("execute.before", (event) => { event.input = { ...event.input } })
  }
}
"#;

#[test]
fn a_repeated_key_is_refused_before_a_pass_through_plugin_callback() {
    let _journey = process_journey_custody();
    let root = root("repeated-key-plugin");
    prepare(&root, 60_000);
    prepare_model_inventory(&root, 128_000);
    let (project, plugin) = (root.join("project"), root.join("plugin").join("pass.mjs"));
    let config = root.join("explicit-plugin.jsonc");
    fs::create_dir_all(&project)
        .and_then(|()| fs::write(project.join("b.txt"), "SECOND_BYTES"))
        .and_then(|()| fs::create_dir_all(root.join("plugin")))
        .and_then(|()| fs::write(&plugin, PLUGIN))
        .and_then(|()| {
            fs::write(
                &config,
                serde_json::json!({"plugins":[{"package":plugin}]}).to_string(),
            )
        })
        .unwrap_or_else(|error| panic!("fixture: {error}"));
    let environment = plugin_tool::plugin_environment(&root, &plugin_tool::node(), &config);
    let pairs: Vec<_> = environment
        .iter()
        .map(|(n, v)| (n.as_str(), v.as_str()))
        .collect();
    let call = "call_repeated";
    let delta = serde_json::to_string(ARGUMENTS).unwrap_or_else(|error| panic!("{error}"));
    let provider = ProviderFixture::start(vec![
        Script::Raw(format!(
            "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"read\"}}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{delta}}}\n\n\
             data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
             data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n"
        )),
        Script::SuccessText("finished"),
    ]);
    let mut child = spawn_with(&root, provider.address, &pairs);
    let address = await_ready(&root, &mut child);
    let active = json_with_read_timeout(address, "GET", "/api/plugin", "", Duration::from_secs(30));
    assert!(
        active["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == "fixture.pass_through" && row["status"] == "active")),
        "the pass-through plugin is not active: {active}"
    );
    let location = serde_json::json!({"location":{"directory":project}}).to_string();
    let session = json(address, "POST", "/api/session", &location)["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session absent"))
        .to_owned();
    let prompt = format!("/api/session/{session}/prompt");
    json(address, "POST", &prompt, r#"{"text":"read the fixture"}"#);
    permission_residuals::wait_session_idle(address, &session);
    let body: serde_json::Value = locked(&provider.bodies)
        .get(1)
        .and_then(|body| serde_json::from_str(body).ok())
        .unwrap_or_else(|| panic!("the tool outcome did not reach the model"));
    let output = body["input"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["call_id"] == call && item["type"] == "function_call_output")
        })
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("the model did not receive the tool outcome"));
    assert!(output.contains("a parameter is repeated"), "{output}");
    assert!(
        !output.contains("SECOND_BYTES") && !output.contains("PRIVATE_KEY"),
        "{output}"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
