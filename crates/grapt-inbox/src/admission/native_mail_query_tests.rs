//! Recovery must resolve one admission by its complete aggregate key.
use super::PENDING_NATIVE_WAKE_ADMISSION;
use grapt_store::EventStore;

#[test]
fn recovery_reads_one_aggregate_admission() -> Result<(), Box<dyn std::error::Error>> {
    let store = EventStore::open_in_memory()?;
    let mut statement = store.connection().prepare(&format!(
        "EXPLAIN QUERY PLAN {PENDING_NATIVE_WAKE_ADMISSION}"
    ))?;
    let details = statement
        .query_map(rusqlite::params![1_i64, "ses_native_wake"], |row| {
            row.get::<_, String>(3)
        })?
        .collect::<Result<Vec<_>, _>>()?;
    assert!(
        details.iter().any(|detail| {
            detail.contains("event_aggregate_seq_idx")
                && detail.contains("aggregate_family=? AND aggregate_id=? AND seq=?")
        }),
        "recovery must select the exact family/session/sequence admission: {details:?}"
    );
    assert!(
        details
            .iter()
            .all(|detail| !detail.contains("event_type_id_idx")),
        "recovery must not scan admissions across sessions: {details:?}"
    );
    Ok(())
}
