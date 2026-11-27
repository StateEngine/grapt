//! Pending response rows must decode through the same tagged union they encode.
use grapt_api::u3a::*;
use grapt_schema::Required;
use serde_json::json;

#[test]
fn pending_user_synthetic_and_compaction_rows_round_trip() -> Result<(), Box<dyn std::error::Error>>
{
    let common = json!({"admittedSeq":1,"id":"msg_pending_roundtrip", "sessionID":"ses_pending_roundtrip","timeCreated":1});
    let mut user = common.clone();
    user["type"] = json!("user");
    user["data"] = json!({"text":"user work"});
    user["delivery"] = json!("queue");
    let user: SessionPendingUserDto = serde_json::from_value(user)?;
    let mut synthetic = common.clone();
    synthetic["type"] = json!("synthetic");
    synthetic["data"] = json!({"text":"synthetic work"});
    synthetic["delivery"] = json!("steer");
    let synthetic: SessionPendingSyntheticDto = serde_json::from_value(synthetic)?;
    let mut compaction = common;
    compaction["type"] = json!("compaction");
    let compaction: SessionPendingCompactionDto = serde_json::from_value(compaction)?;
    let rows = BoundedVec::try_new(vec![
        SessionPendingInfoDto::User(Box::new(user)),
        SessionPendingInfoDto::Synthetic(Box::new(synthetic)),
        SessionPendingInfoDto::Compaction(Box::new(compaction)),
    ])?;
    let page = SessionPendingListResponseDto::new(Required::new(rows));
    let bytes = encode_page(&page, U3aLimits::production()?)?;
    let decoded: SessionPendingListResponseDto = decode_page(&bytes, U3aLimits::production()?)?;
    assert_eq!(decoded, page);
    Ok(())
}
