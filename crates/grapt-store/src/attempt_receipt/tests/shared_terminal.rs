use super::*;
#[test]
fn scc_recovery_shares_one_terminal_across_permitted_attempts() -> TestResult {
    let mut store = elected_store()?;
    let second = attempt_id("att_033602b20002");
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    reserve(&mut store, &reserved_receipt(second.as_str(), 3))?;
    permit(&mut store, &attempt_id(ATTEMPT), PERMIT_MS)?;
    permit(&mut store, &second, PERMIT_MS + 1)?;
    settle(&mut store, &attempt_id(ATTEMPT))?;
    let before: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = ?1",
        [TERMINAL],
        |row| row.get(0),
    )?;
    settle(&mut store, &second)?;
    let after: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = ?1",
        [TERMINAL],
        |row| row.get(0),
    )?;
    assert_eq!(
        before, after,
        "SCC recovery rewrote the owning Step terminal"
    );
    assert_eq!(terminal_rows(&store)?, 1);
    assert_eq!(
        columns(&store, second.as_str())?.settled_ms,
        Some(SETTLED_MS)
    );
    Ok(())
}

#[test]
fn scc_reserved_peer_cannot_hide_a_permitted_attempt() -> TestResult {
    let mut store = elected_store()?;
    let second = attempt_id("att_033602b20002");
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    reserve(&mut store, &reserved_receipt(second.as_str(), 3))?;
    permit(&mut store, &second, PERMIT_MS)?;
    settle(&mut store, &attempt_id(ATTEMPT))?;
    settle(&mut store, &second)?;
    let data: String = store.connection().query_row(
        "SELECT data FROM event WHERE id = ?1",
        [TERMINAL],
        |row| row.get(0),
    )?;
    assert!(
        data.contains("ambiguous"),
        "SCC permitted peer was classified as nothing initiated"
    );
    assert!(
        data.contains(second.as_str()),
        "the terminal must retain the peer's actual ambiguity evidence"
    );
    assert_eq!(terminal_rows(&store)?, 1);
    Ok(())
}
#[test]
fn scc_recovery_refuses_a_terminal_that_denies_a_later_permit() -> TestResult {
    let mut store = elected_store()?;
    reserve(&mut store, &reserved_receipt(ATTEMPT, 3))?;
    settle(&mut store, &attempt_id(ATTEMPT))?;
    let second = attempt_id("att_033602b20002");
    reserve(&mut store, &reserved_receipt(second.as_str(), 3))?;
    permit(&mut store, &second, PERMIT_MS)?;
    assert!(matches!(
        settle(&mut store, &second),
        Err(WriteError::ReceiptTerminalUnproven { .. })
    ));
    assert_eq!(columns(&store, second.as_str())?.settled_ms, None);
    Ok(())
}
