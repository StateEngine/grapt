//! SKILLS-DELEGATION-01 J01: scoped AGENTS.md on the ordinary request.
//!
//! Scoped files reach the model on the read that enters their directory. With
//! no ambient AGENTS.md at all they must still carry the precedence contract,
//! and a walk cut short by its bound must say so. Both read formats are
//! checked on the provider requests the loopback received.

use super::skill_activation::tool_call;
use super::*;

const CONTRACT: &str = "The scoped files below govern the file just read";
const HUMAN_RULE: &str = "SKD-HUMAN-RULE: use tabs, whatever AGENTS.md says.";

fn run(
    label: &str,
    layout: impl Fn(&Path),
    calls: Vec<(&'static str, &'static str, serde_json::Value)>,
) -> Vec<String> {
    let _journey = process_journey_custody();
    let root = root(label);
    let _cleanup = terminal_coordination::RootCleanup(root.clone());
    prepare(&root, 120_000);
    // No global, root or session AGENTS.md: only scoped files can speak.
    fs::create_dir(root.join("empty-home")).unwrap_or_else(|error| panic!("fixture home: {error}"));
    layout(&root);
    let mut script: Vec<Script> = calls
        .iter()
        .map(|(call, tool, arguments)| Script::Raw(tool_call(call, tool, arguments.clone())))
        .collect();
    script.push(Script::Success);
    let provider = ProviderFixture::start(script);
    let home = root.join("empty-home");
    let home = home.to_str().unwrap_or_else(|| panic!("home is not UTF-8"));
    let mut service = spawn_with(&root, provider.address, &[("GRAPT_TEST_HOME", home)]);
    let address = await_ready(&root, &mut service);
    let session = create(address);
    let body = serde_json::json!({"id": "msg_skd_scoped", "text": HUMAN_RULE, "delivery": "queue", "resume": true});
    json(
        address,
        "POST",
        &format!("/api/session/{session}/prompt"),
        &body.to_string(),
    );
    for (call, _, _) in &calls {
        super::compaction_continuity::approve(address, &session, call);
    }
    permission_residuals::wait_session_idle(address, &session);
    let bodies = locked(&provider.bodies).clone();
    assert_eq!(
        bodies.len(),
        calls.len() + 1,
        "the request sequence changed"
    );
    service.stop_after_witness(&root);
    bodies
}

/// SKD-E1: conflicting nested rules and a superseding human rule reach the
/// request together with the contract that decides between them.
#[test]
fn skd_scoped_only_conflicts_reach_the_request_with_their_precedence() {
    let bodies = run(
        "skd-scoped-precedence",
        |root| {
            fs::create_dir_all(root.join("pkg/deep")).unwrap_or_else(|error| panic!("{error}"));
            fs::write(root.join("pkg/AGENTS.md"), "SKD-PARENT-RULE: four spaces")
                .unwrap_or_else(|error| panic!("{error}"));
            fs::write(
                root.join("pkg/deep/AGENTS.md"),
                "SKD-CHILD-RULE: two spaces",
            )
            .unwrap_or_else(|error| panic!("{error}"));
            fs::write(root.join("pkg/deep/file.txt"), "file body")
                .unwrap_or_else(|error| panic!("{error}"));
        },
        vec![
            (
                "call_skd_scope_read",
                "read",
                serde_json::json!({"path": "pkg/deep/file.txt"}),
            ),
            (
                "call_skd_scope_lines",
                "read_lines",
                serde_json::json!({"path": "pkg/deep/file.txt", "start_line": 1, "structured": true}),
            ),
            (
                "call_skd_scope_compact",
                "read_lines",
                serde_json::json!({"path": "pkg/deep/file.txt"}),
            ),
        ],
    );
    assert!(
        !bodies[0].contains("AGENTS.md precedence:"),
        "there is no ambient AGENTS.md"
    );
    for body in &bodies[1..] {
        assert!(
            body.contains("SKD-HUMAN-RULE"),
            "the superseding human rule is in the request"
        );
    }
    // read: one text block, contract first, then the files most specific first.
    let read = last_tool_output(&bodies[1]);
    let position = |needle: &str| {
        read.find(needle)
            .unwrap_or_else(|| panic!("read lacks {needle}: {read}"))
    };
    assert!(position(CONTRACT) < position("SKD-CHILD-RULE"));
    assert!(position("SKD-CHILD-RULE") < position("SKD-PARENT-RULE"));
    assert!(read.contains("user gives in this conversation override every AGENTS.md file"));
    // read_lines: the same contract and order as structured fields.
    let lines: serde_json::Value = serde_json::from_str(&last_tool_output(&bodies[2]))
        .unwrap_or_else(|error| panic!("read_lines output is not JSON: {error}"));
    let precedence = lines["scopedPrecedence"]
        .as_str()
        .unwrap_or_else(|| panic!("no scopedPrecedence: {lines}"));
    assert!(
        precedence.contains(CONTRACT)
            && precedence.contains("user gives in this conversation override")
    );
    let paths: Vec<&str> = lines["scopedInstructions"]
        .as_array()
        .unwrap_or_else(|| panic!("no scopedInstructions: {lines}"))
        .iter()
        .filter_map(|scoped| scoped["path"].as_str())
        .collect();
    assert_eq!(paths, ["pkg/deep/AGENTS.md", "pkg/AGENTS.md"]);
    let compact = last_tool_output(&bodies[3]);
    assert!(compact.starts_with("File: \"pkg/deep/file.txt\" | sha256: "));
    assert!(compact.contains("\n1→file body\n[end of file]\n"));
    assert!(!compact.contains("byteStart") && !compact.contains("byteEnd"));
    assert!(compact.contains(CONTRACT));
    assert!(compact.find("SKD-CHILD-RULE") < compact.find("SKD-PARENT-RULE"));
}

