use grapt_api::u3a::{ToolProgressEntry, ToolProgressMetadata, ToolProgressStatus};
use serde_json::json;

#[test]
fn progress_is_bounded_status_only_data() -> Result<(), Box<dyn std::error::Error>> {
    let row = ToolProgressEntry::new("fixture.add", ToolProgressStatus::Running)?;
    let metadata = ToolProgressMetadata::new(vec![row; 64])?;
    let value = serde_json::to_value(&metadata)?;
    assert_eq!(
        value["toolCalls"][0],
        json!({"tool":"fixture.add","status":"running"})
    );
    assert_eq!(
        serde_json::from_value::<ToolProgressMetadata>(value)?,
        metadata
    );
    assert!(ToolProgressMetadata::new(vec![
        ToolProgressEntry::new("x", ToolProgressStatus::Error)?;
        65
    ])
    .is_err());
    assert!(serde_json::from_value::<ToolProgressMetadata>(
        json!({"toolCalls":vec![json!({"tool":"x","status":"running"});65]})
    )
    .is_err());
    Ok(())
}

#[test]
fn malformed_names_status_and_private_fields_are_refused() {
    for row in [
        json!({"tool":"","status":"running"}),
        json!({"tool":"x".repeat(257),"status":"running"}),
        json!({"tool":"x","status":"unknown"}),
        json!({"tool":"x","status":"running","input":{"secret":"private"}}),
        json!({"tool":"x","status":"completed","output":"private"}),
    ] {
        assert!(serde_json::from_value::<ToolProgressEntry>(row).is_err());
    }
    assert!(
        serde_json::from_value::<ToolProgressMetadata>(json!({"toolCalls":[],"artifacts":[]}))
            .is_err()
    );
    assert!(ToolProgressEntry::new("", ToolProgressStatus::Running).is_err());
}

#[test]
fn retained_tool_lifecycle_has_typed_bounds_and_terminal_versions(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_api::u3a::*;
    let coordinates =
        json!({"sessionID":"ses_live","assistantMessageID":"msg_live","callID":"call_live"});
    let mut started = coordinates.clone();
    started["name"] = json!("execute");
    let data: ToolInputStartedData = serde_json::from_value(started.clone())?;
    let event = DogfoodEvent::ToolInputStarted(DurableEvent::new(
        WireId::parse("evt_live".into()).ok_or("event")?,
        FiniteF64::try_new(42.0)?,
        DurableEventRef::new(AggregateIdDto::try_new("ses_live")?, 3u64.into()),
        data,
    ));
    let wire = serde_json::to_value(event)?;
    assert_eq!(wire["type"], "session.tool.input.started");
    assert_eq!(wire["durable"]["version"], 1);
    started["name"] = json!("x".repeat(257));
    assert!(serde_json::from_value::<ToolInputStartedData>(started).is_err());
    let mut success = coordinates;
    success["content"] = json!([{"type":"text","text":"14"}]);
    success["metadata"] = json!({"toolCalls":[{"tool":"add","status":"completed"}]});
    success["executed"] = json!(false);
    let state = serde_json::from_str(
        r#"{"status":"completed","input":{},"content":[{"type":"text","text":"14"}],"metadata":{}}"#,
    )?;
    let data = ToolSuccessData::from_history(
        WireId::parse("ses_live".into()).ok_or("session")?,
        WireId::parse("msg_live".into()).ok_or("message")?,
        WireId::parse("call_live".into()).ok_or("call")?,
        state,
    )?;
    let event = DogfoodEvent::ToolSuccess(DurableEvent::new(
        WireId::parse("evt_done".into()).ok_or("event")?,
        FiniteF64::try_new(43.0)?,
        DurableEventRef::tool_terminal(AggregateIdDto::try_new("ses_live")?, 4u64.into()),
        data,
    ));
    let wire = serde_json::to_value(event)?;
    assert_eq!(wire["type"], "session.tool.success");
    assert_eq!(wire["durable"]["version"], 2);
    success["content"] = json!([]);
    assert!(serde_json::from_value::<ToolSuccessData>(success.clone()).is_err());
    success["content"] = json!([{"type":"text","text":"14"}]);
    success["privateArtifact"] = json!("secret");
    assert!(serde_json::from_value::<ToolSuccessData>(success).is_err());
    Ok(())
}

#[test]
fn admitted_execute_source_is_not_limited_by_metadata_scalars(
) -> Result<(), Box<dyn std::error::Error>> {
    use grapt_api::u3a::*;
    let called = |input: &str| {
        ToolCalledData::new(
            WireId::parse("ses_source".into()).ok_or("session")?,
            WireId::parse("msg_source".into()).ok_or("message")?,
            WireId::parse("call_source".into()).ok_or("call")?,
            input,
        )
    };
    let source = format!("/*{}*/return 14;", "x".repeat(65_521));
    assert_eq!(source.len(), 65_535);
    let input = json!({"code":source}).to_string();
    let event = called(&input)?;
    assert_eq!(serde_json::to_value(event)?["input"]["code"], source);
    assert!(called(&json!({"code":"x".repeat(256 * 1024)}).to_string()).is_err());
    assert!(called("[]").is_err());
    assert!(called(&format!("{{\"x\":{}{}}}", "[".repeat(33), "]".repeat(33))).is_err());
    let state: ToolResultProjection = serde_json::from_str(&format!(
        "{{\"status\":\"completed\",\"input\":{input},\"content\":[{{\"type\":\"text\",\"text\":\"14\"}}],\"metadata\":{{}}}}"
    ))?;
    let history: SessionMessageToolStateCompletedDto = serde_json::from_str(&format!(
        "{{\"status\":\"completed\",\"input\":{input},\"content\":[{{\"type\":\"text\",\"text\":\"14\"}}],\"metadata\":{{}}}}"
    ))?;
    assert_eq!(serde_json::to_value(history)?["input"]["code"], source);
    ToolSuccessData::from_history(
        WireId::parse("ses_source".into()).ok_or("session")?,
        WireId::parse("msg_source".into()).ok_or("message")?,
        WireId::parse("call_source".into()).ok_or("call")?,
        state,
    )?;
    Ok(())
}
