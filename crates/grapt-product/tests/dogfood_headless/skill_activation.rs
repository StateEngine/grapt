//! SKILLS-DELEGATION-01 J02: installed local skills through the ordinary path.
//!
//! Metadata is discovered on demand; full content reaches the conversation
//! when the model loads it with the `skill` tool. Both claims are
//! read off the provider requests the loopback received.

use super::subagent_child::approve_the_subagent_ask;
use super::*;

pub(super) const BODY_MARKER: &str = "SKD-SKILL-BODY-7f3a";

fn write(path: &Path, text: &str) {
    let parent = path
        .parent()
        .unwrap_or_else(|| panic!("fixture path has no parent"));
    fs::create_dir_all(parent).unwrap_or_else(|error| panic!("fixture directory: {error}"));
    fs::write(path, text).unwrap_or_else(|error| panic!("fixture file: {error}"));
}

/// Project-local skills under the fixture root, with an isolated home so the
/// runner's own skills cannot enter the request.
pub(super) fn install_skills(root: &Path) -> String {
    let skills = root.join(".agents/skills");
    write(
        &skills.join("release-notes/SKILL.md"),
        &format!("---\nname: Release notes\ndescription: Draft release notes from the change log.\n---\n{BODY_MARKER}\nFollow template.md.\n"),
    );
    write(&skills.join("release-notes/template.md"), "## Changes\n");
    write(
        &skills.join("manual-only/SKILL.md"),
        "---\ndescription: Only by explicit selection.\ndisable-model-invocation: true\n---\nmanual body\n",
    );
    write(
        &skills.join("undescribed/SKILL.md"),
        "---\nname: Undescribed\n---\nno description\n",
    );
    let home = root.join("home");
    fs::create_dir_all(&home).unwrap_or_else(|error| panic!("fixture home: {error}"));
    home.to_str()
        .unwrap_or_else(|| panic!("fixture home is not UTF-8"))
        .to_owned()
}

pub(super) fn skill_call(call: &str, id: &str) -> String {
    tool_call(call, "skill", serde_json::json!({ "id": id }))
}

/// One provider turn that calls `tool` with `arguments`, for the tools the
/// named scripts do not speak.
pub(super) fn tool_call(call: &str, tool: &str, arguments: serde_json::Value) -> String {
    let arguments = arguments.to_string();
    format!(
        "data: {{\"type\":\"response.output_item.added\",\"item\":{{\"type\":\"function_call\",\"id\":\"item_{call}\",\"call_id\":\"{call}\",\"name\":\"{tool}\"}}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.delta\",\"item_id\":\"item_{call}\",\"delta\":{}}}\n\n\
         data: {{\"type\":\"response.function_call_arguments.done\",\"item_id\":\"item_{call}\"}}\n\n\
         data: {{\"type\":\"response.completed\",\"response\":{{\"usage\":{{\"input_tokens\":1,\"output_tokens\":1,\"total_tokens\":2}}}}}}\n\n",
        serde_json::to_string(&arguments)
            .unwrap_or_else(|error| panic!("tool arguments encoding failed: {error}"))
    )
}

fn prompt_and_approve(address: SocketAddr, message: &str) -> (String, serde_json::Value) {
    let session = json(address, "POST", "/api/session", "{}");
    let session = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned();
    let prompt_path = format!("/api/session/{session}/prompt");
    let body = format!(
        r#"{{"id":"{message}","text":"write the notes","delivery":"queue","resume":true}}"#
    );
    let prompt = thread::spawn(move || request(address, "POST", &prompt_path, &body));
    let permission = approve_the_subagent_ask(address, &session);
    let (status, response) = prompt
        .join()
        .unwrap_or_else(|_| panic!("the prompt thread panicked"));
    assert_eq!(
        status,
        200,
        "the prompt failed: {}",
        String::from_utf8_lossy(&response)
    );
    permission_residuals::wait_session_idle(address, &session);
    (session, permission)
}

