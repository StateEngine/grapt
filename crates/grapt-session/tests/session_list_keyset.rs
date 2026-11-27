use grapt_session::{
    BoundedSearch, NonNegativeMs, ParentFilter, SessionListCriteria, SessionListDirection,
    SessionListError, SessionListKey, SessionListLimits, SessionListOrder, SessionListScope,
    SessionListStore,
};
use grapt_store::EventStore;
use grapt_types::{LocationRef, ProjectId, SessionId, WorkspaceId};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn session(raw: &str) -> Result<SessionId, Box<dyn std::error::Error>> {
    Ok(raw.parse()?)
}

fn project(raw: &str) -> Result<ProjectId, Box<dyn std::error::Error>> {
    ProjectId::new(raw).ok_or_else(|| "project id must be nonempty".into())
}

#[allow(clippy::too_many_arguments)]
fn insert(
    conn: &Connection,
    id: &str,
    updated_ms: i64,
    project_id: &str,
    workspace_id: Option<&str>,
    parent_id: Option<&str>,
    directory: &str,
    title: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO session_list_legacy_baseline
            (session_id, updated_ms, source_domain, source_digest, cutover_seq)
         VALUES (?1, ?2, 'synthetic-fixture', 'digest', -1)",
        rusqlite::params![id, updated_ms],
    )?;
    conn.execute(
        "INSERT INTO session_list_projection
            (session_id, project_id, workspace_id, parent_id, directory, title, updated_ms,
             provenance_kind, provenance_digest, cutover_seq)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 'legacy-baseline', 'digest', -1)",
        rusqlite::params![
            id,
            project_id,
            workspace_id,
            parent_id,
            directory,
            title,
            updated_ms
        ],
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn criteria(
    scope: SessionListScope,
    workspace: Option<WorkspaceId>,
    search: Option<&str>,
    parent: ParentFilter,
    order: SessionListOrder,
    direction: SessionListDirection,
    anchor: Option<SessionListKey>,
    page_size: usize,
    limits: SessionListLimits,
) -> Result<SessionListCriteria, SessionListError> {
    let title_contains = search
        .map(|value| BoundedSearch::new(value, limits))
        .transpose()?;
    SessionListCriteria::new(
        scope,
        workspace,
        title_contains,
        parent,
        order,
        direction,
        anchor,
        page_size,
        limits,
    )
}

fn ids(
    view: &mut SessionListStore<'_>,
    criteria: &SessionListCriteria,
) -> Result<Vec<String>, SessionListError> {
    Ok(view
        .list(criteria)?
        .items()
        .iter()
        .map(|item| item.key.session_id().as_str().to_owned())
        .collect())
}

#[test]
fn directory_scope_reconciles_windows_spelling_without_changing_posix_identity() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let rows = [
        ("ses_windows_raw", r"C:\workspace\repo"),
        ("ses_windows_slash", "C:/workspace/repo"),
        ("ses_windows_mixed", r"C:\workspace/repo"),
        ("ses_windows_case", "C:/WORKSPACE/repo"),
        ("ses_posix_literal", r"/work/a\b"),
        ("ses_posix_nested", "/work/a/b"),
    ];
    for (id, directory) in rows {
        insert(
            store.connection(),
            id,
            10,
            "prj_a",
            None,
            None,
            directory,
            id,
        )?;
    }
    let mut view = SessionListStore::over(&mut store);
    for (platform, directory, expected) in [
        (
            grapt_types::Platform::Windows,
            r"C:\workspace\repo",
            vec!["ses_windows_mixed", "ses_windows_raw", "ses_windows_slash"],
        ),
        (
            grapt_types::Platform::Posix,
            r"/work/a\b",
            vec!["ses_posix_literal"],
        ),
        (
            grapt_types::Platform::Posix,
            "/work/a/b",
            vec!["ses_posix_nested"],
        ),
    ] {
        let query = criteria(
            SessionListScope::Directory(LocationRef::on(platform, directory, None)?),
            None,
            None,
            ParentFilter::Root,
            SessionListOrder::Ascending,
            SessionListDirection::Next,
            None,
            20,
            SessionListLimits::production(),
        )?;
        assert_eq!(ids(&mut view, &query)?, expected);
    }
    // Querying must not rewrite the original evidence spelling.
    let stored: String = store.connection().query_row(
        "SELECT directory FROM session_list_projection WHERE session_id = 'ses_windows_raw'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(stored, r"C:\workspace\repo");
    Ok(())
}

