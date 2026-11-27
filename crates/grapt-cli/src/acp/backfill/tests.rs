use super::*;
use serde_json::json;
use std::collections::BTreeMap;

fn key(ordinal: u64) -> (String, &'static str, u64) {
    ("msg_a".to_owned(), "agent_message_chunk", ordinal)
}

#[test]
fn only_successful_writes_advance_the_prefix() {
    let mut delivery = Delivery::default();
    let data = BTreeMap::from([
        ("assistantMessageID".to_owned(), json!("msg_a")),
        ("ordinal".to_owned(), json!(0)),
        ("delta".to_owned(), json!("hello ")),
    ]);
    let prepared = Delivery::live_key(&data, "agent_message_chunk")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    assert_eq!(delivery.prefix(&prepared, "hello world").ok(), Some(0));
    delivery
        .record_text(prepared, "hello ")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    delivery.gap();
    delivery.gap();
    assert_eq!(delivery.prefix(&key(0), "hello world").ok(), Some(6));
    assert_eq!(delivery.prefix(&key(1), "another part").ok(), Some(0));
    delivery
        .record_text(key(0), "world")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    assert_eq!(delivery.prefix(&key(0), "hello world").ok(), Some(11));
}

#[test]
fn corrupt_or_non_boundary_prefix_is_an_explicit_fault() {
    let mut delivery = Delivery::default();
    delivery
        .record_text(key(0), "x")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    assert!(delivery.prefix(&key(0), "é and more").is_err());
    assert!(delivery.prefix(&key(0), "y and more").is_err());
    assert!(delivery.prefix(&key(0), "").is_err());
}

#[test]
fn terminal_and_file_effect_guards_survive_repeated_gaps() {
    let mut delivery = Delivery::default();
    delivery
        .record_call("call_a")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    assert_eq!(delivery.claim_sync("call_a").ok(), Some(true));
    delivery
        .record_terminal("call_a")
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    delivery.gap();
    delivery.gap();
    assert!(delivery.call_sent("call_a"));
    assert!(delivery.terminal_sent("call_a"));
    assert_eq!(delivery.claim_sync("call_a").ok(), Some(false));
}

#[test]
fn escaped_chunks_preserve_utf8_and_fit_frames() {
    let block = format!("{}{}\"\\\n", "é".repeat(60_000), "\u{1}".repeat(70_000));
    let owed = chunks(&block);
    assert!(owed.len() > 1);
    for text in &owed {
        let frame = json!({"jsonrpc":"2.0", "method":"session/update", "params":{
            "sessionId":"ses_a", "update":{"sessionUpdate":"agent_message_chunk",
            "messageId":"msg_a", "content":{"type":"text", "text":text}}}});
        assert!(frame.to_string().len() < 262_144);
    }
    assert_eq!(owed.concat(), block);
}

#[test]
fn the_escaped_length_is_what_json_writes() {
    for character in (0u32..0x80)
        .filter_map(char::from_u32)
        .chain(['é', '€', '😀', '\u{2028}'])
    {
        let written =
            serde_json::to_string(&character.to_string()).map_or(0, |json| json.len() - 2);
        assert_eq!(escaped_len(character), written, "{character:?}");
    }
}

#[test]
fn a_later_prompts_turn_is_not_this_prompts() {
    let own = own_turn(vec![
        json!({"type":"assistant", "id":"msg_later", "content":[]}),
        json!({"type":"user", "id":"inp_later"}),
        json!({"type":"assistant", "id":"msg_a", "content":[]}),
    ]);
    assert_eq!(own.len(), 1);
    assert_eq!(own[0]["id"], "msg_a");
}

#[tokio::test]
async fn a_refused_write_does_not_claim_the_missing_text() {
    let mut delivery = Delivery::default();
    let refused = delivery
        .send_text(key(0), "hello", async {
            Err(RpcFault::internal("fixture refused output"))
        })
        .await;
    assert!(refused.is_err());
    assert_eq!(delivery.prefix(&key(0), "hello world").ok(), Some(0));
    delivery
        .send_text(key(0), "hello", async { Ok(()) })
        .await
        .unwrap_or_else(|fault| panic!("{}", fault.message));
    assert_eq!(delivery.prefix(&key(0), "hello world").ok(), Some(5));
}
