//! A corrupt session-list row is skipped and named; the healthy rows around
//! it are still listed and paged.
use grapt_session::{
    ParentFilter, SessionListDirection, SessionListError, SessionListLimits, SessionListOrder,
    SessionListScope, SessionListStore,
};
use grapt_store::EventStore;
use grapt_types::SessionId;

mod session_list_support;
use session_list_support::{criteria, insert};

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn one_corrupt_session_leaves_every_healthy_session_listed() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for (id, updated) in [("ses_a", 10), ("ses_b", 20), ("ses_c", 30), ("ses_d", 40)] {
        insert(
            store.connection(),
            id,
            updated,
            "prj_a",
            None,
            None,
            "/work/a",
            "ok",
        )?;
    }
    // Three corrupt rows sort ahead of every healthy one: a page of size 2
    // must read past all of them rather than return an empty first page.
    for (id, updated) in [("ses_x", 90), ("ses_y", 80), ("not a session id", 70)] {
        insert(
            store.connection(),
            id,
            updated,
            "prj_a",
            None,
            None,
            "/work/a",
            "bad",
        )?;
    }
    store.connection().execute_batch(
        "UPDATE session_list_projection SET provenance_digest = 'tampered'
             WHERE session_id IN ('ses_x', 'ses_y')",
    )?;
    let request = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Descending,
        SessionListDirection::Next,
        None,
        2,
        SessionListLimits::production(),
    )?;
    let page = SessionListStore::over(&mut store).list(&request)?;
    let ids: Vec<&str> = page
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str())
        .collect();
    assert_eq!(ids, ["ses_d", "ses_c"]);
    let degraded: Vec<Option<&str>> = page
        .degraded()
        .iter()
        .map(|row| row.session_id.as_ref().map(SessionId::as_str))
        .collect();
    assert_eq!(degraded, [Some("ses_x"), Some("ses_y"), None]);
    Ok(())
}

/// More corrupt rows than one page may skip still page through to the
/// healthy Session behind them, each page continuing after what it skipped.
#[test]
fn a_run_longer_than_the_skip_bound_continues_on_the_next_page() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_ok",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "ok",
    )?;
    for index in 0..130 {
        let id = format!("ses_bad{index:03}");
        insert(
            store.connection(),
            &id,
            1_000 + index,
            "prj_a",
            None,
            None,
            "/work/a",
            "bad",
        )?;
    }
    store.connection().execute_batch(
        "UPDATE session_list_projection SET provenance_digest = 'tampered'
             WHERE session_id <> 'ses_ok'",
    )?;
    let page = |anchor| {
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            anchor,
            2,
            SessionListLimits::production(),
        )
    };
    let first = SessionListStore::over(&mut store).list(&page(None)?)?;
    assert!(first.items().is_empty());
    assert_eq!(first.degraded().len(), grapt_session::list::MAX_PAGE_ROWS);
    let after = first.continue_after().cloned().ok_or("no continuation")?;
    let second = SessionListStore::over(&mut store).list(&page(Some(after))?)?;
    let ids: Vec<&str> = second
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str())
        .collect();
    assert_eq!(ids, ["ses_ok"]);
    assert_eq!(second.degraded().len(), 2);
    assert!(second.continue_after().is_none(), "the list ended");
    Ok(())
}

#[test]
fn a_store_with_a_corrupt_session_row_still_opens_and_lists() -> TestResult {
    let dir = std::env::temp_dir().join(format!("grapt-list-degrade-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("store.sqlite3");
    {
        let store = EventStore::open(&db, "0.0.1")?;
        insert(
            store.connection(),
            "ses_ok",
            10,
            "prj_a",
            None,
            None,
            "/work/a",
            "ok",
        )?;
        insert(
            store.connection(),
            "ses_bad",
            20,
            "prj_a",
            None,
            None,
            "/work/a",
            "bad",
        )?;
        store.connection().execute(
            "UPDATE session_list_projection SET provenance_digest = 'tampered'
                 WHERE session_id = 'ses_bad'",
            [],
        )?;
    }
    let mut reopened = EventStore::open(&db, "0.0.1")?;
    let request = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Descending,
        SessionListDirection::Next,
        None,
        10,
        SessionListLimits::production(),
    )?;
    let page = SessionListStore::over(&mut reopened).list(&request)?;
    assert_eq!(page.items().len(), 1);
    assert_eq!(page.items()[0].key.session_id().as_str(), "ses_ok");
    assert_eq!(page.degraded().len(), 1);
    drop(reopened);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

/// Rows whose identities no longer parse still position the next page, so a
/// run of them longer than the skip bound does not end the list early.
#[test]
fn a_run_of_unparseable_identities_still_pages_to_the_healthy_session() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_ok",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "ok",
    )?;
    for index in 0..130 {
        insert(
            store.connection(),
            &format!("not a session {index:03}"),
            1_000 + index,
            "prj_a",
            None,
            None,
            "/work/a",
            "bad",
        )?;
    }
    let page = |anchor| {
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            anchor,
            2,
            SessionListLimits::production(),
        )
    };
    let first = SessionListStore::over(&mut store).list(&page(None)?)?;
    assert!(first.items().is_empty());
    assert_eq!(first.degraded().len(), grapt_session::list::MAX_PAGE_ROWS);
    let after = first.continue_after().cloned().ok_or("no continuation")?;
    let second = SessionListStore::over(&mut store).list(&page(Some(after))?)?;
    let ids: Vec<&str> = second
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str())
        .collect();
    assert_eq!(ids, ["ses_ok"]);
    assert_eq!(second.degraded().len(), 2);
    Ok(())
}