fn tied_store() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for (id, time) in [
        ("ses_a", 10),
        ("ses_b", 10),
        ("ses_c", 10),
        ("ses_d", 10),
        ("ses_e", 20),
    ] {
        insert(
            store.connection(),
            id,
            time,
            "prj_a",
            None,
            None,
            "/work/a",
            id,
        )?;
    }
    let mut view = SessionListStore::over(&mut store);
    let limits = SessionListLimits::production();

    for (order, expected) in [
        (
            SessionListOrder::Ascending,
            vec!["ses_a", "ses_b", "ses_c", "ses_d", "ses_e"],
        ),
        (
            SessionListOrder::Descending,
            vec!["ses_e", "ses_d", "ses_c", "ses_b", "ses_a"],
        ),
    ] {
        for page_size in [1, 2, 3] {
            let mut actual = Vec::new();
            let mut anchor = None;
            loop {
                let page = view.list(&criteria(
                    SessionListScope::All,
                    None,
                    None,
                    ParentFilter::Any,
                    order,
                    SessionListDirection::Next,
                    anchor,
                    page_size,
                    limits,
                )?)?;
                if page.items().is_empty() {
                    break;
                }
                actual.extend(
                    page.items()
                        .iter()
                        .map(|item| item.key.session_id().as_str().to_owned()),
                );
                anchor = page.items().last().map(|item| item.key.clone());
            }
            assert_eq!(actual, expected, "order={order:?} page_size={page_size}");
        }
    }

    // An anchor is a key, not a foreign key: this identity is absent but lies
    // between `ses_b` and `ses_c` under the equal timestamp.
    let between = SessionListKey::new(NonNegativeMs::new(10).ok_or("time")?, session("ses_bb")?);
    let page = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        Some(between),
        3,
        limits,
    )?;
    assert_eq!(ids(&mut view, &page)?, ["ses_c", "ses_d", "ses_e"]);
    Ok(())
}

#[test]
fn equal_time_next_pages_are_strict_total_orders_without_gaps() -> TestResult {
    tied_store()
}

#[test]
fn previous_inverts_sql_and_reverses_back_to_requested_order() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for id in ["ses_a", "ses_b", "ses_c", "ses_d", "ses_e"] {
        insert(
            store.connection(),
            id,
            10,
            "prj_a",
            None,
            None,
            "/work/a",
            id,
        )?;
    }
    let mut view = SessionListStore::over(&mut store);
    let limits = SessionListLimits::production();
    let anchor = SessionListKey::new(NonNegativeMs::new(10).ok_or("time")?, session("ses_d")?);
    let ascending = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Previous,
        Some(anchor),
        2,
        limits,
    )?;
    assert_eq!(ids(&mut view, &ascending)?, ["ses_b", "ses_c"]);

    let anchor = SessionListKey::new(NonNegativeMs::new(10).ok_or("time")?, session("ses_b")?);
    let descending = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Descending,
        SessionListDirection::Previous,
        Some(anchor),
        2,
        limits,
    )?;
    assert_eq!(ids(&mut view, &descending)?, ["ses_d", "ses_c"]);

    let before_start = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Previous,
        Some(SessionListKey::new(
            NonNegativeMs::new(10).ok_or("time")?,
            session("ses_a")?,
        )),
        2,
        limits,
    )?;
    assert!(ids(&mut view, &before_start)?.is_empty());
    Ok(())
}