#[test]
fn skd_skills_are_discoverable_and_loaded_on_activation() {
    let _journey = process_journey_custody();
    let root = root("skd-skill-activation");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let home = install_skills(&root);
    let provider = ProviderFixture::start(vec![
        Script::Raw(skill_call("call_skd_skill", "release-notes")),
        Script::Success,
    ]);
    let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");

    let (_session, permission) = prompt_and_approve(address, "msg_skd_skill");
    assert_eq!(
        permission["action"], "skill",
        "activation is a permissioned act"
    );

    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 2, "one activation turn, then the answer");
    let offered = &bodies[0];
    assert!(offered.contains("1 available skills"));
    assert!(!offered.contains("Draft release notes from the change log."));
    assert!(
        !offered.contains(BODY_MARKER),
        "skill content entered the request before it was activated"
    );
    assert!(
        !offered.contains("<id>manual-only</id>"),
        "a skill that opted out of model invocation was offered"
    );
    assert!(
        !offered.contains("<id>undescribed</id>"),
        "a skill without a description was offered"
    );
    let activated = &bodies[1];
    assert!(
        activated.contains(BODY_MARKER),
        "the loaded skill's content reached the next request"
    );
    assert!(
        activated.contains("template.md"),
        "the skill's sibling assets are listed with it"
    );
    service.stop_after_witness(&root);
}

#[test]
fn skd_manual_only_skill_refuses_model_activation() {
    let _journey = process_journey_custody();
    let root = root("skd-user-only-skill");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let home = install_skills(&root);
    let manual = root.join(".agents/skills/manual-only");
    fs::hard_link(manual.join("SKILL.md"), manual.join("alias.md"))
        .unwrap_or_else(|error| panic!("main alias: {error}"));
    write(&manual.join("template.txt"), "MANUAL-SIBLING-RESOURCE");
    let provider = ProviderFixture::start(vec![
        Script::Raw(skill_call("call_manual_skill", "manual-only")),
        Script::Raw(tool_call(
            "call_manual_file",
            "skill",
            serde_json::json!({
                "id":"manual-only","file":"SKILL.md"
            }),
        )),
        Script::Raw(tool_call(
            "call_manual_alias",
            "skill",
            serde_json::json!({"id":"manual-only","file":"alias.md"}),
        )),
        Script::Raw(tool_call(
            "call_manual_sibling",
            "skill",
            serde_json::json!({"id":"manual-only","file":"template.txt"}),
        )),
        Script::Success,
    ]);
    let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    let path = format!("/api/session/{session}/prompt");
    let prompt = thread::spawn(move || {
        request(
            address,
            "POST",
            &path,
            r#"{"id":"msg_manual_skill","text":"load the skill","delivery":"queue","resume":true}"#,
        )
    });
    for _ in 0..4 {
        approve_the_subagent_ask(address, &session);
    }
    assert_eq!(
        prompt.join().unwrap_or_else(|_| panic!("prompt thread")).0,
        200
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies);
    let answer = bodies.last().unwrap_or_else(|| panic!("answer absent"));
    assert!(answer.contains("requires user invocation through its slash command"));
    assert!(!answer.contains("manual body"));
    assert!(answer.contains("MANUAL-SIBLING-RESOURCE"));
    drop(bodies);
    service.stop_after_witness(&root);
}

