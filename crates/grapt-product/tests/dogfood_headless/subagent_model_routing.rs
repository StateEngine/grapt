//! SKILLS-DELEGATION-01 J03: the child runs on the routed model.
//!
//! Amendment §7.5: the explicit Subagent model selection is the only authority
//! that may route a child to a different target; without one the child runs on
//! the parent's own target; an unusable explicit selection refuses rather than
//! running another model. Each witness reads the model off the provider
//! requests the loopback actually received, so stored selector state alone
//! cannot pass it.

use super::subagent_child::approve_the_subagent_ask;
use super::*;

pub(super) const CHILD_MODEL: &str = "gpt-child";

/// Offer a second model of the same provider beside the configured one.
pub(super) fn offer_child_model(root: &Path, offered: bool) {
    let mut models = serde_json::json!({
        "gpt-live": {"id":"gpt-live","name":"gpt-live","family":"gpt-5","tool_call":true,
            "modalities":{"input":["text"],"output":["text"]},"limit":{"output":32000}},
    });
    let mut inventory = vec![serde_json::json!({
        "slug":"gpt-live","visibility":"list","context_window":128_000,"supported_reasoning_levels":[]
    })];
    if offered {
        models[CHILD_MODEL] = serde_json::json!({"id":CHILD_MODEL,"name":CHILD_MODEL,"family":"gpt-5",
            "tool_call":true,"modalities":{"input":["text"],"output":["text"]},"limit":{"output":32000}});
        inventory.push(serde_json::json!({
            "slug":CHILD_MODEL,"visibility":"list","context_window":128_000,"supported_reasoning_levels":[]
        }));
    }
    let catalog = serde_json::json!({"openai":{"id":"openai","name":"OpenAI","npm":"@ai-sdk/openai","models":models}});
    fs::write(
        root.join("model-catalog.json"),
        serde_json::to_vec(&catalog).unwrap_or_else(|error| panic!("catalog encode: {error}")),
    )
    .unwrap_or_else(|error| panic!("model catalog fixture failed: {error}"));
    fs::write(
        root.join("account-models.json"),
        serde_json::to_vec(&serde_json::json!({"models":inventory}))
            .unwrap_or_else(|error| panic!("inventory encode: {error}")),
    )
    .unwrap_or_else(|error| panic!("model inventory fixture failed: {error}"));
}

/// The `model` each provider request named, in arrival order.
fn requested_models(provider: &ProviderFixture) -> Vec<String> {
    locked(&provider.bodies)
        .iter()
        .map(|body| {
            serde_json::from_str::<serde_json::Value>(body)
                .ok()
                .and_then(|request| request["model"].as_str().map(str::to_owned))
                .unwrap_or_else(|| panic!("a provider request named no model: {body}"))
        })
        .collect()
}

fn new_session(address: SocketAddr) -> String {
    let session = json(address, "POST", "/api/session", "{}");
    session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned()
}

