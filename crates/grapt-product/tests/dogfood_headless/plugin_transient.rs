//! Transient plugin generation uses the Session's existing tool visibility policy.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn plugin_generation_withholds_denied_tools_and_keeps_attended_write() {
    let _journey = process_journey_custody();
    let root = root("plugin-transient-withhold");
    prepare(&root, 60_000);
    prepare_model_inventory(&root, 128_000);
    let project = root.join("project");
    fs::create_dir_all(&project).unwrap_or_else(|error| panic!("project: {error}"));
    let existing = project.join("existing.txt");
    fs::write(&existing, "original bytes")
        .unwrap_or_else(|error| panic!("overwrite fixture: {error}"));
    let plugin = root.join("generator-plugin").join("generator.mjs");
    fs::create_dir_all(plugin.parent().unwrap_or(&root))
        .unwrap_or_else(|error| panic!("plugin directory: {error}"));
    fs::write(
        &plugin,
        r#"export default {
  id: "fixture.transient",
  async setup(ctx) {
    await ctx.tool.transform((draft) => draft.add({
      name: "fixture_echo", description: "Generate transient text",
      args: {type:"object",properties:{text:{type:"string"}},required:["text"]},
      async execute(_input, context) {
        const text = await ctx.session.generate({
          sessionID: context.sessionID, prompt: "transient withholding witness"
        })
        return {title:"transient",output:text,metadata:{}}
      }
    }))
  }
}
"#,
    )
    .unwrap_or_else(|error| panic!("plugin source: {error}"));
    let config = root.join("explicit-plugin.jsonc");
    fs::write(
        &config,
        serde_json::json!({"plugins":[{"package":plugin}]}).to_string(),
    )
    .unwrap_or_else(|error| panic!("plugin config: {error}"));
    let environment = plugin_tool::plugin_environment(&root, &plugin_tool::node(), &config);
    let pairs = environment
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect::<Vec<_>>();
    let provider = ProviderFixture::start(vec![
        Script::ToolPlugin {
            call: "call_transient",
            text: "generate",
        },
        Script::SuccessText("transient answer"),
        // Author allows creation and asks before overwrite. The same offered
        // tool must keep its call-time decision after transient generation.
        Script::ToolInput {
            call: "call_create",
            tool: "write",
            arguments: serde_json::json!({"path":"created.txt","content":"created bytes"}),
        },
        Script::ToolInput {
            call: "call_overwrite",
            tool: "write",
            arguments: serde_json::json!({"path":"existing.txt","content":"replacement bytes"}),
        },
        Script::SuccessText("generation and denial settled"),
    ]);
    let mut child = spawn_with(&root, provider.address, &pairs);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let plugins =
        json_with_read_timeout(address, "GET", "/api/plugin", "", Duration::from_secs(30));
    assert!(
        plugins["data"].as_array().is_some_and(|rows| rows
            .iter()
            .any(|row| row["id"] == "fixture.transient" && row["status"] == "active")),
        "generation plugin is not active: {plugins}"
    );
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location":{"directory":project}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session absent"))
        .to_owned();
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        r#"{"preset":"author"}"#,
    );
    assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
    let path = format!("/api/session/{session}/prompt");
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &path,
            r#"{"text":"use the generation plugin","delivery":"queue","resume":true}"#,
        )
    });
    let permission = permission_boundaries::pending(address, &session, "call_transient");
    permission_boundaries::reply(address, &session, &permission, "once");
    let overwrite = permission_boundaries::pending(address, &session, "call_overwrite");
    permission_boundaries::reply(address, &session, &overwrite, "reject");
    let (status, body) = prompt
        .join()
        .unwrap_or_else(|_| panic!("prompt thread panicked"));
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    let denied = permission_boundaries::output(&provider, "call_overwrite");
    let created = fs::read(project.join("created.txt"))
        .unwrap_or_else(|error| panic!("allowed create: {error}"));
    let preserved =
        fs::read(&existing).unwrap_or_else(|error| panic!("rejected overwrite: {error}"));
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(bodies.len(), 5, "real provider requests: {bodies:?}");
    let generation: serde_json::Value = serde_json::from_str(&bodies[1])
        .unwrap_or_else(|error| panic!("transient provider request: {error}"));
    assert!(
        generation["input"]
            .to_string()
            .contains("transient withholding witness"),
        "the request must come from real plugin_generate"
    );
    let tools = generation["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("transient tools absent"));
    assert!(
        !tools.iter().any(|tool| tool["name"] == "shell"),
        "transient generation offered wholly denied shell"
    );
    assert!(
        tools.iter().any(|tool| tool["name"] == "write"),
        "transient generation withheld write requiring a call-time decision"
    );
    assert!(
        denied.contains("the permission request was rejected"),
        "{denied}"
    );
    assert_eq!(created, b"created bytes", "allowed write did not execute");
    assert_eq!(preserved, b"original bytes", "rejected write executed");
}
