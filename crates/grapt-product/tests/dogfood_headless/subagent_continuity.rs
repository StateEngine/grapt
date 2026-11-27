//! SESSION-CONTINUITY-01 J03: subagent delivery across process loss.
//!
//! A foreground child lost with its runtime settles once as process loss under
//! that dead generation and is never replayed; its stale child Session cannot
//! be prompted by anyone afterwards, so no late answer can settle the parent;
//! and after restart a successor child runs under the new generation and
//! delivers its answer to the same parent with its lineage intact.

use super::subagent_child::approve_the_subagent_ask;
use super::*;

struct Operation {
    child: String,
    parent: String,
    status: String,
    reason: Option<String>,
    run_generation: Option<i64>,
    tool_call: Option<String>,
}

fn operations(root: &Path) -> Vec<Operation> {
    let store = EventStore::open(
        &root.join("database.sqlite3"),
        "subagent-continuity-evidence",
    )
    .unwrap_or_else(|error| panic!("subagent evidence store is unreadable: {error}"));
    let mut statement = store
        .connection()
        .prepare(
            "SELECT child_session_id, parent_session_id, status, reason, run_generation, tool_call_id
             FROM subagent_operation ORDER BY rowid",
        )
        .unwrap_or_else(|error| panic!("subagent evidence query failed: {error}"));
    let rows = statement
        .query_map([], |row| {
            Ok(Operation {
                child: row.get(0)?,
                parent: row.get(1)?,
                status: row.get(2)?,
                reason: row.get(3)?,
                run_generation: row.get(4)?,
                tool_call: row.get(5)?,
            })
        })
        .unwrap_or_else(|error| panic!("subagent evidence rows failed: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("subagent evidence row failed: {error}"));
    rows
}

fn tool_state(history: &serde_json::Value, call: &str) -> Option<serde_json::Value> {
    history["data"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|part| part["id"] == call)
        .map(|part| part["state"].clone())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn a_lost_child_settles_once_and_a_successor_child_delivers_to_the_same_parent() {
    let _journey = process_journey_custody();
    let root = root("j03-subagent-continuity");
    prepare(&root, 45_000);
    attended(&root);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let provider = ProviderFixture::start(vec![
        Script::ToolSubagent {
            call: "call_j03_lost_child",
            task: "this child is lost with its runtime",
        },
        Script::HeldOutput {
            bytes: 16,
            entered: Arc::clone(&entered),
            release: Arc::clone(&release),
        },
        Script::ToolSubagent {
            call: "call_j03_successor_child",
            task: "finish the delegated work after restart",
        },
        Script::SuccessText("successor child answer after restart"),
        Script::SuccessText("parent finished with the successor answer"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let _approver = approval::Attachment::open(address, "*");
    let parent = create(address);
    let prompt = {
        let path = format!("/api/session/{parent}/prompt");
        thread::spawn(move || {
            request_with_read_timeout(
                address,
                "POST",
                &path,
                r#"{"id":"msg_j03_first","text":"delegate the first task","delivery":"queue","resume":true}"#,
                Duration::from_secs(60),
            )
        })
    };
    approve_the_subagent_ask(address, &parent);
    let _ = entered.wait();
    child.stop_after_witness(&root);
    drop(child);
    let _ = release.wait();
    let _ = prompt.join();
    let calls_at_loss = provider.calls.load(Ordering::SeqCst);

    let mut restarted = spawn(&root, provider.address);
    let address = await_ready(&root, &mut restarted);
    let _approver = approval::Attachment::open(address, "*");
    let lost = operations(&root);
    assert_eq!(
        lost.len(),
        1,
        "exactly one child was admitted before the loss"
    );
    let lost = &lost[0];
    assert_eq!(
        (lost.status.as_str(), lost.reason.as_deref()),
        ("cancelled", Some("process-loss")),
        "the lost child did not settle as process loss"
    );
    assert_eq!(lost.parent, parent, "the lost child lost its spawn lineage");
    assert_eq!(lost.tool_call.as_deref(), Some("call_j03_lost_child"));
    let history = json(
        address,
        "GET",
        &format!("/api/session/{parent}/message"),
        "",
    );
    let lost_call = tool_state(&history, "call_j03_lost_child").unwrap_or_else(|| {
        panic!("the lost child's call is absent from parent history: {history}")
    });
    assert!(
        lost_call["status"] == "error" && lost_call["error"]["type"] == "ToolOutcomeUnavailable",
        "the lost child's call did not settle as an unavailable outcome: {lost_call}"
    );

    // Recipient escape: the stale child Session accepts no further prompt, so
    // no late answer can be manufactured for the parent.
    let (status, body) = request(
        address,
        "POST",
        &format!("/api/session/{}/prompt", lost.child),
        r#"{"id":"msg_j03_escape","text":"answer the parent late","delivery":"queue","resume":true}"#,
    );
    assert!(
        status >= 400,
        "a stale child Session accepted a late prompt ({status}): {}",
        String::from_utf8_lossy(&body)
    );
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        calls_at_loss,
        "a stale child or restart replayed provider work"
    );

    let (status, body) = request_with_read_timeout(
        address,
        "POST",
        &format!("/api/session/{parent}/prompt"),
        r#"{"id":"msg_j03_second","text":"delegate the work again","delivery":"queue","resume":true}"#,
        Duration::from_secs(60),
    );
    assert_eq!(status, 200, "{}", String::from_utf8_lossy(&body));
    approve_the_subagent_ask(address, &parent);
    permission_residuals::wait_session_idle(address, &parent);
    let settled = operations(&root);
    assert_eq!(
        settled.len(),
        2,
        "the successor child was not admitted once"
    );
    let successor = &settled[1];
    assert_eq!(
        successor.status, "completed",
        "the successor child did not complete"
    );
    assert_eq!(
        successor.parent, parent,
        "the successor child lost its spawn lineage"
    );
    assert_eq!(
        successor.tool_call.as_deref(),
        Some("call_j03_successor_child")
    );
    assert_ne!(
        successor.child, settled[0].child,
        "the successor reused the stale child"
    );
    assert!(
        successor.run_generation.is_some(),
        "the successor child ran without a runtime generation stamp"
    );
    let history = json(
        address,
        "GET",
        &format!("/api/session/{parent}/message"),
        "",
    );
    let delivered = tool_state(&history, "call_j03_successor_child")
        .unwrap_or_else(|| panic!("the successor call is absent from parent history: {history}"));
    assert_eq!(delivered["status"], "completed");
    assert!(
        delivered
            .to_string()
            .contains("successor child answer after restart"),
        "the successor child's answer was not delivered to its parent: {delivered}"
    );
    assert_eq!(
        settled[0].status, "cancelled",
        "a late settlement rewrote the lost child"
    );
    restarted.stop_after_witness(&root);
    drop(restarted);
    process_fixture::cleanup(&root);
}
