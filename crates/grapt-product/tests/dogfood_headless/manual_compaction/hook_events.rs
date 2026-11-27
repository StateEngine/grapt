use super::*;

#[test]
fn compaction_hooks_surround_the_operation_and_post_requires_a_commit() {
    let _journey = process_journey_custody();
    for success in [true, false] {
        let summaries = if success {
            vec![Script::SuccessText(
                "## Objective\n- continue the seed task",
            )]
        } else {
            vec![Script::SuccessText("")]
        };
        let (root, _provider, mut child, address, session) = seeded_configured(
            if success {
                "compact-hooks-success"
            } else {
                "compact-hooks-failure"
            },
            summaries,
            None,
            &["PreCompact", "PostCompact"],
        );
        prompt(address, &session, "msg_hook_compact", "continue");
        permission_residuals::wait_session_idle(address, &session);
        assert_carried(&root, "msg_hook_compact", "session.execution.succeeded.1");
        child.stop_after_witness(&root);
        drop(child);
        let text = fs::read_to_string(root.join("advanced-events.jsonl"))
            .unwrap_or_else(|error| panic!("compaction hook log missing: {error}"));
        let observed: Vec<String> = text
            .lines()
            .map(|line| {
                let row: serde_json::Value = serde_json::from_str(line)
                    .unwrap_or_else(|error| panic!("compaction hook row malformed: {error}"));
                row["hook_event_name"]
                    .as_str()
                    .unwrap_or_else(|| panic!("hook event missing"))
                    .to_owned()
            })
            .collect();
        let expected = if success {
            vec!["PreCompact", "PostCompact"]
        } else {
            vec!["PreCompact"]
        };
        assert_eq!(observed, expected);
        assert_eq!(
            events(&root, "session.compaction.ended.2").is_empty(),
            !success
        );
        process_fixture::cleanup(&root);
    }
}