#[test]
fn scopes_and_filters_compose_with_and_and_keep_root_distinct_from_any() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_a",
        1,
        "prj_a",
        Some("ws_a"),
        None,
        "/a",
        "needle one",
    )?;
    insert(
        store.connection(),
        "ses_b",
        2,
        "prj_a",
        Some("ws_a"),
        Some("ses_a"),
        "/a",
        "needle two",
    )?;
    insert(
        store.connection(),
        "ses_c",
        3,
        "prj_b",
        None,
        None,
        "/b",
        "other",
    )?;
    insert(
        store.connection(),
        "ses_d",
        4,
        "prj_b",
        None,
        None,
        "/b",
        "needle outsider",
    )?;
    let mut view = SessionListStore::over(&mut store);
    let limits = SessionListLimits::production();
    let workspace = WorkspaceId::new("ws_a").ok_or("workspace")?;
    let location = LocationRef::new("/a", Some(workspace.clone()))?;
    let filtered = criteria(
        SessionListScope::Directory(location),
        Some(workspace),
        Some("needle"),
        ParentFilter::Root,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        10,
        limits,
    )?;
    assert_eq!(ids(&mut view, &filtered)?, ["ses_a"]);

    let project_id = project("prj_a")?;
    let child = criteria(
        SessionListScope::project(project_id.clone(), Some("display/one")),
        None,
        None,
        ParentFilter::Child(session("ses_a")?),
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        10,
        limits,
    )?;
    assert_eq!(ids(&mut view, &child)?, ["ses_b"]);
    let varied_subpath = criteria(
        SessionListScope::project(project_id, Some("display/two")),
        None,
        None,
        ParentFilter::Child(session("ses_a")?),
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        10,
        limits,
    )?;
    assert_eq!(ids(&mut view, &varied_subpath)?, ["ses_b"]);
    Ok(())
}

#[test]
fn criteria_text_page_and_vm_bounds_refuse_instead_of_clamping_or_skipping() -> TestResult {
    let production = SessionListLimits::production();
    assert!(matches!(
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            None,
            0,
            production
        ),
        Err(SessionListError::PageLimitExceeded { actual: 0, .. })
    ));
    assert!(matches!(
        criteria(
            SessionListScope::All,
            None,
            None,
            ParentFilter::Any,
            SessionListOrder::Descending,
            SessionListDirection::Next,
            None,
            129,
            production
        ),
        Err(SessionListError::PageLimitExceeded { actual: 129, .. })
    ));
    assert!(matches!(
        BoundedSearch::new("é".repeat(513), production),
        Err(SessionListError::SearchLimitExceeded { actual: 1026, .. })
    ));

    let mut store = EventStore::open_in_memory()?;
    for index in 0..50 {
        insert(
            store.connection(),
            &format!("ses_{index:03}"),
            index,
            "prj_a",
            None,
            None,
            "/work/a",
            "text",
        )?;
    }
    let one = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        1,
        production,
    )?;
    assert_eq!(
        SessionListStore::over(&mut store).list(&one)?.items().len(),
        1
    );
    let tiny = SessionListLimits::new(128, 1024, 262_144, 8_388_608, 1)?;
    let bounded = criteria(
        SessionListScope::All,
        None,
        Some("absent"),
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        1,
        tiny,
    )?;
    assert!(matches!(
        SessionListStore::over(&mut store).list(&bounded),
        Err(SessionListError::QueryBudgetExceeded)
    ));
    Ok(())
}

