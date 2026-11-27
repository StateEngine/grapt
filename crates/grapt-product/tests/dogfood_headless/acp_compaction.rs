//! An ACP turn whose own Step compacts must finish. The product publishes
//! `session.compaction.ended` when any compaction commits; the CLI's event
//! closure refused it, so every ACP turn that compacted (a long interactive session,
//! a long benchmark row) failed with -32603 at its compaction boundary.
#![cfg(unix)]
use super::acp_process::{now, AcpObserved, AcpProcess, ACP_FRAMES};
use super::*;

const SUMMARY_REQUEST: &str = "Output exactly the Markdown structure shown inside <template>";
const CALLS: [(&str, &str); 8] = [
    ("call_auto_1", "auto-1.txt"),
    ("call_auto_2", "auto-2.txt"),
    ("call_auto_3", "auto-3.txt"),
    ("call_auto_4", "auto-4.txt"),
    ("call_auto_5", "auto-5.txt"),
    ("call_auto_6", "auto-6.txt"),
    ("call_auto_7", "auto-7.txt"),
    ("call_auto_8", "auto-8.txt"),
];

/// Trigger of each committed compaction descriptor, read-only.
fn descriptor_triggers(root: &Path) -> Vec<String> {
    let db = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("store open failed: {error}"));
    let mut query = db
        .prepare(
            "SELECT json_extract(data, '$.trigger') FROM event
             WHERE type = 'session.compaction.ended.2' ORDER BY seq",
        )
        .unwrap_or_else(|error| panic!("descriptor query failed: {error}"));
    query
        .query_map([], |row| row.get::<_, String>(0))
        .and_then(|rows| rows.collect())
        .unwrap_or_else(|error| panic!("descriptor read failed: {error}"))
}

/// Eight grouped 36 000-byte reads cross the context threshold, so the
/// turn's next Step runs the automatic compactor before it continues.
#[test]
fn an_acp_turn_that_compacts_automatically_completes() {
    let _journey = process_journey_custody();
    let root = root("acp-auto-compaction");
    prepare(&root, 500_000);
    for (_, path) in CALLS {
        fs::write(root.join(path), text_lines(b'x', 36_000))
            .unwrap_or_else(|error| panic!("large fixture write failed: {error}"));
    }
    let provider = ProviderFixture::start(vec![
        Script::ToolReads {
            calls: CALLS.to_vec(),
        },
        Script::Output(5_645),
        Script::SuccessText("continued after the compaction"),
    ]);
    let deadline = now() + Duration::from_secs(60);
    let mut acp = AcpProcess::start(&root, provider.address, ACP_FRAMES);
    let mut observed = AcpObserved::default();
    let outcome = (|| {
        acp.request(
            1,
            "initialize",
            serde_json::json!({"protocolVersion": 1, "clientCapabilities": {}}),
            deadline,
            &mut observed,
        )?;
        let created = acp.request(
            2,
            "session/new",
            serde_json::json!({"cwd": root, "mcpServers": []}),
            deadline,
            &mut observed,
        )?;
        let session = created["sessionId"]
            .as_str()
            .ok_or("no sessionId")?
            .to_owned();
        acp.request(
            3,
            "session/prompt",
            serde_json::json!({"sessionId": session,
                "prompt": [{"type": "text", "text": "read all eight files"}]}),
            deadline,
            &mut observed,
        )
    })();
    acp.cleanup();
    assert!(
        acp.cleaned_up,
        "the ACP CLI or its product outlived the turn"
    );
    drop(acp);
    let result = outcome.unwrap_or_else(|error| panic!("ACP turn failed: {error}"));
    assert_eq!(result["stopReason"], "end_turn", "{result}");
    assert_eq!(provider.calls.load(Ordering::SeqCst), 3);
    assert!(locked(&provider.bodies)[1].contains(SUMMARY_REQUEST));
    let triggers = descriptor_triggers(&root);
    assert_eq!(triggers.len(), 1, "{triggers:?}");
    assert_ne!(triggers[0], "userRequested");
    process_fixture::cleanup(&root);
}
