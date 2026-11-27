use grapt_inbox::{InboxEvent, InboxFold};
use grapt_types::{AdmissionSeq, MessageId};
use serde_json::json;

fn admitted(id: &str) -> Result<InboxEvent, Box<dyn std::error::Error>> {
    Ok(InboxEvent::decode(
        "session.input.admitted",
        1,
        &json!({"sessionID":"ses_s","inputID":id,
                "input":{"type":"user","delivery":"steer"}}),
    )?
    .ok_or("admitted event")?)
}

fn promoted(id: &str) -> Result<InboxEvent, Box<dyn std::error::Error>> {
    Ok(InboxEvent::decode(
        "session.input.promoted",
        1,
        &json!({"sessionID":"ses_s","inputID":id}),
    )?
    .ok_or("promoted event")?)
}

#[test]
fn promotion_clears_the_row_in_place() -> Result<(), Box<dyn std::error::Error>> {
    let mut fold = InboxFold::default();
    fold.apply(admitted("msg_a")?, 4);
    assert_eq!(fold.pending.len(), 1);
    fold.apply(promoted("msg_a")?, 5);
    assert!(fold.pending.is_empty());
    assert!(fold.violations.is_empty());
    assert!(fold.unmatched_promotions.is_empty());
    Ok(())
}

#[test]
fn admitted_seq_is_the_events_own_durable_seq() -> Result<(), Box<dyn std::error::Error>> {
    let mut fold = InboxFold::default();
    fold.apply(admitted("msg_a")?, 4267);
    let message: MessageId = "msg_a".parse()?;
    let row = &fold.pending[&message];
    assert_eq!(row.admitted_seq, AdmissionSeq::new(4267));
    Ok(())
}

#[test]
fn unmatched_promotion_is_measured_not_fatal() -> Result<(), Box<dyn std::error::Error>> {
    let mut fold = InboxFold::default();
    fold.apply(promoted("msg_ghost")?, 9);
    assert_eq!(fold.unmatched_promotions, vec!["msg_ghost"]);
    assert!(fold.violations.is_empty());
    Ok(())
}
