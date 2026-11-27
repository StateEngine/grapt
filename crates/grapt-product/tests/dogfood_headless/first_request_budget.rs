//! Capture the ordinary planner; no paid provider or observed token claim.
#[cfg(unix)]
use super::process_fixture::spawn_controlled;
#[cfg(unix)]
use super::*;

#[test]
#[cfg(unix)]
fn first_request_defers_mcp_and_skill_metadata_without_dropping_project_inputs() {
    let _journey = process_journey_custody();
    let root = root("cache05-first-request");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 30_000);
    for index in 0..20 {
        let directory = root.join(format!(".agents/skills/fixture-{index}"));
        fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("skill fixture: {error}"));
        fs::write(directory.join("SKILL.md"), format!(
            "---\nname: Fixture {index}\ndescription: SC_CACHE05_METADATA_{index} {}\n---\nSC_CACHE05_BODY_{index}",
            "large optional metadata ".repeat(30)))
            .unwrap_or_else(|error| panic!("skill fixture: {error}"));
    }
    fs::write(
        root.join("AGENTS.md"),
        "SC_CACHE05_REQUIRED_PROJECT_CONTEXT",
    )
    .unwrap_or_else(|error| panic!("project instructions: {error}"));
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../scripts/stateengine-native/fixture_server.py");
    assert!(path.is_file(), "MCP fixture is absent");
    let config = root.join("grapt.jsonc");
    let mut value: serde_json::Value = serde_json::from_slice(
        &fs::read(&config).unwrap_or_else(|error| panic!("fixture config: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    value["mcp"] = serde_json::json!({"auxiliary":{"type":"local",
        "command":["python3",path]}});
    fs::write(config, value.to_string()).unwrap_or_else(|error| panic!("fixture config: {error}"));
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut service = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_first_request_budget", "text":"Reply READY.",
            "delivery":"queue", "resume":true,
            "files":[{"name":"fixture.txt","uri":"data:text/plain,SC_CACHE05_ATTACHMENT"}]})
        .to_string(),
    );
    permission_residuals::wait_session_idle(address, &session);
    service.stop_after_witness(&root);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 1);
    let body = &bodies[0];
    assert!(body.contains("SC_CACHE05_REQUIRED_PROJECT_CONTEXT"));
    assert!(body.contains("SC_CACHE05_ATTACHMENT"));
    assert!(body.contains("20 available skills"));
    assert!(!body.contains("SC_CACHE05_METADATA_"));
    assert!(!body.contains("SC_CACHE05_BODY_"));
    let request: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("wire capture: {error}"));
    let tools = request["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("wire tools missing"));
    assert!(
        tools.iter().any(|tool| tool["name"] == "execute"),
        "MCP must have connected and offered its discovery tool"
    );
    assert!(
        !tools.iter().any(|tool| tool["name"]
            .as_str()
            .is_some_and(|name| name.starts_with("auxiliary_"))),
        "full MCP schema leaked into initial tools"
    );
}

/// Shared with the cache-history journey: preserve all 120 full schemas.
#[cfg(unix)]
pub(super) fn install_large_catalog(root: &Path) {
    let script = root.join("large_catalog.py");
    fs::write(
        &script,
        r#"
import json, sys
for _ in range(16):
    line = sys.stdin.buffer.readline(65537)
    if not line: break
    if len(line) > 65536: raise ValueError('input bound')
    message = json.loads(line)
    if 'id' not in message: continue
    method = message['method']
    if method == 'initialize':
        result = {'protocolVersion':'2025-06-18','capabilities':{'tools':{}},
                  'serverInfo':{'name':'large-catalog','version':'1'}}
    elif method == 'tools/list':
        result = {'tools':[{'name':f'tool_{i:03}', 'description':'Supported schema',
            'inputSchema':{'type':'object','properties':{'value':{
            'type':'string','description':'x'*2300}}}} for i in range(120)]}
    elif method == 'tools/call':
        assert message['params']['name'] == 'tool_119'
        assert message['params']['arguments'] == {'value':'boundary-ok'}
        result = {'isError':False,'content':[{'type':'text','text':'LARGE_CATALOG_CALL_SUCCEEDED'}]}
    else: raise ValueError('unexpected request')
    reply = json.dumps({'jsonrpc':'2.0','id':message['id'],'result':result})
    if len(reply) > 1048576: raise ValueError('output bound')
    print(reply, flush=True)
"#,
    )
    .unwrap_or_else(|error| panic!("large MCP fixture: {error}"));
    let path = root.join("grapt.jsonc");
    let mut config: serde_json::Value = serde_json::from_slice(
        &fs::read(&path).unwrap_or_else(|error| panic!("fixture config: {error}")),
    )
    .unwrap_or_else(|error| panic!("fixture config: {error}"));
    config["mcp"] = serde_json::json!({"auxiliary":{"type":"local",
        "command":["python3",script]}});
    config["permission"] = serde_json::json!({"auxiliary_*":"ask"});
    fs::write(path, config.to_string()).unwrap_or_else(|error| panic!("fixture config: {error}"));
}

#[test]
#[cfg(unix)]
fn a_previously_supported_large_catalog_can_be_discovered_and_called_by_default() {
    let _journey = process_journey_custody();
    let root = root("cache05-large-catalog");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 30_000);
    install_large_catalog(&root);
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: "call_large_catalog",
            tool: "execute",
            arguments: serde_json::json!({"code":"return catalog.describe('auxiliary_tool_119');"}),
        },
        Script::ToolInput {
            call: "call_large_tool",
            tool: "execute",
            arguments: serde_json::json!({"code":"return await tools.auxiliary_tool_119({value:'boundary-ok'});"}),
        },
        Script::Success,
    ]);
    let mut service = spawn_controlled(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    json(address, "POST", &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_large_catalog", "text":"Inspect and invoke the final catalog tool.",
            "delivery":"queue", "resume":true}).to_string());
    let mut approved = false;
    let wait = PollDeadline::new();
    while wait.pending() {
        let page = json(
            address,
            "GET",
            &format!("/api/session/{session}/permission"),
            "",
        );
        if let Some(row) = page["data"].as_array().and_then(|rows| rows.first()) {
            assert_eq!(row["action"], "auxiliary_tool_119");
            permission_boundaries::reply(address, &session, row, "once");
            approved = true;
            break;
        }
        thread::sleep(Duration::from_millis(10));
    }
    assert!(
        approved,
        "large-catalog tool never reached ordinary approval ({wait})"
    );
    permission_residuals::wait_session_idle(address, &session);
    service.stop_after_witness(&root);
    let bodies = locked(&provider.bodies);
    assert_eq!(bodies.len(), 3);
    let initial: serde_json::Value =
        serde_json::from_str(&bodies[0]).unwrap_or_else(|error| panic!("wire capture: {error}"));
    let tools = initial["tools"]
        .as_array()
        .unwrap_or_else(|| panic!("tools absent"));
    assert!(tools.iter().any(|tool| tool["name"] == "execute"));
    assert!(!tools.iter().any(|tool| tool["name"]
        .as_str()
        .is_some_and(|name| name.starts_with("auxiliary_"))));
    assert!(!bodies[0].contains(&"x".repeat(2300)));
    assert!(
        bodies[1].contains(&"x".repeat(2300)),
        "full discovered schema missing"
    );
    assert!(bodies[2].contains("LARGE_CATALOG_CALL_SUCCEEDED"));
}
