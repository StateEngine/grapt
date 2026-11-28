//! Reconnecting a named connection without a model tries only models on the
//! wire that connection already uses.
use crate::dogfood::connection_setup::key::setup::candidate;
use grapt_connections::{catalog::KeyProvider, store::Family};
use serde_json::json;

#[test]
fn a_named_reconnect_tries_only_models_on_its_saved_wire() -> Result<(), String> {
    let provider = KeyProvider::from_id("snowflake-cortex")?.ok_or("mixed endpoint")?;
    let messages = json!({"id":"claude-test"});
    let chat = json!({"id":"chat-test"});
    assert!(candidate(provider, None, &messages) && candidate(provider, None, &chat));
    let wire = Some(Family::AnthropicMessages);
    assert!(candidate(provider, wire, &messages));
    assert!(
        !candidate(provider, wire, &chat),
        "a model on the other wire was tried"
    );
    let wire = Some(provider.family());
    assert!(candidate(provider, wire, &chat));
    assert!(!candidate(provider, wire, &messages));
    Ok(())
}
