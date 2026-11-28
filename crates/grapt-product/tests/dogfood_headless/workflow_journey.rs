//! SKILLS-DELEGATION-01 J04: one repository task through the whole workflow.
//!
//! Scoped AGENTS.md, an installed skill and foreground delegation to a
//! differing model in one Session, then a superseding instruction across a
//! compaction, then a restart. Every claim is read off the provider requests
//! the loopback received, in order.

use super::skill_activation::{install_skills, skill_call, tool_call, BODY_MARKER};
use super::subagent_model_routing::{offer_child_model, CHILD_MODEL};
use super::*;

const ROOT_RULE: &str = "SKD-ROOT-RULE: keep release notes terse.";
const PACKAGE_RULE: &str = "SKD-PACKAGE-RULE: package files use two-space indents.";
const SUPERSEDE: &str = "SKD-SUPERSEDE: write the notes to CHANGES.md, not NOTES.md.";
const TEMPLATE: &str = "SKD-TEMPLATE-ASSET-c3e8\n## Changes\n";
const NOTES: &str = "SKD-TEMPLATE-ASSET-c3e8\n## Changes\n- pkg: version 2\n";
const LARGE_READ: usize = 36_000;
const READS: [(&str, &str); 8] = [
    ("call_skd_bulk_1", "bulk-1.txt"),
    ("call_skd_bulk_2", "bulk-2.txt"),
    ("call_skd_bulk_3", "bulk-3.txt"),
    ("call_skd_bulk_4", "bulk-4.txt"),
    ("call_skd_bulk_5", "bulk-5.txt"),
    ("call_skd_bulk_6", "bulk-6.txt"),
    ("call_skd_bulk_7", "bulk-7.txt"),
    ("call_skd_bulk_8", "bulk-8.txt"),
];
/// Every governed effect the journey completes, each exactly once.
const EFFECTS: [&str; 10] = [
    "call_skd_discover",
    "call_skd_pkg",
    "call_skd_skill",
    "call_skd_template",
    "call_skd_child",
    "call_skd_edit",
    "call_skd_pkg_again",
    "call_skd_template_again",
    "call_skd_write",
    "call_skd_bulk_1",
];

