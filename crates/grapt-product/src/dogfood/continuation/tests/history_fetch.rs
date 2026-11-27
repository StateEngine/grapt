use super::*;

#[test]
fn history_fetch_ignores_listing_cursor_and_preserves_exact_output_scope() -> Result<(), String> {
    let fixture = Fixture::new("history-fetch-cursor")?;
    let output = format!("{}é-tail", "a".repeat(65_535));
    fixture.record_exchange_output(true, false, false, READ_TOOL, "read", output.as_bytes())?;
    let call = ToolCallId::new("call_d2a_read").ok_or("call")?;
    let fetch = |session: &SessionId, call: &ToolCallId, cursor, offset| {
        history(
            fixture.inbox()?,
            fixture.artifacts()?.as_ref(),
            session,
            Some(call),
            cursor,
            Some(offset),
        )
    };
    for offset in [0, 65_535] {
        let expected = fetch(&fixture.session, &call, None, offset)?;
        for cursor in [0, 12, u64::MAX] {
            assert_eq!(
                fetch(&fixture.session, &call, Some(cursor), offset)?,
                expected,
                "listing cursor changed output at byte {offset}"
            );
        }
        let page: serde_json::Value =
            serde_json::from_slice(&expected).map_err(|error| error.to_string())?;
        assert_eq!(page["offset"].as_u64(), Some(offset));
        assert_eq!(page["call"].as_str(), Some(call.as_str()));
        let expected_text = if offset == 0 {
            &output[..65_535]
        } else {
            "é-tail"
        };
        assert_eq!(page["content"].as_str(), Some(expected_text));
    }
    let unknown = ToolCallId::new("call_not_recorded").ok_or("unknown call")?;
    let foreign = "ses_foreign_history"
        .parse::<SessionId>()
        .map_err(|error| error.to_string())?;
    for cursor in [None, Some(0), Some(12)] {
        for (session, selected) in [(&fixture.session, &unknown), (&foreign, &call)] {
            let error = fetch(session, selected, cursor, 0)
                .err()
                .ok_or("unavailable call fell back to listing or disclosed output")?;
            assert!(error.contains("no settled call"), "{error}");
        }
        let error = fetch(&fixture.session, &call, cursor, 65_536)
            .err()
            .ok_or("fetch accepted a split UTF-8 code point")?;
        assert!(error.contains("UTF-8 boundary"), "{error}");
    }
    Ok(())
}