#[test]
fn skd_an_unknown_skill_is_refused_and_nothing_is_loaded() {
    let _journey = process_journey_custody();
    for interactive in [false, true] {
        let root = root("skd-skill-unknown");
        let _cleanup = terminal_coordination::RootCleanup(root.clone());
        prepare(&root, 8_000);
        if interactive {
            attended(&root);
        } else {
            unattended(&root);
        }
        let home = install_skills(&root);
        let provider = ProviderFixture::start(vec![
            Script::Raw(skill_call("call_skd_unknown", "not-installed")),
            Script::Success,
        ]);
        let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
        let address = await_ready(&root, &mut service);
        let _approver = interactive.then(|| approval::Attachment::open(address, "*"));

        let session = if interactive {
            prompt_and_approve(address, "msg_skd_unknown").0
        } else {
            let session = create(address);
            json(
                address,
                "POST",
                &format!("/api/session/{session}/prompt"),
                r#"{"id":"msg_skd_unknown","text":"load the missing skill","delivery":"queue","resume":true}"#,
            );
            permission_residuals::wait_session_idle(address, &session);
            session
        };

        let bodies = locked(&provider.bodies).clone();
        assert_eq!(bodies.len(), 2);
        assert!(
            bodies[1].contains("Skill not found in the installed catalog"),
            "the model was told the skill could not be loaded"
        );
        assert!(bodies[1].contains("release-notes"));
        assert!(bodies[1].contains("repository helper script is not automatically a skill"));
        let history = json(
            address,
            "GET",
            &format!("/api/session/{session}/message"),
            "",
        );
        let history = history.to_string();
        assert!(history.contains("Skill not found in the installed catalog"));
        assert!(history.contains("Nearest available skills: release-notes"));
        assert!(!history.contains(BODY_MARKER));
        assert!(!bodies[1].contains(BODY_MARKER));
        service.stop_after_witness(&root);
    }
}

#[test]
fn skill_discovery_metadata_then_full_content_reach_the_conversation_tail() {
    let _journey = process_journey_custody();
    let root = root("cache05-skill-discovery");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 30_000);
    unattended(&root);
    let home = install_skills(&root);
    let provider = ProviderFixture::start(vec![
        Script::Raw(tool_call(
            "call_skill_query",
            "skill",
            serde_json::json!({"query":"release notes"}),
        )),
        Script::Raw(skill_call("call_skill_load", "release-notes")),
        Script::Success,
    ]);
    let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &serde_json::json!({"id":"msg_skill_discovery", "text":"write the notes",
            "delivery":"queue", "resume":true})
        .to_string(),
    );
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 3);
    assert!(!bodies[0].contains("Draft release notes from the change log."));
    assert!(bodies[1].contains("Draft release notes from the change log."));
    assert!(!bodies[1].contains(BODY_MARKER));
    assert!(bodies[2].contains(BODY_MARKER));
    assert!(!bodies[1].contains("Only by explicit selection."));
    for pair in bodies.windows(2) {
        let before: serde_json::Value = serde_json::from_str(&pair[0])
            .unwrap_or_else(|error| panic!("captured request: {error}"));
        let after: serde_json::Value = serde_json::from_str(&pair[1])
            .unwrap_or_else(|error| panic!("captured request: {error}"));
        assert_eq!(before["tools"], after["tools"]);
        let prefix = before["input"]
            .as_array()
            .unwrap_or_else(|| panic!("request input missing"));
        assert_eq!(
            prefix.as_slice(),
            &after["input"]
                .as_array()
                .unwrap_or_else(|| panic!("request input missing"))[..prefix.len()]
        );
    }
    service.stop_after_witness(&root);
}

#[test]
fn skd_a_malformed_skill_catalog_is_reported_not_offered_as_empty() {
    let _journey = process_journey_custody();
    let root = root("skd-skill-malformed");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    attended(&root);
    let home = install_skills(&root);
    write(
        &root.join(".agents/skills/broken/SKILL.md"),
        "---\ndescription: [unterminated\n---\nbody\n",
    );
    let provider = ProviderFixture::start(vec![Script::Success]);
    let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", &home)]);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = json(address, "POST", "/api/session", "{}");
    let session = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("session response carried no id"))
        .to_owned();
    let (status, _) = request(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_skd_malformed","text":"hello","delivery":"queue","resume":true}"#,
    );
    assert_eq!(status, 200);
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 1);
    assert!(
        bodies[0].contains("core/skill-guidance is temporarily unavailable"),
        "an unreadable catalog must be reported, not rendered as no skills"
    );
    assert!(!bodies[0].contains("<available_skills>"));
    service.stop_after_witness(&root);
}

