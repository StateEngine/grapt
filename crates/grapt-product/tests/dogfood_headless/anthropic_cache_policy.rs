//! SC-CACHE-04: shared prefix bytes and System authority on real Product requests.
use super::*;

#[test]
fn cache04_two_native_sessions_share_the_cached_system_prefix_and_hour_ttl(
) -> Result<(), Box<dyn std::error::Error>> {
    let _journey = process_journey_custody();
    let root = root("cache04-session-prefix");
    prepare(&root, 120_000);
    context_quality::configure_anthropic(&root, 200_000);
    let provider = ProviderFixture::start(vec![
        context_quality::anthropic_text("first done", serde_json::json!({"input_tokens":20})),
        context_quality::anthropic_text("second done", serde_json::json!({"input_tokens":20})),
    ]);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let mut sessions = Vec::new();
    for _ in 0..2 {
        let session = create(address);
        context_quality::select_anthropic(address, &session);
        json(
            address,
            "POST",
            &format!("/api/session/{session}/prompt"),
            r#"{"text":"hello"}"#,
        );
        permission_residuals::wait_session_idle(address, &session);
        sessions.push(session);
    }
    assert_ne!(sessions[0], sessions[1]);
    let bodies = locked(&provider.bodies)
        .iter()
        .map(|raw| serde_json::from_str::<serde_json::Value>(raw))
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(bodies.len(), 2);
    assert_eq!(bodies[0]["tools"], bodies[1]["tools"]);
    assert_eq!(bodies[0]["system"][0], bodies[1]["system"][0]);
    for (body, session) in bodies.iter().zip(sessions) {
        assert_eq!(body["max_tokens"], 32000);
        let system = body["system"].as_array().ok_or("System blocks")?;
        assert_eq!(system.len(), 2);
        assert_eq!(
            system[0]["cache_control"],
            serde_json::json!({"type":"ephemeral","ttl":"1h"})
        );
        assert!(!system[0].to_string().contains(&session));
        assert!(system[1]["text"]
            .as_str()
            .ok_or("session context")?
            .contains(&session));
        assert!(system[1].get("cache_control").is_none());
        assert!(!body["messages"].to_string().contains(&session));
        assert_eq!(
            body["tools"]
                .as_array()
                .and_then(|tools| tools.last())
                .ok_or("last tool")?["cache_control"]["ttl"],
            "1h"
        );
    }
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
    Ok(())
}
