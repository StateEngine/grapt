//! Manual compaction preserves the coding prefix, then warms its replacement.
use super::*;

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn requested_anthropic_summary_reuses_the_coding_prefix_and_its_replacement_stays_stable(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("manual-anthropic-cache-prefix");
    prepare(&root, 120_000);
    context_quality::configure_anthropic(&root, 200_000);
    let catalog_path = root.join("model-catalog.json");
    let mut catalog: serde_json::Value = serde_json::from_slice(&fs::read(&catalog_path)?)?;
    catalog["anthropic"]["models"]["claude-live"]["limit"]["output"] = serde_json::json!(8192);
    fs::write(catalog_path, serde_json::to_vec(&catalog)?)?;
    let response =
        |text| context_quality::anthropic_text(text, serde_json::json!({"input_tokens": 100}));
    let provider = ProviderFixture::start(vec![
        response("seeded"),
        response("## Objective\n- Preserve the lookup notes and continue the task."),
        response("continued after compaction"),
        response("continued with the same new prefix"),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = create(address);
    context_quality::select_anthropic(address, &session);
    prompt(
        address,
        &session,
        "msg_cache_manual_seed",
        &"Keep these lookup notes for the next question. ".repeat(256),
    );
    assert_carried(
        &root,
        "msg_cache_manual_seed",
        "session.execution.succeeded.1",
    );
    child.stop_after_witness(&root);
    drop(child);
    admit_offline(&root, &session);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    for (id, text) in [
        ("msg_cache_manual_next", "Continue the lookup task."),
        (
            "msg_cache_manual_later",
            "Continue again without changing the task.",
        ),
    ] {
        prompt(address, &session, id, text);
        assert_carried(&root, id, "session.execution.succeeded.1");
        permission_residuals::wait_session_idle(address, &session);
    }
    child.stop_after_witness(&root);
    drop(child);
    let bodies = locked(&provider.bodies)
        .iter()
        .map(|body| serde_json::from_str::<serde_json::Value>(body))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        bodies.len(),
        4,
        "one summary, two continuations, no extra calls"
    );
    let fixed = |body: &serde_json::Value| {
        let mut body = body.clone();
        if let Some(body) = body.as_object_mut() {
            body.remove("messages");
        }
        body
    };
    assert_eq!(
        fixed(&bodies[0]),
        fixed(&bodies[1]),
        "summary rewrote the coding head"
    );
    assert_eq!(fixed(&bodies[1]), fixed(&bodies[2]));
    assert_eq!(fixed(&bodies[2]), fixed(&bodies[3]));
    let messages = |body: &serde_json::Value| -> Result<Vec<serde_json::Value>, &'static str> {
        body["messages"]
            .as_array()
            .cloned()
            .ok_or("missing messages")
    };
    let seed = messages(&bodies[0])?;
    let summary = messages(&bodies[1])?;
    let last = summary.last().ok_or("empty summary input")?;
    assert_eq!(last["role"], "user");
    assert!(last.to_string().contains(SUMMARY_REQUEST));
    // Cache markers move as the tail grows; all content before that tail must
    // survive exactly. Keep the raw head/options assertions above unnormalized.
    fn content(mut value: serde_json::Value) -> serde_json::Value {
        match &mut value {
            serde_json::Value::Object(object) => {
                object.remove("cache_control");
                for child in object.values_mut() {
                    *child = content(child.take());
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    *child = content(child.take());
                }
            }
            _ => {}
        }
        value
    }
    assert_eq!(
        content(serde_json::json!(&summary[..seed.len()])),
        content(serde_json::json!(seed))
    );
    let resumed = messages(&bodies[2])?;
    let later = messages(&bodies[3])?;
    assert_eq!(
        content(serde_json::json!(&resumed[0])),
        content(serde_json::json!(&summary[0])),
        "whole original seed must remain reusable before the summary"
    );
    assert!(resumed.iter().any(|message| {
        message["role"] == "assistant"
            && message
                .to_string()
                .contains("Model-written continuation summary.")
            && message
                .to_string()
                .contains("Earlier user messages are historical")
    }));
    assert_eq!(
        content(serde_json::json!(&later[..resumed.len()])),
        content(serde_json::json!(resumed))
    );
    let ended = events(&root, "session.compaction.ended.2");
    assert_eq!(ended.len(), 1);
    assert_eq!(ended[0]["trigger"], "userRequested");
    assert_eq!(pending(&root), 0);
    process_fixture::cleanup(&root);
    Ok(())
}