/// Every page of `order`, two Sessions at a time, as listed ids.
fn every_page(
    store: &mut EventStore,
    order: SessionListOrder,
) -> Result<Vec<String>, Box<dyn std::error::Error>> {
    let mut listed = Vec::new();
    let mut anchor = None;
    for _ in 0..8 {
        let request = criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            order,
            SessionListDirection::Next,
            anchor.clone(),
            2,
            SessionListLimits::production(),
        )?;
        let page = SessionListStore::over(store).list(&request)?;
        listed.extend(
            page.items()
                .iter()
                .map(|item| item.key.session_id().as_str().to_owned()),
        );
        anchor = match (page.continue_after(), page.items().last()) {
            (Some(after), _) => Some(after.clone()),
            (None, Some(last)) => Some((&last.key).into()),
            (None, None) => break,
        };
    }
    Ok(listed)
}

#[test]
fn rows_without_a_key_sharing_a_time_are_passed_in_both_directions() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for id in ["ses_a", "ses_b"] {
        insert(
            store.connection(),
            id,
            10,
            "prj_a",
            None,
            None,
            "/work/a",
            "ok",
        )?;
    }
    for _ in 0..3 {
        store.connection().execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES (NULL, 'prj_a', '/work/a', 'keyless', 10, 'legacy-baseline', 'digest', -1)",
            [],
        )?;
    }
    for order in [SessionListOrder::Ascending, SessionListOrder::Descending] {
        let mut listed = every_page(&mut store, order)?;
        listed.sort();
        assert_eq!(listed, ["ses_a", "ses_b"], "{order:?}");
    }
    Ok(())
}

#[test]
fn a_cursor_whose_row_now_holds_another_key_is_stale() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for index in 0..130 {
        insert(
            store.connection(),
            &format!("not a session {index:03}"),
            1_000,
            "prj_a",
            None,
            None,
            "/work/a",
            "bad",
        )?;
    }
    let request = |anchor| {
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            anchor,
            2,
            SessionListLimits::production(),
        )
    };
    let first = SessionListStore::over(&mut store).list(&request(None)?)?;
    let after = first.continue_after().cloned().ok_or("no continuation")?;
    let grapt_session::BoundaryKey::Row { rowid, .. } = after.after() else {
        return Err("the continuation is not after a stored row".into());
    };
    // The row goes, and its rowid comes back at the same time with a new key.
    store.connection().execute(
        "UPDATE session_list_projection SET session_id = 'not a session reused'
           WHERE rowid = ?1",
        [rowid],
    )?;
    let stale = SessionListStore::over(&mut store).list(&request(Some(after))?);
    assert!(
        matches!(stale, Err(SessionListError::StaleCursor)),
        "a reused row passed for the cursor's: {stale:?}"
    );
    Ok(())
}

