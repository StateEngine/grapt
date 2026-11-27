//! Corrupt and unreadable capture histories remain distinct refusal outcomes.
use super::*;

#[test]
fn a_corrupt_row_refuses_the_page() {
    let root = scratch("corrupt");
    let database = root.join("store.db");
    let appended = {
        let mut store = open(&database);
        seed(&mut store, Some(&database), "ses_a", 2, None)
    };
    assert_eq!(appended[1].1, EventSeq::new(1));
    let corrupt_id = appended[1].0.clone();
    {
        let raw = open_raw(&database);
        match raw.execute(
            "UPDATE event SET data = 'not-json' WHERE id = ?1",
            params![corrupt_id],
        ) {
            Ok(1) => {}
            Ok(rows) => panic!("corruption staging updated {rows} rows, expected 1"),
            Err(error) => panic!("corruption staging failed: {error}"),
        }
    }
    let store = open(&database);
    let all = CaptureInterest::All;
    match read_refusal(
        &store,
        &all,
        &CaptureCursor::start(all.clone()),
        MAX_CAPTURE_PAGE_ROWS,
    ) {
        CaptureReadError::CorruptRow { id, detail } => {
            assert_eq!(id, corrupt_id);
            assert!(!detail.is_empty());
        }
        other => panic!("an undecodable row must refuse the page as CorruptRow, not {other:?}"),
    }
}

#[test]
fn an_unreadable_store_is_distinct_from_absent_and_corrupt() {
    let root = scratch("unreadable");
    let database = root.join("store.db");
    let store = {
        let mut store = open(&database);
        // This legacy fault truncates the main database. WAL can still serve
        // its committed pages, so stage this fault in rollback-journal mode.
        match store
            .connection()
            .execute_batch("PRAGMA journal_mode = DELETE")
        {
            Ok(()) => {}
            Err(error) => panic!("truncation journal fixture failed: {error}"),
        }
        let _a = seed(&mut store, Some(&database), "ses_a", 1, None);
        // A fully cached small database would answer the page from cache and
        // hide the truncation; one page of cache forces the read to the file.
        match store.connection().execute_batch("PRAGMA cache_size = 1") {
            Ok(()) => {}
            Err(error) => panic!("cache pragma failed: {error}"),
        }
        store
    };
    match std::fs::write(&database, b"") {
        Ok(()) => {}
        Err(error) => panic!("truncation staging failed: {error}"),
    }
    let all = CaptureInterest::All;
    match read_refusal(
        &store,
        &all,
        &CaptureCursor::start(all.clone()),
        MAX_CAPTURE_PAGE_ROWS,
    ) {
        CaptureReadError::UnreadableStore { detail } => assert!(!detail.is_empty()),
        other => panic!("a truncated store must refuse as UnreadableStore, not {other:?}"),
    }
}
