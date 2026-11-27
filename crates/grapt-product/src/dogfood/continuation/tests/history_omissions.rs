//! Unreadable durable items stay visible as gaps through the public pager.
use super::*;

/// Corrupt one stored row of `kind`.
///
/// A kind suffix in `kind` names one exact version; without one every version
/// of that kind matches, which is what a fixture needs when the durable owner
/// chooses the version (an attempt's opening is v1 or v2 depending on whether
/// it carries a selection).
fn corrupt(fixture: &Fixture, kind: &str, order: &str) -> Result<(), String> {
    let connection =
        rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
    let changed = connection
        .execute(
            &format!(
                "UPDATE event SET data='{{' WHERE id=(SELECT id FROM event
             WHERE aggregate_family='session' AND aggregate_id=?1
               AND (type=?2 OR type GLOB ?2 || '.*')
             ORDER BY seq {order} LIMIT 1)"
            ),
            (fixture.session.as_str(), kind),
        )
        .map_err(|error| error.to_string())?;
    assert_eq!(changed, 1, "fixture did not corrupt exactly one item");
    Ok(())
}

#[test]
fn an_assistant_page_marks_unreadable_content() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-assistant-omission")?;
    corrupt(&fixture, "session.content.appended.1", "DESC")?;
    let (response, _) = page(&app, &fixture.session, json!({"limit":"200"}))?;
    // The gap is DATA on the row that lost the item, addressed by its durable
    // coordinate; the assistant's own content is untouched.
    let marked = response["data"]
        .as_array()
        .ok_or("page")?
        .iter()
        .any(|row| {
            row["metadata"]["omittedTotal"]
                .as_u64()
                .is_some_and(|n| n >= 1)
        });
    assert!(
        marked,
        "the public history silently omitted an assistant item: {response}"
    );
    Ok(())
}

#[test]
fn an_omission_does_not_rewrite_the_users_words() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-user-omission")?;
    corrupt(&fixture, "session.content.appended.1", "ASC")?;
    let (response, _) = page(
        &app,
        &fixture.session,
        json!({"limit":"200", "order":"asc"}),
    )?;
    assert_eq!(response["data"][0]["type"], "user");
    assert_eq!(response["data"][0]["text"], "use the tool");
    let marked = response["data"]
        .as_array()
        .ok_or("page")?
        .iter()
        .any(|row| {
            row["metadata"]["omittedTotal"]
                .as_u64()
                .is_some_and(|n| n >= 1)
        });
    assert!(marked, "no row named the omission: {response}");
    Ok(())
}

#[test]
fn an_unreadable_opening_spares_other_public_messages() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-opening-omission")?;
    corrupt(&fixture, "session.assistant-message.opened", "ASC")?;
    let (response, _) = page(
        &app,
        &fixture.session,
        json!({"limit":"200", "order":"asc"}),
    )?;
    // The unreadable opening cannot name itself, so it is not a row; the
    // messages around it still render, and the page names the gap.
    assert!(response.to_string().contains(&format!("answer {STEPS}")));
    let marked = response["data"]
        .as_array()
        .ok_or("page")?
        .iter()
        .any(|row| {
            row["metadata"]["omittedTotal"]
                .as_u64()
                .is_some_and(|n| n >= 1)
        });
    assert!(
        marked,
        "the unreadable opening carried no named gap: {response}"
    );
    Ok(())
}

#[test]
fn a_descending_page_does_not_mark_omissions_from_older_pages() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-descending-omission")?;
    corrupt(&fixture, "session.content.appended.1", "ASC")?;
    let latest: MessageId = "msg_after_omission"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(&app.inbox, &fixture.session, &latest, "latest input")?;
    let (response, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
    assert_eq!(response["data"][0]["id"], latest.as_str());
    assert_eq!(response["data"][0]["text"], "latest input");
    Ok(())
}

/// A descending page reports each gap on the row whose durable range owns it.
///
/// Rows come back newest first, so a range that took the NEXT RESPONSE ROW as
/// its upper bound gave the newest row `[newest, older)` — empty — and parked
/// every gap in the page on the oldest row. The gap is placed by durable
/// sequence here, and asserted through the public response.
#[test]
fn a_descending_page_places_each_gap_on_the_row_that_owns_it() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-descending-range")?;
    // Corrupt the NEWEST assistant content row, which a descending page returns
    // first: its own row must carry the marker, not an older one.
    corrupt(&fixture, "session.content.appended.1", "DESC")?;
    let (response, _) = page(&app, &fixture.session, json!({"limit":"2"}))?;
    let rows = response["data"].as_array().ok_or("page")?;
    assert_eq!(rows.len(), 2, "the fixture needs a two-row descending page");
    let marked = |row: &serde_json::Value| {
        row["metadata"]["omittedTotal"]
            .as_u64()
            .is_some_and(|total| total >= 1)
    };
    assert!(
        marked(&rows[0]),
        "the row whose content went missing carried no marker: {response}"
    );
    assert!(
        !marked(&rows[1]),
        "the older row absorbed a gap that is not in its range: {response}"
    );
    Ok(())
}
