//! Structural tool-ordering contradictions still refuse a replay.
//!
//! A tool exchange whose partner row is merely UNREADABLE is a separate,
//! unresolved question: position alone cannot pair parallel calls, so this
//! reader keeps refusing rather than guessing which exchange to drop.
use super::*;

#[test]
fn a_terminal_before_its_call_still_refuses_recent_projection() -> Result<(), String> {
    let fixture = Fixture::new("replay-retrograde-result")?;
    fixture.record_exchange()?;
    let connection = recent::raw_conn(&fixture)?;
    let position = |kind: &str| -> Result<i64, String> {
        connection
            .query_row(
                "SELECT seq FROM event WHERE aggregate_family='session'
             AND aggregate_id=?1 AND type=?2",
                (fixture.session.as_str(), kind),
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())
    };
    let call = position("session.content.appended.1")?;
    let terminal = position("session.tool.terminal.1")?;
    let temporary =
        i64::try_from(sustained_defects::head(&fixture)?).map_err(|error| error.to_string())? + 1;
    for (from, to) in [(call, temporary), (terminal, call), (temporary, terminal)] {
        let changed = connection
            .execute(
                "UPDATE event SET seq=?3 WHERE aggregate_family='session'
             AND aggregate_id=?1 AND seq=?2",
                (fixture.session.as_str(), from, to),
            )
            .map_err(|error| error.to_string())?;
        assert_eq!(changed, 1);
    }
    let head = sustained_defects::head(&fixture)?;
    assert!(
        super::super::recent::prepare_recent(
            &lock(fixture.inbox()?),
            fixture.artifacts()?,
            &fixture.session,
            head,
        )
        .is_err(),
        "a structural tool ordering contradiction was projected as a gap"
    );
    Ok(())
}
