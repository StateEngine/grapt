use super::*;

fn fork(conn: &Connection, child: &str, parent: &str, boundary: i64) -> TestResult {
    row(
        conn,
        child,
        0,
        "session.forked.2",
        &serde_json::json!({
            "sessionID":child, "parentID":parent, "parentSeq":boundary, "from":"msg_accounting"
        }),
    )
}

#[test]
fn history_fork_counts_only_child_calls_and_uses_frozen_parent_evidence() -> TestResult {
    let conn = Fixture::new("fork-own-usage")?;
    schema(&conn)?;
    open(&conn, "ses_parent", 0, "pat_parent", 1)?;
    usage(&conn, "ses_parent", 1, "pat_parent", 90)?;
    fork(&conn, "ses_child", "ses_parent", 1)?;
    open(&conn, "ses_child", 1, "pat_parent", 1)?;
    usage(&conn, "ses_child", 2, "pat_parent", 90)?;
    usage(&conn, "ses_child", 3, "pat_child", 10)?;
    // A later parent reconciliation cannot widen the frozen evidence.
    usage(&conn, "ses_parent", 2, "pat_parent", 99)?;
    let child = conn.read("ses_child")?;
    assert!(child.enumeration_complete);
    assert_eq!(child.attempts.len(), 1);
    assert_eq!(child.attempts[0].attempt.as_str(), "pat_child");
    assert_eq!(
        child.attempts[0]
            .usage
            .input_fresh()
            .map(|v| v.count().value()),
        Some(10)
    );
    assert!(child.attempts[0].usage.cache_read().is_none());
    fork(&conn, "ses_grandchild", "ses_child", 3)?;
    usage(&conn, "ses_grandchild", 1, "pat_parent", 90)?;
    usage(&conn, "ses_grandchild", 2, "pat_child", 10)?;
    assert!(conn.read("ses_grandchild")?.attempts.is_empty());
    usage(&conn, "ses_child", 4, "pat_parent", 99)?;
    assert!(
        conn.read("ses_child").is_err(),
        "post-fork evidence hid changed child usage"
    );
    conn.finish()
}

#[test]
fn fork_accounting_refuses_missing_boundaries_but_accepts_empty_prefix() -> TestResult {
    let conn = Fixture::new("fork-boundary")?;
    schema(&conn)?;
    usage(&conn, "ses_parent", 0, "pat_parent", 90)?;
    usage(&conn, "ses_parent", 5, "pat_later", 10)?;
    for (child, boundary) in [("ses_gap", 2), ("ses_tail", 999999)] {
        fork(&conn, child, "ses_parent", boundary)?;
        assert!(conn.read(child).is_err(), "missing boundary accepted");
    }
    fork(&conn, "ses_empty", "ses_parent", -1)?;
    usage(&conn, "ses_empty", 1, "pat_parent", 90)?;
    assert_eq!(conn.read("ses_empty")?.attempts.len(), 1);
    conn.finish()
}
