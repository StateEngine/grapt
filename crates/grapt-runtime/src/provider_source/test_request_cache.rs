//! Admission-time request facts have their own codec, separate from completions.
use super::*;

pub(super) fn request_rows(inbox: &Mutex<Inbox>, session: &SessionId) -> usize {
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let mut query = guard
        .store()
        .connection()
        .prepare(
            "SELECT created,
                CASE WHEN length(CAST(data AS BLOB)) <= ?2 THEN data END
         FROM event WHERE aggregate_id = ?1 AND type = 'session.request.cache.1'
         ORDER BY seq LIMIT 2",
        )
        .unwrap_or_else(|error| panic!("request witness query failed: {error}"));
    let max_bytes = i64::try_from(grapt_event::request_cache::MAX_RECORD_BYTES)
        .unwrap_or_else(|error| panic!("request witness byte bound is invalid: {error}"));
    let rows = query
        .query_map(rusqlite::params![session.as_str(), max_bytes], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .unwrap_or_else(|error| panic!("request witness read failed: {error}"))
        .collect::<Result<Vec<_>, _>>()
        .unwrap_or_else(|error| panic!("request witness row failed: {error}"));
    assert!(
        rows.len() <= 1,
        "one attempt produced duplicate request facts"
    );
    for (created, data) in &rows {
        assert_eq!(*created, TestClock.now_ms());
        let data = data
            .as_ref()
            .unwrap_or_else(|| panic!("request fact exceeds its byte bound"));
        let value = serde_json::from_str(data)
            .unwrap_or_else(|error| panic!("request fact is not JSON: {error}"));
        let fact = grapt_event::request_cache::decode(&value)
            .unwrap_or_else(|error| panic!("request fact failed strict decoding: {error}"));
        assert_eq!(fact.attempt, attempt("pat_u2_1"));
        assert_eq!(fact.step.value(), 1);
    }
    rows.len()
}