/// HOLD-4: every SQLite read a list request performs is under the request's
/// declared VM-step budget, and the request's cost does not grow with the
/// projection.
///
/// Both arms use the SAME hostile large clean projection. A clean projection is
/// the expensive case for the whole-table integrity scan on purpose: the scan
/// is `LIMIT 1` over a WHERE that no healthy row satisfies, so it stops early
/// only when something is already wrong and otherwise reads every row.
#[test]
fn a_hostile_large_clean_projection_is_bounded_and_costs_no_whole_table_scan() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for index in 0..4_000 {
        insert(
            store.connection(),
            &format!("ses_{index:06}"),
            i64::from(index),
            "prj_a",
            None,
            None,
            "/work/a",
            "text",
        )?;
    }

    // Arm 1 — no pre-budget scan. This allowance is ample for the shape
    // verification and one indexed page, and far below a whole-table joined
    // pass over 4,000 rows. The request must SUCCEED: if whole-projection
    // verification still ran on the request path it would exhaust here, and if
    // it ran outside the budget it would be unbounded work the bound does not
    // cover. Either way the defect is visible from this one assertion.
    let scan_free = SessionListLimits::new(128, 1024, 262_144, 8_388_608, 60_000)?;
    let page = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        4,
        scan_free,
    )?;
    // Deliberately NOT `?`. Under the mutant that puts whole-projection
    // verification back on the request path this call returns
    // QueryBudgetExceeded, and an Err carried out through `?` gives the mutation
    // harness no diagnostic to record. Naming the refusal keeps this row's
    // evidence distinct from every other row that fails by returning Err.
    let served = match SessionListStore::over(&mut store).list(&page) {
        Ok(page) => page,
        Err(error) => panic!(
            "a bounded page over a large clean projection must be served, not refused: {error:?}"
        ),
    };
    assert_eq!(
        served.items().len(),
        4,
        "a bounded page over a large clean projection must still be served"
    );

    // Arm 2 — the bound is live and covers the request. The same projection
    // under a one-step allowance exhausts deterministically and is reported as
    // a budget refusal, never as a corrupt or empty projection.
    let exhausting = SessionListLimits::new(128, 1024, 262_144, 8_388_608, 1)?;
    let starved = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        4,
        exhausting,
    )?;
    match SessionListStore::over(&mut store).list(&starved) {
        Err(SessionListError::QueryBudgetExceeded) => {}
        other => panic!("a starved request must be a typed budget refusal, got {other:?}"),
    }
    Ok(())
}

#[test]
fn unreadable_rows_and_missing_indexes_are_corruption_not_empty_pages() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_big",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "oversized-title",
    )?;
    let tiny = SessionListLimits::new(8, 8, 8, 64, 10_000)?;
    let request = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        1,
        tiny,
    )?;
    match SessionListStore::over(&mut store).list(&request) {
        Err(SessionListError::ProjectionCorrupt { reason }) => {
            assert!(reason.contains("over the configured row bound"), "{reason}");
        }
        other => panic!("oversized title must refuse before decode, got {other:?}"),
    }

    store
        .connection()
        .execute_batch("DROP INDEX session_list_updated_key_idx")?;
    let production = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        1,
        SessionListLimits::production(),
    )?;
    match SessionListStore::over(&mut store).list(&production) {
        Err(SessionListError::ProjectionCorrupt { reason }) => {
            assert!(reason.contains("session_list_updated_key_idx"), "{reason}");
        }
        other => panic!("missing required index must be typed corruption, got {other:?}"),
    }
    store.connection().execute_batch(
        "CREATE INDEX session_list_updated_key_idx
             ON session_list_projection (updated_ms, session_id COLLATE NOCASE)",
    )?;
    match SessionListStore::over(&mut store).list(&production) {
        Err(SessionListError::ProjectionCorrupt { reason }) => {
            assert!(reason.contains("collations"), "{reason}");
        }
        other => panic!("wrong required-index collation must be corruption, got {other:?}"),
    }
    Ok(())
}