#[test]
fn a_page_inside_a_long_run_sharing_a_time_stays_inside_the_step_budget() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.connection().execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 69999)
         INSERT INTO session_list_legacy_baseline
            (session_id, updated_ms, source_domain, source_digest, cutover_seq)
         SELECT printf('ses_%06d', i), 5, 'synthetic-fixture', 'digest', -1 FROM n",
        [],
    )?;
    store.connection().execute(
        "WITH RECURSIVE n(i) AS (SELECT 0 UNION ALL SELECT i + 1 FROM n WHERE i < 69999)
         INSERT INTO session_list_projection
            (session_id, project_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         SELECT printf('ses_%06d', i), 'prj_a', '/work/a', 'ok', 5,
                'legacy-baseline', 'digest', -1 FROM n",
        [],
    )?;
    let anchor = grapt_session::SessionListKey::new(
        grapt_session::NonNegativeMs::new(5).ok_or("time")?,
        "ses_035000".parse()?,
    );
    for (order, expected) in [
        (SessionListOrder::Ascending, ["ses_035001", "ses_035002"]),
        (SessionListOrder::Descending, ["ses_034999", "ses_034998"]),
    ] {
        let request = criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            order,
            SessionListDirection::Next,
            Some((&anchor).into()),
            2,
            SessionListLimits::production(),
        )?;
        let page = SessionListStore::over(&mut store).list(&request)?;
        let ids: Vec<_> = page
            .items()
            .iter()
            .map(|item| item.key.session_id().as_str().to_owned())
            .collect();
        assert_eq!(ids, expected, "{order:?}");
    }
    Ok(())
}

#[test]
fn a_cursor_whose_oversized_key_changed_past_the_row_bound_is_stale() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_ok",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "ok",
    )?;
    let key = |index: u32| {
        let mut key = vec![0_u8; 300_000];
        key[..4].copy_from_slice(&index.to_be_bytes());
        key
    };
    for index in 0..130 {
        store.connection().execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES (?1, 'prj_a', '/work/a', 'big', 1000, 'legacy-baseline', 'digest', -1)",
            [key(index)],
        )?;
    }
    let request = |anchor| {
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            anchor,
            2,
            SessionListLimits::production(),
        )
    };
    let first = SessionListStore::over(&mut store).list(&request(None)?)?;
    let after = first.continue_after().cloned().ok_or("no continuation")?;
    let grapt_session::BoundaryKey::Row { rowid, .. } = after.after() else {
        return Err("the continuation is not after a stored row".into());
    };
    // Same type and length; one byte differs past the row text bound.
    let mut changed: Vec<u8> = store.connection().query_row(
        "SELECT session_id FROM session_list_projection WHERE rowid = ?1",
        [rowid],
        |row| row.get(0),
    )?;
    changed[262_144] = 0xFF;
    store.connection().execute(
        "UPDATE session_list_projection SET session_id = ?1 WHERE rowid = ?2",
        (changed, rowid),
    )?;
    let stale = SessionListStore::over(&mut store).list(&request(Some(after))?);
    assert!(
        matches!(stale, Err(SessionListError::StaleCursor)),
        "a changed oversized key passed for the cursor's: {stale:?}"
    );
    Ok(())
}

#[test]
fn a_run_of_rows_whose_time_does_not_read_still_continues() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_ok",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "ok",
    )?;
    // Such rows break the table's CHECK; another writer or a disk can still
    // leave them.
    store
        .connection()
        .execute_batch("PRAGMA ignore_check_constraints = ON")?;
    for index in 0..130 {
        store.connection().execute(
            "INSERT INTO session_list_projection
                (session_id, project_id, directory, title, updated_ms,
                 provenance_kind, provenance_digest, cutover_seq)
             VALUES (?1, 'prj_a', '/work/a', 'bad', 'not a time',
                     'legacy-baseline', 'digest', -1)",
            [format!("ses_bad_time_{index:03}")],
        )?;
    }
    store
        .connection()
        .execute_batch("PRAGMA ignore_check_constraints = OFF")?;
    for order in [SessionListOrder::Descending, SessionListOrder::Ascending] {
        assert_eq!(every_page(&mut store, order)?, ["ses_ok"], "{order:?}");
    }
    Ok(())
}

#[test]
fn a_search_never_reads_a_title_over_the_row_bound() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let oversized = format!("find {}", "x".repeat(300_000));
    for (id, title) in [("ses_ok", "find me"), ("ses_big", oversized.as_str())] {
        insert(
            store.connection(),
            id,
            1,
            "prj_a",
            None,
            None,
            "/work/a",
            title,
        )?;
    }
    let request = criteria(
        SessionListScope::All,
        None,
        Some("find"),
        ParentFilter::Any,
        SessionListOrder::Descending,
        SessionListDirection::Next,
        None,
        10,
        SessionListLimits::production(),
    )?;
    let page = SessionListStore::over(&mut store).list(&request)?;
    let ids: Vec<_> = page
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str())
        .collect();
    assert_eq!(ids, ["ses_ok"]);
    assert!(
        page.degraded().is_empty(),
        "the oversized title was read to match the search"
    );
    Ok(())
}