fn prompt(address: SocketAddr, session: &str, id: &str, text: &str) {
    let body = serde_json::json!({"id": id, "text": text, "delivery": "queue", "resume": true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
}

fn model(body: &serde_json::Value) -> &str {
    body["model"]
        .as_str()
        .unwrap_or_else(|| panic!("a provider request named no model: {body}"))
}

/// `(call id, status)` for every journey tool call in public history.
fn effect_census(address: SocketAddr, session: &str) -> Vec<(String, String)> {
    let page = json(
        address,
        "GET",
        &format!("/api/session/{session}/message"),
        "",
    );
    page["data"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|message| message["content"].as_array())
        .flatten()
        .filter_map(|part| {
            let id = part["id"]
                .as_str()
                .filter(|id| id.starts_with("call_skd_"))?;
            Some((id.to_owned(), part["state"]["status"].as_str()?.to_owned()))
        })
        .collect()
}

#[test]
#[expect(clippy::cognitive_complexity, reason = "legacy")]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn skd_repository_task_uses_scope_skill_and_delegation_across_compaction_and_restart(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("skd-workflow");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 500_000);
    unattended(&root);
    offer_child_model(&root, true);
    let home = install_skills(&root);
    fs::write(
        root.join(".agents/skills/release-notes/template.md"),
        TEMPLATE,
    )?;
    fs::write(root.join("AGENTS.md"), ROOT_RULE)?;
    fs::create_dir_all(root.join("pkg"))?;
    fs::write(root.join("pkg/AGENTS.md"), PACKAGE_RULE)?;
    fs::write(root.join("pkg/lib.txt"), "version = 1\n")?;
    for (_, path) in READS {
        fs::write(root.join(path), text_lines(b'x', LARGE_READ))?;
    }
    let template = serde_json::json!({"id": "release-notes", "file": "template.md"});
    let provider = ProviderFixture::start(vec![
        Script::Raw(tool_call(
            "call_skd_discover",
            "skill",
            serde_json::json!({"query":"release notes"}),
        )),
        // Objective: read inside the package, load the skill and its template,
        // delegate a question, then make the code change.
        Script::ToolRead {
            call: "call_skd_pkg",
            path: "pkg/lib.txt",
        },
        Script::Raw(skill_call("call_skd_skill", "release-notes")),
        Script::Raw(tool_call("call_skd_template", "skill", template.clone())),
        Script::ToolSubagent {
            call: "call_skd_child",
            task: "list the package files",
        },
        Script::Success,
        Script::Raw(tool_call(
            "call_skd_edit",
            "edit",
            serde_json::json!({"path": "pkg/lib.txt", "old_text": "version = 1", "new_text": "version = 2"}),
        )),
        Script::SuccessText("version bumped; notes next"),
        // The superseding instruction: grouped reads force one compaction,
        // then the scope and the template are deliberately reloaded and the
        // notes are written where the human now wants them.
        Script::ToolReads {
            calls: READS.to_vec(),
        },
        Script::Output(5_645),
        Script::ToolRead {
            call: "call_skd_pkg_again",
            path: "pkg/lib.txt",
        },
        Script::Raw(tool_call("call_skd_template_again", "skill", template)),
        Script::Raw(tool_call(
            "call_skd_write",
            "write",
            serde_json::json!({"path": "CHANGES.md", "content": NOTES}),
        )),
        Script::SuccessText("notes written to CHANGES.md"),
        // After the restart.
        Script::SuccessText("status after restart"),
    ]);
    let environment = [("GRAPT_TEST_HOME", home.as_str())];
    let mut service = spawn_with(&root, provider.address, &environment);
    let address = await_ready(&root, &mut service);
    let session = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("session response carried no id")?
        .to_owned();
    let (status, _) = request(
        address,
        "PUT",
        &format!("/api/session/{session}/subagent-model"),
        &format!(r#"{{"model":{{"providerID":"openai","id":"{CHILD_MODEL}"}}}}"#),
    );
    assert_eq!(status, 204);

    prompt(
        address,
        &session,
        "msg_skd_objective",
        "Bump the package version and draft release notes in NOTES.md.",
    );
    for call in [
        "call_skd_discover",
        "call_skd_pkg",
        "call_skd_skill",
        "call_skd_template",
        "call_skd_child",
        "call_skd_edit",
    ] {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);

    prompt(address, &session, "msg_skd_supersede", SUPERSEDE);
    for (call, _) in READS {
        super::compaction_continuity::approve(address, &session, call);
    }
    for call in [
        "call_skd_pkg_again",
        "call_skd_template_again",
        "call_skd_write",
    ] {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    service.stop_after_witness(&root);
    drop(service);

    let mut restarted = spawn_with(&root, provider.address, &environment);
    let address = await_ready(&root, &mut restarted);
    prompt(address, &session, "msg_skd_status", "Report status.");
    permission_residuals::wait_session_idle(address, &session);

    let bodies = locked(&provider.bodies)
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(bodies.len(), 15, "the journey's request sequence changed");
    let initial = bodies[0].to_string();
    assert!(initial.contains("use the skill tool to discover it"));
    assert!(!initial.contains("Draft release notes from the change log."));
    assert!(!initial.contains(BODY_MARKER));
    // The discovery call adds metadata before the established repository flow.
    let bodies = &bodies[1..];
    let text: Vec<String> = bodies.iter().map(ToString::to_string).collect();

    // Scope: the root rule and the precedence statement are ambient; the
    // package rule is not, until a read enters the package.
    assert!(text[0].contains("SKD-ROOT-RULE") && text[0].contains("AGENTS.md precedence:"));
    assert!(!text[0].contains("SKD-PACKAGE-RULE"));
    assert!(
        text[1].contains("SKD-PACKAGE-RULE"),
        "the package rule rode on the read"
    );
    // Skill: offered by metadata, loaded on activation, its template loaded on purpose.
    assert!(
        text[0].contains("release-notes")
            && text[0].contains("Draft release notes from the change log.")
            && !text[0].contains(BODY_MARKER)
    );
    assert!(text[2].contains(BODY_MARKER) && !text[2].contains("SKD-TEMPLATE-ASSET"));
    assert!(
        text[3].contains("SKD-TEMPLATE-ASSET"),
        "the skill's template reached the request"
    );
    // Delegation: the child ran on the selected model, under the same scope.
    assert_eq!(model(&bodies[3]), "gpt-live");
    assert_eq!(
        model(&bodies[4]),
        CHILD_MODEL,
        "the child ran on its own model"
    );
    assert!(text[4].contains("SKD-ROOT-RULE"));
    assert_eq!(model(&bodies[5]), "gpt-live");
    assert!(
        text[5].contains("Hello from the live provider."),
        "the child's answer returned"
    );
    // Compaction: one summary request.
    assert!(text[8].contains("Output exactly the Markdown structure shown inside <template>"));
    assert_eq!(
        text.iter()
            .filter(|body| body
                .contains("Output exactly the Markdown structure shown inside <template>"))
            .count(),
        1
    );
    // After compaction the superseding instruction and ambient scope hold,
    // and the package rule and the template are back because they were reloaded.
    for body in &text[9..] {
        assert!(body.contains("SKD-ROOT-RULE"), "ambient scope was lost");
        assert!(
            body.contains("use the skill tool to discover it"),
            "skill guidance was lost"
        );
        assert!(
            body.contains("SKD-SUPERSEDE"),
            "the superseding instruction was lost"
        );
    }
    assert!(
        text[10].contains("SKD-PACKAGE-RULE"),
        "the package rule was reloaded"
    );
    assert!(
        text[11].contains("SKD-TEMPLATE-ASSET"),
        "the template was reloaded"
    );
    // The artifact: the code change and the notes, at the superseding destination.
    assert_eq!(
        fs::read_to_string(root.join("pkg/lib.txt"))?,
        "version = 2\n"
    );
    assert_eq!(fs::read_to_string(root.join("CHANGES.md"))?, NOTES);
    assert!(
        !root.join("NOTES.md").exists(),
        "the superseded destination was written"
    );
    // Restart: every completed effect is recorded once and none ran again.
    let census = effect_census(address, &session);
    for effect in EFFECTS {
        let rows: Vec<_> = census.iter().filter(|(id, _)| id == effect).collect();
        assert_eq!(
            rows.len(),
            1,
            "{effect} is not recorded exactly once: {census:?}"
        );
        assert_eq!(
            rows[0].1, "completed",
            "{effect} did not complete: {census:?}"
        );
    }
    let children: Vec<_> = json(address, "GET", "/api/session", "")["data"]
        .as_array()
        .ok_or("session list was not an array")?
        .iter()
        .filter(|row| row["parentID"].as_str() == Some(session.as_str()))
        .cloned()
        .collect();
    assert_eq!(children.len(), 1, "a completed child ran again");
    assert!(text[13].contains("Report status."));
    restarted.stop_after_witness(&root);
    Ok(())
}

/// J01 fork: a forked Session takes its instructions from the files as they
/// are when it runs, under the same scope rules, and the parent's superseded
/// AGENTS.md text does not ride along.
#[test]
fn skd_a_fork_reads_current_scoped_instructions() -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("skd-fork-scope");
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 8_000);
    fs::write(root.join("AGENTS.md"), "SKD-FORK-RULE-ONE")?;
    let provider = ProviderFixture::start(vec![
        Script::SuccessText("parent answer"),
        Script::SuccessText("fork answer"),
    ]);
    let mut service = spawn(&root, provider.address);
    let address = await_ready(&root, &mut service);
    let parent = json(address, "POST", "/api/session", "{}")["data"]["id"]
        .as_str()
        .ok_or("session response carried no id")?
        .to_owned();
    let inspection_path = format!("/api/session/{parent}/instructions/inspect");
    assert_eq!(
        json(address, "GET", &inspection_path, "")["captured"],
        false
    );
    let (status, _) = request(
        address,
        "PUT",
        &format!("/api/session/{parent}/instructions/entries/private-test"),
        r#"{"value":"PRIVATE_CONTEXT_VALUE"}"#,
    );
    assert_eq!(status, 204);
    prompt(address, &parent, "msg_skd_fork_parent", "first task");
    permission_residuals::wait_session_idle(address, &parent);

    fs::write(root.join("AGENTS.md"), "SKD-FORK-RULE-TWO")?;
    let inspection = json(address, "GET", &inspection_path, "");
    assert_eq!(inspection["captured"], true);
    assert!(inspection["ambient"].as_str().is_some_and(
        |text| text.contains("SKD-FORK-RULE-ONE") && !text.contains("SKD-FORK-RULE-TWO")
    ));
    assert!(!inspection.to_string().contains("PRIVATE_CONTEXT_VALUE"));
    let forked = json(
        address,
        "POST",
        &format!("/api/session/{parent}/fork"),
        "{}",
    )["data"]["id"]
        .as_str()
        .ok_or("the fork named no Session")?
        .to_owned();
    prompt(address, &forked, "msg_skd_fork_child", "fork task");
    permission_residuals::wait_session_idle(address, &forked);

    let bodies = locked(&provider.bodies).clone();
    assert_eq!(bodies.len(), 2);
    assert!(bodies[0].contains("SKD-FORK-RULE-ONE"));
    assert!(
        bodies[1].contains("SKD-FORK-RULE-TWO") && bodies[1].contains("AGENTS.md precedence:"),
        "the fork runs under the current scoped instructions"
    );
    let request: serde_json::Value = serde_json::from_str(&bodies[1])?;
    let input = request["input"]
        .as_array()
        .ok_or("fork request has no input array")?;
    let latest = input
        .iter()
        .filter(|item| item["role"] == "system")
        .filter_map(|item| item["content"].as_str())
        .rfind(|text| text.contains("SKD-FORK-RULE-ONE") || text.contains("SKD-FORK-RULE-TWO"))
        .ok_or("fork lost project instructions")?;
    assert!(
        latest.contains("SKD-FORK-RULE-TWO") && !latest.contains("SKD-FORK-RULE-ONE"),
        "the latest fork instruction value must supersede its inherited baseline"
    );
    if bodies[1].contains("SKD-FORK-RULE-ONE") {
        assert!(
            latest.starts_with(
                "Instruction source core/instructions replaces its previously supplied value:"
            ),
            "inherited old instructions require an explicit later replacement"
        );
    }
    service.stop_after_witness(&root);
    Ok(())
}
