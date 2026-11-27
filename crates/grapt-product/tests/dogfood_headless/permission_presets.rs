//! A Session's permission preset decides which calls need a person: the
//! default `maintainer` runs commands unasked, `author` refuses them in its
//! own words, and a profile may choose its own default.
use super::*;

const CALL: &str = "call_preset_shell";

fn tool_output(body: &str) -> String {
    let request: serde_json::Value = serde_json::from_str(body)
        .unwrap_or_else(|error| panic!("provider request was not JSON: {error}"));
    request["input"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|item| item["type"] == "function_call_output" && item["call_id"] == CALL)
        })
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("provider request omitted the command's output"))
        .to_owned()
}

/// Run one command turn on a new Session, optionally at `preset`; the
/// output the model was shown, and the permission rows left pending.
fn command_turn(
    label: &str,
    preset: Option<&str>,
    profile_default: Option<&str>,
    global_default: Option<&str>,
    project_default: Option<&str>,
) -> (String, usize) {
    let _journey = process_journey_custody();
    let root = root(label);
    prepare(&root, 200_000);
    if let Some(name) = profile_default {
        let config = root.join("service.conf");
        let mut text = fs::read_to_string(&config).unwrap_or_else(|error| panic!("{error}"));
        text.push_str(&format!("permission_preset={name}\n"));
        fs::write(&config, text).unwrap_or_else(|error| panic!("{error}"));
    }
    for (directory, preset) in [
        (root.join("global-config"), global_default),
        (root.join("project"), project_default),
    ] {
        fs::create_dir_all(&directory).unwrap_or_else(|error| panic!("{error}"));
        if let Some(preset) = preset {
            fs::write(
                directory.join("grapt.json"),
                serde_json::json!({"permission_preset": preset}).to_string(),
            )
            .unwrap_or_else(|error| panic!("{error}"));
        }
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolInput {
            call: CALL,
            tool: "shell",
            arguments: serde_json::json!({"command":"echo preset-ok","timeout_ms":20_000}),
        },
        Script::SuccessText("the turn ended"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(
        address,
        "POST",
        "/api/session",
        &serde_json::json!({"location": {"directory": root.join("project")}}).to_string(),
    )["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("created Session had no id"))
        .to_owned();
    let selected_directory = json(address, "GET", &format!("/api/session/{session}"), "")["data"]
        ["location"]["directory"]
        .as_str()
        .unwrap_or_else(|| panic!("created Session omitted its directory"))
        .to_owned();
    let expected_directory = fs::canonicalize(root.join("project"))
        .unwrap_or_else(|error| panic!("fixture directory could not be resolved: {error}"));
    assert_eq!(
        fs::canonicalize(selected_directory)
            .unwrap_or_else(|error| panic!("Session directory could not be resolved: {error}")),
        expected_directory,
        "the Session did not use the configured project"
    );
    if let Some(name) = preset {
        let (status, body) = request(
            address,
            "POST",
            &format!("/api/session/{session}/preset"),
            &serde_json::json!({"preset": name}).to_string(),
        );
        assert_eq!(status, 204, "{}", String::from_utf8_lossy(&body));
        assert_eq!(
            json(address, "GET", &format!("/api/session/{session}"), "")["data"]["preset"],
            name
        );
    }
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"run it"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    let pending = json(
        address,
        "GET",
        &format!("/api/session/{session}/permission"),
        "",
    )["data"]
        .as_array()
        .map_or(0, Vec::len);
    let calls = provider.calls.load(Ordering::SeqCst);
    let bodies = locked(&provider.bodies).clone();
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(calls, 2, "the turn did not continue past the command");
    (tool_output(&bodies[1]), pending)
}

#[test]
fn the_default_runs_a_command_without_asking() {
    let (output, pending) = command_turn("preset-default", None, None, None, None);
    assert!(
        output.contains("preset-ok"),
        "the command did not run: {output}"
    );
    assert_eq!(pending, 0, "the default asked for a command");
}

#[test]
fn an_author_session_is_refused_a_command_in_its_own_words() {
    let (output, pending) = command_turn("preset-author", Some("author"), None, None, None);
    assert!(
        output.contains("shell is denied by this Session's author permission preset"),
        "{output}"
    );
    assert!(
        !output.contains("preset-ok"),
        "a denied command ran: {output}"
    );
    assert_eq!(pending, 0, "a denial was asked instead");
}

#[test]
fn the_profile_default_applies_to_a_session_without_its_own() {
    let (output, _) = command_turn("preset-profile", None, Some("author"), None, None);
    assert!(
        output.contains("denied by this Session's author permission preset"),
        "{output}"
    );
    // The Session's own choice outranks the profile's default.
    let (output, _) = command_turn(
        "preset-override",
        Some("maintainer"),
        Some("author"),
        None,
        None,
    );
    assert!(output.contains("preset-ok"), "{output}");
}

#[test]
fn a_project_cannot_raise_the_global_default() {
    let (output, pending) = command_turn(
        "preset-project-raise",
        None,
        None,
        Some("author"),
        Some("maintainer"),
    );
    assert!(
        output.contains("denied by this Session's author permission preset"),
        "a project's looser preset raised the global default: {output}"
    );
    assert!(
        !output.contains("preset-ok"),
        "a denied command ran: {output}"
    );
    assert_eq!(pending, 0, "a denial was asked instead");
}

#[test]
fn a_project_can_narrow_the_global_default() {
    let (output, pending) = command_turn(
        "preset-project-narrow",
        None,
        None,
        Some("maintainer"),
        Some("author"),
    );
    assert!(
        output.contains("denied by this Session's author permission preset"),
        "a project's stricter preset was ignored: {output}"
    );
    assert!(
        !output.contains("preset-ok"),
        "a denied command ran: {output}"
    );
    assert_eq!(pending, 0, "a denial was asked instead");
}

#[test]
fn the_profile_default_wins_over_global_and_looser_project_config() {
    let (output, pending) = command_turn(
        "preset-profile-global",
        None,
        Some("author"),
        Some("maintainer"),
        Some("maintainer"),
    );
    assert!(
        output.contains("denied by this Session's author permission preset"),
        "service.conf did not retain the user's narrower default: {output}"
    );
    assert!(
        !output.contains("preset-ok"),
        "a denied command ran: {output}"
    );
    assert_eq!(pending, 0, "a denial was asked instead");
}

#[test]
fn an_unknown_preset_is_refused_naming_the_available_ones() {
    let _journey = process_journey_custody();
    let root = root("preset-unknown");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(Vec::new());
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{session}/preset"),
        r#"{"preset":"auto"}"#,
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    assert_eq!(status, 400);
    assert!(
        String::from_utf8_lossy(&body)
            .contains("Unknown permission preset: auto. Available: inspect, author, maintainer"),
        "{}",
        String::from_utf8_lossy(&body)
    );
}

#[test]
fn approval_wait_rejects_a_failed_step_that_never_called_the_tool() {
    let _journey = process_journey_custody();
    let root = root("preset-approval-failed-step");
    prepare(&root, 200_000);
    let provider = ProviderFixture::start(vec![Script::Unauthorized]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"text":"run the expected command"}"#,
    );
    permission_residuals::wait_session_idle(address, &session);
    assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
    let result = std::panic::catch_unwind(|| {
        compaction_continuity::approve(address, &session, "call_never_started");
    });
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    let failure = result.err().unwrap_or_else(|| {
        panic!("approval wait accepted a failed Step without the named tool call")
    });
    let diagnostic = failure
        .downcast_ref::<String>()
        .map(String::as_str)
        .or_else(|| failure.downcast_ref::<&str>().copied())
        .unwrap_or_else(|| panic!("approval wait failed without a text diagnostic"));
    assert_eq!(
        diagnostic,
        "the Session went idle without running call_never_started"
    );
}
