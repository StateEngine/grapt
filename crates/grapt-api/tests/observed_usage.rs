use grapt_api::u3a::{SessionMessageInfoDto, TokenUsageObservationDto};

#[test]
fn observed_counts_preserve_absence_zero_and_safe_integer_bounds() -> Result<(), String> {
    for source in [
        "{}",
        "{\"cacheRead\":0}",
        "{\"inputTotal\":9007199254740991}",
    ] {
        let decoded: TokenUsageObservationDto =
            serde_json::from_str(source).map_err(|e| e.to_string())?;
        let roundtrip = serde_json::to_value(decoded).map_err(|e| e.to_string())?;
        let original: serde_json::Value =
            serde_json::from_str(source).map_err(|e| e.to_string())?;
        assert_eq!(roundtrip, original);
    }
    for field in [
        "inputTotal",
        "inputFresh",
        "output",
        "reasoning",
        "cacheRead",
        "cacheWrite",
        "total",
    ] {
        for value in [
            "null",
            "-1",
            "0.5",
            "9007199254740992",
            "18446744073709551615",
            "\"0\"",
        ] {
            let source = format!("{{\"{field}\":{value}}}");
            assert!(
                serde_json::from_str::<TokenUsageObservationDto>(&source).is_err(),
                "accepted {source}"
            );
        }
    }
    Ok(())
}

#[test]
fn old_and_partial_assistant_messages_decode_without_inventing_legacy_tokens() -> Result<(), String>
{
    let old = r#"{"type":"assistant","id":"msg_test","time":{"created":1.0},"agent":"build","model":{"id":"model","providerID":"openrouter"},"content":[]}"#;
    let mut value: serde_json::Value = serde_json::from_str(old).map_err(|e| e.to_string())?;
    let _: SessionMessageInfoDto = serde_json::from_str(old).map_err(|e| e.to_string())?;
    value["observedUsage"] =
        serde_json::json!({"inputTotal":19,"inputFresh":19,"output":3,"cacheRead":0});
    let encoded = format!(
        "{},\"observedUsage\":{}}}",
        old.trim_end_matches('}'),
        value["observedUsage"]
    );
    let decoded: SessionMessageInfoDto =
        serde_json::from_str(&encoded).map_err(|e| e.to_string())?;
    let actual = serde_json::to_value(decoded).map_err(|e| e.to_string())?;
    assert_eq!(actual, value);
    assert!(actual.get("tokens").is_none());
    assert!(actual["observedUsage"].get("cacheWrite").is_none());
    Ok(())
}
