//! Real native requests retain an immutable prefix across a project instruction edit.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn native_instruction_edits_use_supported_system_updates_without_relabeling_user_data(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    for (label, model, inline) in [
        ("native", "claude-opus-5-5", true),
        ("legacy", "claude-sonnet-5", false),
    ] {
        let root = root(&format!("anthropic-epochs-{label}"));
        prepare(&root, 120_000);
        attended(&root);
        context_quality::configure_anthropic(&root, 200_000);
        for name in ["model-catalog.json", "anthropic-account-models.json"] {
            let path = root.join(name);
            fs::write(
                &path,
                fs::read_to_string(&path)?.replace("claude-live", model),
            )?;
        }
        fs::write(
            root.join("AGENTS.md"),
            "NATIVE-EPOCH-OLD repository policy.",
        )?;
        fs::write(root.join("a.txt"), "file contents")?;
        let provider = ProviderFixture::start(vec![
            Script::Raw(context_quality::anthropic_stream(
                &[
                    serde_json::json!({"type":"thinking","thinking":"inspect before editing",
                    "signature":"NATIVE-EPOCH-SIGNATURE"}),
                    serde_json::json!({"type":"tool_use","id":"toolu_epoch","name":"write",
                    "input":{"path":"a.txt","content":"updated file contents"}}),
                ],
                "tool_use",
                serde_json::json!({"input_tokens":20}),
            )),
            context_quality::anthropic_text("done", serde_json::json!({"input_tokens":30})),
        ]);
        let mut child = spawn(&root, provider.address);
        let address = await_ready(&root, &mut child);
        let _approver = approval::Attachment::open(address, "*");
        let session = create(address);
        let (status, _) = request(
            address,
            "POST",
            &format!("/api/session/{session}/model"),
            &serde_json::json!({"model":{"providerID":"anthropic","id":model}}).to_string(),
        );
        assert_eq!(status, 204);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"id":"msg_epoch","text":"update a.txt","delivery":"queue","resume":true}"#,
        );
        wait_calls(&provider.calls, 1);
        let asked = permission_residuals::wait_until(
            Duration::from_secs(10),
            permission_residuals::SETTLE_POLL,
            || {
                let page = json(
                    address,
                    "GET",
                    &format!("/api/session/{session}/permission"),
                    "",
                );
                page["data"].as_array().is_some_and(|rows| {
                    rows.iter()
                        .any(|row| row["source"]["callID"] == "toolu_epoch")
                })
            },
        );
        assert!(
            asked.is_some(),
            "the instruction edit must occur while the write awaits approval"
        );
        fs::write(
            root.join("AGENTS.md"),
            "NATIVE-EPOCH-NEW repository policy.",
        )?;
        compaction_continuity::approve(address, &session, "toolu_epoch");
        permission_residuals::wait_session_idle(address, &session);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        let bodies = locked(&provider.bodies)
            .iter()
            .map(|body| serde_json::from_str::<serde_json::Value>(body))
            .collect::<Result<Vec<_>, _>>()?;
        assert!(bodies[0]["system"].to_string().contains("NATIVE-EPOCH-OLD"));
        assert!(bodies[1]["messages"]
            .as_array()
            .ok_or("messages")?
            .iter()
            .filter(|message| message["role"] == "assistant")
            .flat_map(|message| message["content"].as_array().into_iter().flatten())
            .any(|block| block["type"] == "thinking"
                && block["thinking"] == "inspect before editing"
                && block["signature"] == "NATIVE-EPOCH-SIGNATURE"));
        if inline {
            assert_eq!(bodies[0]["system"], bodies[1]["system"]);
            let messages = bodies[1]["messages"].as_array().ok_or("messages")?;
            let last = messages.last().ok_or("last message")?;
            assert_eq!(last["role"], "system");
            assert!(last.to_string().contains("NATIVE-EPOCH-NEW"));
            assert!(!last.to_string().contains("NATIVE-EPOCH-OLD"));
            let results = &messages.get(messages.len() - 2).ok_or("tool results")?["content"];
            assert_eq!(results[0]["type"], "tool_result");
            assert_eq!(results[0]["tool_use_id"], "toolu_epoch");
            assert!(messages
                .iter()
                .filter(|message| message["role"] == "user")
                .all(|message| !message.to_string().contains("NATIVE-EPOCH-NEW")));
        } else {
            assert!(bodies[1]["system"].to_string().contains("NATIVE-EPOCH-NEW"));
            assert!(!bodies[1]["system"].to_string().contains("NATIVE-EPOCH-OLD"));
            assert!(bodies[1]["messages"]
                .as_array()
                .ok_or("messages")?
                .iter()
                .all(|message| message["role"] != "system"));
        }
        child.stop_after_witness(&root);
        drop(child);
        process_fixture::cleanup(&root);
    }
    Ok(())
}
