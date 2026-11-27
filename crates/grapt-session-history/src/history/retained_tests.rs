use super::*;

#[test]
fn target_tool_page_survives_accumulation_without_hiding_corruption(
) -> Result<(), Box<dyn std::error::Error>> {
    const BOUND: usize = crate::MAX_TOOL_CALL_PAGE_ROWS;
    let conn = stored(&[]);
    for ordinal in 0..BOUND + 2 {
        append_stored_tool_call(&conn, ordinal);
    }
    for name in ["call_page_0", "call_page_129"] {
        let call = ToolCallId::new(name).ok_or("call")?;
        let page = SessionHistoryStore::new(&conn).tool_call_page_for(&session(), &call)?;
        assert_eq!(page.rows().len(), 1);
        assert_eq!(page.rows()[0].call(), &call);
    }
    let absent = ToolCallId::new("absent").ok_or("call")?;
    assert!(SessionHistoryStore::new(&conn)
        .tool_call_page_for(&session(), &absent)?
        .rows()
        .is_empty());
    for malformed in [
        "{bad",
        r#"{"item":{"kind":"alien","call":"other"}}"#,
        r#"{"item":{"kind":"tool"}}"#,
    ] {
        conn.execute(
            "UPDATE event SET data = ?1 WHERE id = 'evt_tool_page_0'",
            [malformed],
        )?;
        assert!(matches!(
            SessionHistoryStore::new(&conn).tool_call_page_for(&session(), &absent),
            Err(HistoryReadError::Corrupt { .. })
        ));
    }
    Ok(())
}

#[test]
fn scc_retained_tool_page_bypasses_old_rows_without_raising_bound(
) -> Result<(), Box<dyn std::error::Error>> {
    const BOUND: usize = crate::MAX_TOOL_CALL_PAGE_ROWS;
    let conn = stored(&[]);
    for ordinal in 0..BOUND + 2 {
        append_stored_tool_call(&conn, ordinal);
    }
    let history = SessionHistoryStore::new(&conn);
    assert!(matches!(
        history.tool_call_page(&session()),
        Err(HistoryReadError::ToolCallPageTooLong { .. })
    ));
    let page = history.tool_call_page_from(&session(), grapt_types::EventSeq::new(2))?;
    assert_eq!(page.rows().len(), BOUND);
    assert_eq!(page.rows()[0].call().as_str(), "call_page_2");
    assert!(matches!(
        history.tool_call_page_from(&session(), grapt_types::EventSeq::new(1)),
        Err(HistoryReadError::ToolCallPageTooLong { .. })
    ));
    assert!(history
        .tool_call_page_from(&session(), grapt_types::EventSeq::new(BOUND as u64 + 2))?
        .rows()
        .is_empty());
    assert!(history
        .tool_call_page_from(&session(), grapt_types::EventSeq::new(u64::MAX))
        .is_err());
    conn.execute("UPDATE event SET seq = -1 WHERE id = 'evt_tool_page_0'", [])?;
    assert!(matches!(
        history.tool_call_page_from(&session(), grapt_types::EventSeq::new(2)),
        Err(HistoryReadError::ToolCallPageTooLong { .. })
    ));
    assert!(matches!(
        history.tool_call_page_from(&session(), grapt_types::EventSeq::new(1_281)),
        Err(HistoryReadError::Corrupt { seq: -1, .. })
    ));
    Ok(())
}

#[test]
fn scc_retained_fold_excludes_prior_rows_and_names_selected_corruption(
) -> Result<(), Box<dyn std::error::Error>> {
    let conn = stored(&[
        (0, "session.content.appended.1", "{bad"),
        (1, "unowned.fact.1", "{}"),
    ]);
    let history = SessionHistoryStore::new(&conn);
    // The bad row is a named gap, not a refused read: a window that excludes it
    // is clean, and one that includes it says only that it was omitted.
    assert_eq!(history.fold(&session())?.omissions().len(), 1);
    assert!(history
        .fold_from(&session(), grapt_types::EventSeq::new(1))?
        .messages()?
        .is_empty());
    assert_eq!(
        history
            .fold_from(&session(), grapt_types::EventSeq::FIRST)?
            .omissions()
            .len(),
        1
    );
    conn.execute("UPDATE event SET seq = -1 WHERE seq = 0", [])?;
    assert!(matches!(
        history.fold_from(&session(), grapt_types::EventSeq::new(1)),
        Err(HistoryReadError::Corrupt { seq: -1, .. })
    ));
    Ok(())
}