// One exact witness identity per SC mutant keeps the mutation ledger's
// attribution records distinct even where several mutations attack different
// parts of the same end-to-end scenario.
#[test]
fn sc04_session_id_tie_break_is_load_bearing() -> TestResult {
    equal_time_next_pages_are_strict_total_orders_without_gaps()
}
#[test]
fn sc05_anchor_comparisons_are_strict() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    for id in ["ses_a", "ses_b", "ses_c", "ses_d"] {
        insert(
            store.connection(),
            id,
            10,
            "prj_a",
            None,
            None,
            "/work/a",
            id,
        )?;
    }
    let request = criteria(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        Some(SessionListKey::new(
            NonNegativeMs::new(10).ok_or("time")?,
            session("ses_b")?,
        )),
        2,
        SessionListLimits::production(),
    )?;
    assert_eq!(
        ids(&mut SessionListStore::over(&mut store), &request)?,
        ["ses_c", "ses_d"]
    );
    Ok(())
}
#[test]
fn sc06_timestamp_and_id_share_one_direction() -> TestResult {
    equal_time_next_pages_are_strict_total_orders_without_gaps()
}
#[test]
fn sc07_previous_inverts_physical_order() -> TestResult {
    previous_inverts_sql_and_reverses_back_to_requested_order()
}
#[test]
fn sc08_previous_reverses_physical_results() -> TestResult {
    previous_inverts_sql_and_reverses_back_to_requested_order()
}
#[test]
fn sc09_previous_uses_its_own_anchor_predicate() -> TestResult {
    previous_inverts_sql_and_reverses_back_to_requested_order()
}
#[test]
fn sc10_filters_compose_as_conjunctions() -> TestResult {
    scopes_and_filters_compose_with_and_and_keep_root_distinct_from_any()
}
#[test]
fn sc11_root_is_not_any_parent() -> TestResult {
    scopes_and_filters_compose_with_and_and_keep_root_distinct_from_any()
}
#[test]
fn sc12_subpath_is_absent_from_store_criteria() -> TestResult {
    scopes_and_filters_compose_with_and_and_keep_root_distinct_from_any()
}
#[test]
fn sc13_directory_uses_canonical_location_ownership() -> TestResult {
    scopes_and_filters_compose_with_and_and_keep_root_distinct_from_any()
}
#[test]
fn sc14_sql_limit_is_structural() -> TestResult {
    criteria_text_page_and_vm_bounds_refuse_instead_of_clamping_or_skipping()
}
#[test]
fn sc15_vm_progress_guard_is_load_bearing() -> TestResult {
    criteria_text_page_and_vm_bounds_refuse_instead_of_clamping_or_skipping()
}
#[test]
fn sc16_text_bounds_fire_before_decode_allocation() -> TestResult {
    unreadable_rows_and_missing_indexes_are_corruption_not_empty_pages()
}
#[test]
fn sc17_oversize_page_is_refused_not_clamped() -> TestResult {
    criteria_text_page_and_vm_bounds_refuse_instead_of_clamping_or_skipping()
}
#[test]
fn sc22_required_composite_indexes_are_verified() -> TestResult {
    unreadable_rows_and_missing_indexes_are_corruption_not_empty_pages()
}

#[test]
fn recent_projects_list_newest_first_and_refuse_text_over_the_row_bound() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    insert(
        store.connection(),
        "ses_a",
        1,
        "prj_a",
        None,
        None,
        "/work/a",
        "a",
    )?;
    insert(
        store.connection(),
        "ses_b",
        2,
        "prj_b",
        None,
        None,
        "/work/b",
        "b",
    )?;
    insert(
        store.connection(),
        "ses_c",
        3,
        "prj_a",
        None,
        None,
        "/work/a/sub",
        "c",
    )?;
    let listed: Vec<(String, String, i64)> = SessionListStore::over(&mut store)
        .recent_projects(10)?
        .into_iter()
        .map(|(project, directory, updated)| (project.as_str().to_owned(), directory, updated))
        .collect();
    assert_eq!(
        listed,
        vec![
            ("prj_a".to_owned(), "/work/a/sub".to_owned(), 3),
            ("prj_b".to_owned(), "/work/b".to_owned(), 2),
            ("prj_a".to_owned(), "/work/a".to_owned(), 1),
        ]
    );
    assert_eq!(
        SessionListStore::over(&mut store).recent_projects(1)?.len(),
        1
    );

    // A directory one byte over the row bound is refused, never materialized.
    let oversized = format!(
        "/{}",
        "x".repeat(grapt_store::session_list::MAX_ROW_TEXT_BYTES)
    );
    insert(
        store.connection(),
        "ses_big",
        4,
        "prj_big",
        None,
        None,
        &oversized,
        "big",
    )?;
    match SessionListStore::over(&mut store).recent_projects(10) {
        Err(SessionListError::ProjectionCorrupt { reason }) => {
            assert!(reason.contains("absent or oversized"), "{reason}");
        }
        other => panic!("oversized directory must refuse before decode, got {other:?}"),
    }
    Ok(())
}
