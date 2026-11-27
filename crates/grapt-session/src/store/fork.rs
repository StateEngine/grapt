//! History ancestry is distinct from a fresh subagent's parent identity.
use super::*;
use grapt_session_history::fork_boundary::{self, ReadError};
use std::collections::HashSet;

impl SessionStore<'_> {
    /// The oldest retained history-fork ancestor, for stable request affinity.
    /// Fresh child Sessions and empty-history forks retain their own identity.
    /// A corrupt, cyclic or excessively deep chain is refused, never truncated.
    pub fn history_fork_origin(&self, session: &SessionId) -> Result<SessionId, SessionStoreError> {
        let corrupt = |reason| SessionStoreError::Corrupt {
            session: session.as_str().to_owned(),
            reason,
        };
        let store = |source| SessionStoreError::Store {
            session: session.as_str().to_owned(),
            source,
        };
        let mut current = session.clone();
        let mut visited = HashSet::new();
        let mut statement = self.conn.prepare(fork_boundary::SQL).map_err(store)?;
        let mut boundary_statement = self
            .conn
            .prepare(fork_boundary::BOUNDARY_SQL)
            .map_err(store)?;
        for _ in 0..128 {
            if !visited.insert(current.clone()) {
                return Err(corrupt("history fork ancestry contains a cycle".into()));
            }
            let boundary = fork_boundary::read(&mut statement, &mut boundary_statement, &current)
                .map_err(|error| match error {
                ReadError::Store(source) => store(source),
                ReadError::Corrupt(reason) => corrupt(reason),
            })?;
            let Some(boundary) = boundary else {
                return Ok(current);
            };
            if boundary.parent_seq() == grapt_types::ParentSeq::Nothing {
                return Ok(current);
            }
            current = boundary.parent().clone();
        }
        Err(corrupt("history fork ancestry exceeds 128 Sessions".into()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    type TestResult = Result<(), Box<dyn std::error::Error>>;
    fn event(conn: &Connection, session: &str, seq: i64, kind: &str, data: Value) -> TestResult {
        conn.execute(
            "INSERT INTO event VALUES ('session',?1,?2,?3,?4)",
            rusqlite::params![session, seq, kind, data.to_string()],
        )?;
        Ok(())
    }
    #[test]
    fn history_origin_is_stable_for_nested_forks_and_distinct_for_fresh_children() -> TestResult {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE event(aggregate_family TEXT,aggregate_id TEXT,seq INTEGER,type TEXT,data TEXT);")?;
        event(
            &conn,
            "ses_root",
            0,
            "session.created.1",
            serde_json::json!({}),
        )?;
        for (child, parent, boundary) in [
            ("ses_child", "ses_root", 0),
            ("ses_leaf", "ses_child", 0),
            ("ses_empty", "ses_root", -1),
        ] {
            event(
                &conn,
                child,
                0,
                "session.forked.2",
                serde_json::json!({"sessionID":child,"parentID":parent,"parentSeq":boundary}),
            )?;
        }
        event(
            &conn,
            "ses_fresh",
            0,
            "session.created.1",
            serde_json::json!({"info":{"parentID":"ses_root"}}),
        )?;
        let store = SessionStore::new(&conn);
        for (session, origin) in [
            ("ses_root", "ses_root"),
            ("ses_child", "ses_root"),
            ("ses_leaf", "ses_root"),
            ("ses_empty", "ses_empty"),
            ("ses_fresh", "ses_fresh"),
        ] {
            assert_eq!(
                store.history_fork_origin(&session.parse()?)?.as_str(),
                origin
            );
            assert_eq!(
                store.history_fork_origin(&session.parse()?)?.as_str(),
                origin,
                "resume changed ancestry"
            );
        }
        // Later parent changes cannot change immutable affinity.
        event(
            &conn,
            "ses_root",
            1,
            "session.model.selected.1",
            serde_json::json!({}),
        )?;
        assert_eq!(
            store.history_fork_origin(&"ses_leaf".parse()?)?.as_str(),
            "ses_root"
        );
        conn.execute("UPDATE event SET data=json_set(data,'$.parentSeq',999999) WHERE aggregate_id='ses_child'",[])?;
        assert!(store.history_fork_origin(&"ses_leaf".parse()?).is_err());
        Ok(())
    }
    #[test]
    fn history_origin_refuses_cycles_repeated_facts_and_unreadable_store() -> TestResult {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch("CREATE TABLE event(aggregate_family TEXT,aggregate_id TEXT,seq INTEGER,type TEXT,data TEXT);")?;
        for (child, parent) in [("ses_a", "ses_b"), ("ses_b", "ses_a")] {
            event(
                &conn,
                child,
                0,
                "session.forked.2",
                serde_json::json!({"sessionID":child,"parentID":parent,"parentSeq":0}),
            )?;
        }
        assert!(SessionStore::new(&conn)
            .history_fork_origin(&"ses_a".parse()?)
            .is_err());
        conn.execute_batch("DELETE FROM event WHERE aggregate_id='ses_b';")?;
        event(
            &conn,
            "ses_b",
            0,
            "session.created.1",
            serde_json::json!({}),
        )?;
        event(
            &conn,
            "ses_a",
            1,
            "session.forked.2",
            serde_json::json!({"sessionID":"ses_a","parentID":"ses_b","parentSeq":0}),
        )?;
        assert!(SessionStore::new(&conn)
            .history_fork_origin(&"ses_a".parse()?)
            .is_err());
        conn.execute_batch("DROP TABLE event;")?;
        assert!(matches!(
            SessionStore::new(&conn).history_fork_origin(&"ses_a".parse()?),
            Err(SessionStoreError::Store { .. })
        ));
        Ok(())
    }
}