/// J02 / SKD-E3: a loaded skill's own files reach the conversation through the
/// skill tool wherever the skill is installed (home, configured directory,
/// project), while ordinary reads stay inside the project and a skill file
/// path cannot climb out of its skill.
#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn skd_installed_skill_files_reach_the_next_request_through_the_skill() {
    let _journey = process_journey_custody();
    let root = root("skd-skill-files");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    attended(&root);
    install_skills(&root);
    write(
        &root.join(".agents/skills/release-notes/template.md"),
        "SKD-PROJECT-ASSET-41c0\n",
    );
    let outside = super::root("skd-skill-files-outside");
    let _outside_cleanup = terminal_coordination::RootCleanup(outside.clone());
    let global = outside.join("home/.agents/skills/global-notes");
    write(
        &global.join("SKILL.md"),
        "---\ndescription: Global notes.\n---\nSKD-GLOBAL-BODY\nUse template.md.\n",
    );
    write(&global.join("template.md"), "SKD-GLOBAL-ASSET-5e21\n");
    write(
        &outside.join("home/.agents/skills/escape.md"),
        "SKD-ESCAPED-77aa\n",
    );
    let configured = outside.join("config/skills/configured-notes");
    write(
        &configured.join("SKILL.md"),
        "---\ndescription: Configured notes.\n---\nbody\n",
    );
    write(
        &configured.join("guide/steps.md"),
        "SKD-CONFIGURED-ASSET-0b9d\n",
    );
    let global_template = global.join("template.md");
    let global_template = global_template
        .to_str()
        .unwrap_or_else(|| panic!("asset path"));
    let file = |call: &str, id: &str, file: &str| {
        Script::Raw(tool_call(
            call,
            "skill",
            serde_json::json!({"id": id, "file": file}),
        ))
    };
    let provider = ProviderFixture::start(vec![
        Script::Raw(skill_call("call_skd_files_load", "global-notes")),
        file("call_skd_files_global", "global-notes", "template.md"),
        file(
            "call_skd_files_configured",
            "configured-notes",
            "guide/steps.md",
        ),
        file("call_skd_files_project", "release-notes", "template.md"),
        file("call_skd_files_escape", "global-notes", "../escape.md"),
        Script::Raw(tool_call(
            "call_skd_files_read",
            "read",
            serde_json::json!({"path": global_template}),
        )),
        Script::Success,
    ]);
    let home_dir = outside.join("home");
    let config_dir = outside.join("config");
    let environment = [
        (
            "GRAPT_TEST_HOME",
            home_dir.to_str().unwrap_or_else(|| panic!("home")),
        ),
        (
            "GRAPT_CONFIG_DIR",
            config_dir.to_str().unwrap_or_else(|| panic!("config")),
        ),
    ];
    let mut service = spawn_with(&root, provider.address, &environment);
    let address = await_ready(&root, &mut service);
    let _approver = approval::Attachment::open(address, "*");
    let session = create(address);
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        r#"{"id":"msg_skd_files","text":"Use the notes skills and their templates.","delivery":"queue","resume":true}"#,
    );
    for call in [
        "call_skd_files_load",
        "call_skd_files_global",
        "call_skd_files_configured",
        "call_skd_files_project",
    ] {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);

    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 7, "the skill-file request sequence changed");
    assert!(
        bodies[1].contains("SKD-GLOBAL-BODY") && bodies[1].contains("<file>template.md</file>")
    );
    assert!(
        !bodies[1].contains("SKD-GLOBAL-ASSET-5e21"),
        "listing a file is not loading it"
    );
    assert!(
        bodies[2].contains("SKD-GLOBAL-ASSET-5e21"),
        "the home skill's file arrived"
    );
    assert!(
        bodies[3].contains("SKD-CONFIGURED-ASSET-0b9d"),
        "the configured skill's file arrived"
    );
    assert!(
        bodies[4].contains("SKD-PROJECT-ASSET-41c0"),
        "the project skill's file arrived"
    );
    assert!(bodies[5].contains("relative to the skill's base directory"));
    assert!(
        bodies[5..]
            .iter()
            .all(|body| !body.contains("SKD-ESCAPED-77aa")),
        "a skill path escaped"
    );
    assert!(
        bodies[6].contains("tool path resolves outside the project root"),
        "ordinary reads keep workspace containment"
    );
    service.stop_after_witness(&root);
    fs::remove_dir_all(&outside).unwrap_or_else(|error| panic!("outside cleanup: {error}"));
}