/// Prompt the parent, approve its subagent ask, and wait for it to settle.
fn delegate(address: SocketAddr, session: &str, message: &str) {
    let prompt_path = format!("/api/session/{session}/prompt");
    let body =
        format!(r#"{{"id":"{message}","text":"delegate this","delivery":"queue","resume":true}}"#);
    let prompt = thread::spawn(move || request(address, "POST", &prompt_path, &body));
    approve_the_subagent_ask(address, session);
    let (status, response) = prompt
        .join()
        .unwrap_or_else(|_| panic!("the delegating prompt thread panicked"));
    assert_eq!(
        status,
        200,
        "the delegating prompt failed: {}",
        String::from_utf8_lossy(&response)
    );
    permission_residuals::wait_session_idle(address, session);
}

fn children_of(address: SocketAddr, parent: &str) -> Vec<String> {
    json(address, "GET", "/api/session", "")["data"]
        .as_array()
        .unwrap_or_else(|| panic!("session list was not an array"))
        .iter()
        .filter(|row| row["parentID"].as_str() == Some(parent))
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}

fn delegating_scripts(call: &'static str) -> Vec<Script> {
    vec![
        Script::ToolSubagent {
            call,
            task: "inspect the repository",
        },
        Script::Success,
        Script::Success,
    ]
}

#[test]
fn skd_child_runs_on_the_selected_subagent_model() {
    let _journey = process_journey_custody();
    let root = root("skd-subagent-selected");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    offer_child_model(&root, true);
    let provider = ProviderFixture::start(delegating_scripts("call_skd_selected"));
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = new_session(address);
    let (status, body) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/subagent-model"),
        &format!(r#"{{"model":{{"providerID":"openai","id":"{CHILD_MODEL}"}}}}"#),
    );
    assert_eq!(
        status,
        204,
        "subagent model select: {}",
        String::from_utf8_lossy(&body)
    );

    delegate(address, &session, "msg_skd_selected");

    assert_eq!(
        requested_models(&provider),
        vec!["gpt-live", CHILD_MODEL, "gpt-live"],
        "the parent ran on its own model and the child on the selected one"
    );
    let children = children_of(address, &session);
    assert_eq!(children.len(), 1, "exactly one child ran");
    child.stop_after_witness(&root);
}

#[test]
fn skd_child_inherits_the_parent_model_without_a_subagent_selection() {
    let _journey = process_journey_custody();
    let root = root("skd-subagent-inherit");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    offer_child_model(&root, true);
    let provider = ProviderFixture::start(delegating_scripts("call_skd_inherit"));
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let session = new_session(address);
    // The parent runs on a model other than the configured default, so a child
    // that fell back to the default would be visible on the wire.
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/model"),
        &format!(r#"{{"model":{{"providerID":"openai","id":"{CHILD_MODEL}"}}}}"#),
    );
    assert!(
        (200..300).contains(&status),
        "parent model select: {}",
        String::from_utf8_lossy(&body)
    );

    delegate(address, &session, "msg_skd_inherit");

    assert_eq!(
        requested_models(&provider),
        vec![CHILD_MODEL, CHILD_MODEL, CHILD_MODEL],
        "agent default runs the child on the parent's own target"
    );
    child.stop_after_witness(&root);
}

#[test]
fn skd_an_unavailable_subagent_model_refuses_without_running_a_child() {
    let _journey = process_journey_custody();
    let root = root("skd-subagent-unavailable");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    offer_child_model(&root, true);
    let first = ProviderFixture::start(Vec::new());
    let mut service = spawn(&root, first.address);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = new_session(address);
    let (status, body) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/subagent-model"),
        &format!(r#"{{"model":{{"providerID":"openai","id":"{CHILD_MODEL}"}}}}"#),
    );
    assert_eq!(
        status,
        204,
        "subagent model select: {}",
        String::from_utf8_lossy(&body)
    );
    service.stop_after_witness(&root);

    // The selection survives the restart; the model it names no longer does.
    offer_child_model(&root, false);
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_skd_unavailable",
            task: "inspect the repository",
        },
        Script::Success,
        // Unused when routing refuses (two requests). A child that ran on a
        // substituted model would consume it and settle, so the counterexample
        // fails on the request census below rather than stalling on an
        // exhausted fixture.
        Script::Success,
    ]);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    delegate(address, &session, "msg_skd_unavailable");

    assert_eq!(
        requested_models(&provider),
        vec!["gpt-live", "gpt-live"],
        "no request ran on a substituted model, and no child request ran at all"
    );
    assert!(
        children_of(address, &session).is_empty(),
        "a refused routing must not leave a child behind"
    );
    // The public history records the failed call; the refusal's detail is
    // folded into the continuation the parent sends onward, so the provider
    // body is where the parent model is told why (as for a failed child).
    let history = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    let call = history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .find(|part| part["id"].as_str() == Some("call_skd_unavailable"))
        .unwrap_or_else(|| panic!("the parent records no subagent call: {history}"));
    assert_eq!(
        call["state"]["status"], "error",
        "the refusal failed the call"
    );
    let continued = locked(&provider.bodies)
        .last()
        .cloned()
        .unwrap_or_else(|| panic!("no continuation request was sent"));
    assert!(
        continued.contains("cannot run this child"),
        "the parent model was told why delegation refused: {continued}"
    );
    service.stop_after_witness(&root);
}
