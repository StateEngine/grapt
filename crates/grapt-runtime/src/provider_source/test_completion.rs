//! Step completion assertions shared by production-source witnesses.
use super::*;

pub(super) fn terminal_rows(inbox: &Mutex<Inbox>, session: &SessionId, kind: &str) -> i64 {
    let guard = match inbox.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    let instants: Vec<(i64, i64)> = {
        let mut query = guard.store().connection().prepare(
            "SELECT created, json_extract(data, '$.completedAt') FROM event WHERE aggregate_id = ?1 AND type = ?2"
        ).unwrap_or_else(|error| panic!("completion witness query failed: {error}"));
        query
            .query_map([session.as_str(), kind], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap_or_else(|error| panic!("completion witness read failed: {error}"))
            .collect::<Result<Vec<_>, _>>()
            .unwrap_or_else(|error| panic!("completion witness row failed: {error}"))
    };
    assert!(
        instants.iter().all(|(reserved, observed)| {
            *reserved == TestClock.now_ms()
                && *observed == SettlementClock.now_ms()
                && observed > reserved
        }),
        "Step completion reused its pre-I/O reservation instead of the one settlement observation"
    );
    let sql = format!("SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type = '{kind}'");
    match guard
        .store()
        .connection()
        .query_row(&sql, [session.as_str()], |row| row.get(0))
    {
        Ok(rows) => rows,
        Err(error) => panic!("durable {kind} count must read: {error}"),
    }
}