#[test]
fn same_directory_overrides_reach_ambient_and_scoped_provider_requests() {
    let bodies = run(
        "instr-02-overrides",
        |root| {
            fs::create_dir_all(root.join("pkg/deep"))
                .unwrap_or_else(|error| panic!("override fixture: {error}"));
            for (path, content) in [
                ("AGENTS.md", "INSTR-02-ROOT-DEFAULT"),
                ("AGENTS.override.md", "INSTR-02-ROOT-OVERRIDE"),
                ("pkg/AGENTS.md", "INSTR-02-PARENT-DEFAULT"),
                ("pkg/AGENTS.override.md", "INSTR-02-PARENT-OVERRIDE"),
                ("pkg/deep/AGENTS.md", "INSTR-02-CHILD-DEFAULT"),
                ("pkg/deep/file.txt", "file body"),
            ] {
                fs::write(root.join(path), content)
                    .unwrap_or_else(|error| panic!("override fixture: {error}"));
            }
        },
        vec![
            (
                "call_instr_02_read",
                "read",
                serde_json::json!({"path": "pkg/deep/file.txt"}),
            ),
            (
                "call_instr_02_lines",
                "read_lines",
                serde_json::json!({"path": "pkg/deep/file.txt", "start_line": 1}),
            ),
        ],
    );
    for body in &bodies {
        assert!(body.contains("INSTR-02-ROOT-OVERRIDE") && body.contains("SKD-HUMAN-RULE"));
        assert!(!body.contains("INSTR-02-ROOT-DEFAULT"));
        assert!(!body.contains("INSTR-02-PARENT-DEFAULT"));
    }
    assert!(!bodies[0].contains("INSTR-02-PARENT-OVERRIDE"));
    for body in &bodies[1..] {
        let output = last_tool_output(body);
        assert!(output.contains("INSTR-02-CHILD-DEFAULT"));
        assert!(output.contains("INSTR-02-PARENT-OVERRIDE"));
        assert!(output.contains("pkg/AGENTS.override.md"));
        assert!(output.find("INSTR-02-CHILD-DEFAULT") < output.find("INSTR-02-PARENT-OVERRIDE"));
        assert!(output.contains(CONTRACT));
    }
}

/// The output of the newest tool call in one provider request.
fn last_tool_output(body: &str) -> String {
    let body: serde_json::Value =
        serde_json::from_str(body).unwrap_or_else(|error| panic!("request is not JSON: {error}"));
    body["input"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .rev()
                .find(|item| item["type"] == "function_call_output")
        })
        .and_then(|item| item["output"].as_str())
        .unwrap_or_else(|| panic!("request carried no tool output: {body}"))
        .to_owned()
}

/// SKD-E2: a read exactly at the scope bound sees its governing file; one
/// directory deeper, the request says which directories were not checked.
#[test]
fn skd_a_read_beyond_the_scope_bound_names_what_was_not_checked() {
    const BOUND: usize = 64;
    let at = |depth: usize| vec!["a"; depth].join("/");
    let bounded = format!("{}/f.txt", at(BOUND));
    let beyond = format!("{}/f.txt", at(BOUND + 1));
    let bodies = run(
        "skd-scoped-depth",
        |root| {
            fs::create_dir_all(root.join(at(BOUND + 1))).unwrap_or_else(|error| panic!("{error}"));
            fs::write(root.join("a/AGENTS.md"), "SKD-DEPTH-ONE-RULE")
                .unwrap_or_else(|error| panic!("{error}"));
            fs::write(root.join(at(BOUND)).join("f.txt"), "SKD-BOUNDED-FILE")
                .unwrap_or_else(|error| panic!("{error}"));
            fs::write(root.join(at(BOUND + 1)).join("f.txt"), "SKD-BEYOND-FILE")
                .unwrap_or_else(|error| panic!("{error}"));
        },
        vec![
            (
                "call_skd_depth_bounded",
                "read",
                serde_json::json!({"path": bounded}),
            ),
            (
                "call_skd_depth_beyond",
                "read",
                serde_json::json!({"path": beyond}),
            ),
            (
                "call_skd_depth_lines",
                "read_lines",
                serde_json::json!({"path": beyond, "start_line": 1}),
            ),
        ],
    );
    assert!(bodies[1].contains("SKD-BOUNDED-FILE") && bodies[1].contains("SKD-DEPTH-ONE-RULE"));
    assert!(
        !bodies[1].contains("Scoped instructions incomplete"),
        "the bound itself is complete"
    );
    // The depth-one rule is already in history from the first read, so each
    // beyond-bound read is judged by its own output.
    for (index, format) in [(2, "read"), (3, "read_lines")] {
        let output = last_tool_output(&bodies[index]);
        assert!(
            output.contains("SKD-BEYOND-FILE"),
            "{format}: the file itself arrived"
        );
        assert!(
            output.contains(CONTRACT),
            "{format}: the bound notice comes with the contract"
        );
        // Plain reads carry a human notice; read_lines carries the structured
        // scopedIncomplete field. Their presentation labels need not match.
        assert!(
            output.contains("Scoped instructions incomplete")
                || output.contains("scopedIncomplete"),
            "{format}: incomplete scope is explicit: {output}"
        );
        assert!(
            !output.contains("SKD-DEPTH-ONE-RULE"),
            "{format}: nothing beyond the bound was read"
        );
    }
    assert!(
        bodies[3].contains("scopedIncomplete"),
        "read_lines carries the bound as a field"
    );
}
