use super::*;

type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error>>;

const SESSION: &str = "ses_a";

/// The per-cut reader this module had before promotions were batched: each
/// cut asks [`promotion_sequence`] on its own. Batched reads must agree with it
/// on every cut and every refusal.
fn per_cut(conn: &Connection, session: &SessionId) -> std::result::Result<Vec<(u64, u64)>, String> {
    let mut statement = conn
        .prepare(
            "SELECT seq, type, CASE WHEN length(CAST(data AS BLOB)) <= 4096 THEN data END
             FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1
             AND type LIKE 'session.revert.committed.%' ORDER BY seq LIMIT ?2",
        )
        .map_err(|error| error.to_string())?;
    let rows = statement
        .query_map((session.as_str(), (MAX_REWINDS + 1) as i64), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|error| error.to_string())?;
    let mut cuts = HistoryVisibility::default();
    for row in rows {
        if cuts.cuts.len() == MAX_REWINDS {
            return Err(VisibilityError::TooLong.to_string());
        }
        let (end, kind, raw) = row.map_err(|error| error.to_string())?;
        if kind != "session.revert.committed.1" {
            return Err(format!(
                "rewind history is corrupt: unsupported boundary {kind}"
            ));
        }
        let raw = raw.ok_or("rewind history is corrupt: boundary exceeds 4096 bytes")?;
        let fact = boundary(&raw).map_err(|error| error.to_string())?;
        if fact.session() != session {
            return Err("rewind history is corrupt: boundary names another session".into());
        }
        let end = sequence(end).map_err(|error| error.to_string())?;
        let from = promotion_sequence(conn, session, fact.before())
            .map_err(|error| error.to_string())?
            .value();
        if from >= end || !cuts.contains(from) {
            return Err(
                "rewind history is corrupt: boundary does not select a surviving earlier prompt"
                    .into(),
            );
        }
        cuts.cuts.push((from, end));
    }
    Ok(cuts.cuts)
}

fn batched(conn: &Connection, session: &SessionId) -> std::result::Result<Vec<(u64, u64)>, String> {
    HistoryVisibility::read(conn, session)
        .map(|visible| visible.cuts)
        .map_err(|error| error.to_string())
}

struct Log {
    conn: Connection,
    seq: i64,
}

impl Log {
    fn new() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.execute_batch(
            "CREATE TABLE event (seq INTEGER NOT NULL, aggregate_family TEXT NOT NULL,
               aggregate_id TEXT NOT NULL, type TEXT NOT NULL, data BLOB NOT NULL);",
        )?;
        Ok(Self { conn, seq: 0 })
    }

    fn push(&mut self, kind: &str, data: &str) -> Result<i64> {
        self.seq += 1;
        self.conn.execute(
            "INSERT INTO event VALUES (?1, 'session', ?2, ?3, ?4)",
            (self.seq, SESSION, kind, data),
        )?;
        Ok(self.seq)
    }

    fn promote(&mut self, input: &str) -> Result<i64> {
        self.push(
            "session.input.promoted.1",
            &format!(r#"{{"sessionID":"{SESSION}","inputID":"{input}"}}"#),
        )
    }

    fn work(&mut self) -> Result<i64> {
        self.push("session.step.ended.3", "{}")
    }

    fn rewind(&mut self, input: &str) -> Result<i64> {
        self.push(
            "session.revert.committed.1",
            &format!(r#"{{"sessionID":"{SESSION}","to":"{input}"}}"#),
        )
    }

    fn agree(&self) -> Result<std::result::Result<Vec<(u64, u64)>, String>> {
        let session: SessionId = SESSION.parse()?;
        let expected = per_cut(&self.conn, &session);
        assert_eq!(batched(&self.conn, &session), expected);
        Ok(expected)
    }
}

#[test]
fn batched_cuts_match_the_per_cut_reader() -> Result {
    let mut log = Log::new()?;
    let first = log.promote("msg_a")?;
    log.work()?;
    let second = log.promote("msg_b")?;
    log.work()?;
    let cut = log.rewind("msg_b")?;
    let third = log.promote("msg_c")?;
    log.work()?;
    let last = log.rewind("msg_c")?;
    log.promote("msg_d")?;
    assert_eq!(
        log.agree()?,
        Ok(vec![
            (second as u64, cut as u64),
            (third as u64, last as u64)
        ])
    );
    assert!(first < second);
    Ok(())
}

#[test]
fn many_cuts_still_resolve_each_prompt() -> Result {
    let mut log = Log::new()?;
    log.promote("msg_keep")?;
    for index in 0..40 {
        let input = format!("msg_{index}");
        log.promote(&input)?;
        log.work()?;
        log.rewind(&input)?;
    }
    assert_eq!(log.agree()?.map(|cuts| cuts.len()), Ok(40));
    Ok(())
}

#[test]
fn batched_refusals_match_the_per_cut_reader() -> Result {
    type Build = fn(&mut Log) -> Result;
    let cases: [(&str, Build); 11] = [
        ("no promotion", |log| {
            log.promote("msg_a")?;
            log.rewind("msg_z")?;
            Ok(())
        }),
        ("duplicate", |log| {
            log.promote("msg_a")?;
            log.promote("msg_b")?;
            log.promote("msg_b")?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("malformed before", |log| {
            log.promote("msg_a")?;
            log.push("session.input.promoted.1", "not json")?;
            log.promote("msg_b")?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("non-text input after", |log| {
            log.promote("msg_a")?;
            log.promote("msg_b")?;
            log.push(
                "session.input.promoted.1",
                &format!(r#"{{"sessionID":"{SESSION}","inputID":7}}"#),
            )?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("oversized", |log| {
            log.promote("msg_a")?;
            let padding = "x".repeat(4096);
            log.push(
                "session.input.promoted.1",
                &format!(r#"{{"sessionID":"{SESSION}","inputID":"msg_b","pad":"{padding}"}}"#),
            )?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("other session", |log| {
            log.promote("msg_a")?;
            log.push(
                "session.input.promoted.1",
                r#"{"sessionID":"ses_b","inputID":"msg_b"}"#,
            )?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("same prompt twice", |log| {
            log.promote("msg_a")?;
            log.promote("msg_b")?;
            log.rewind("msg_b")?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        ("later bad boundary after a bad promotion", |log| {
            log.promote("msg_a")?;
            log.rewind("msg_z")?;
            log.push("session.revert.committed.1", "not json")?;
            Ok(())
        }),
        ("bad boundary before a good one", |log| {
            log.promote("msg_a")?;
            log.promote("msg_b")?;
            log.push("session.revert.committed.2", "{}")?;
            log.rewind("msg_b")?;
            Ok(())
        }),
        // The boundary match is case-insensitive, so a case variant is
        // selected and refused as unsupported rather than read as no cut.
        ("upper-case boundary", |log| {
            log.promote("msg_a")?;
            log.push("SESSION.REVERT.COMMITTED.1", "{}")?;
            Ok(())
        }),
        ("mixed-case boundary", |log| {
            log.promote("msg_a")?;
            log.push("session.revert.COMMITTED.1", "{}")?;
            Ok(())
        }),
    ];
    for (name, build) in cases {
        let mut log = Log::new()?;
        build(&mut log)?;
        let outcome = log.agree()?;
        assert!(outcome.is_err(), "{name}: {outcome:?}");
    }
    Ok(())
}

/// Every transcript read asks for visibility, so its boundary query must only
/// seek the production index and never visit the whole session. Filtering the
/// session's rows by LIKE could not use the type column: on row A's
/// 10,222-event Session each call took 4.9 ms, about 14 calls per rendered
/// history row.
#[test]
fn boundary_read_only_seeks_the_session_type_index() -> Result {
    let log = Log::new()?;
    log.conn.execute_batch(
        "CREATE UNIQUE INDEX event_aggregate_seq_idx ON event (aggregate_family, aggregate_id, seq);
         CREATE INDEX event_aggregate_type_seq_idx
             ON event (aggregate_family, aggregate_id, type, seq);",
    )?;
    let plan = log
        .conn
        .prepare(&format!("EXPLAIN QUERY PLAN {BOUNDARIES}"))?
        .query_map((SESSION, 1), |row| row.get::<_, String>(3))?
        .collect::<std::result::Result<Vec<_>, _>>()?
        .join("\n");
    // The distinct-type walk, then the row read for each matching type.
    assert!(
        plan.contains(
            "COVERING INDEX event_aggregate_type_seq_idx (aggregate_family=? AND aggregate_id=? AND type>?)"
        ),
        "{plan}"
    );
    assert!(
        plan.contains(
            "event_aggregate_type_seq_idx (aggregate_family=? AND aggregate_id=? AND type=?)"
        ),
        "{plan}"
    );
    // No step reads the session's rows by sequence alone.
    assert!(
        !plan.contains("event_aggregate_seq_idx (aggregate_family=? AND aggregate_id=?)"),
        "{plan}"
    );
    Ok(())
}

#[test]
fn visibility_sql_preserves_half_open_cuts_and_overlapping_rewinds() -> Result {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE candidates (seq INTEGER);
        WITH RECURSIVE numbers(seq) AS (VALUES(-1) UNION ALL
          SELECT seq+1 FROM numbers WHERE seq<1000)
        INSERT INTO candidates SELECT seq FROM numbers;
        INSERT INTO candidates VALUES (NULL), (9223372036854775806), (9223372036854775807);",
    )?;
    let cases = [
        vec![],
        vec![(0, 1)],
        vec![(5, 10), (15, 20), (25, 30)],
        vec![(20, 30), (10, 40), (5, 50)],
        vec![(5, 10), (10, 15), (14, 20), (30, 40)],
        vec![(i64::MAX as u64 - 1, i64::MAX as u64)],
        (0..MAX_REWINDS as u64)
            .map(|index| (index * 3, index * 3 + 1))
            .collect(),
    ];
    for cuts in cases {
        let visibility = HistoryVisibility { cuts };
        let original_count = visibility.count();
        let rows = conn
            .prepare(&format!(
                "SELECT seq FROM candidates WHERE {} ORDER BY seq",
                visibility.sql("seq")
            ))?
            .query_map([], |row| row.get::<_, Option<i64>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        let expected = conn
            .prepare("SELECT seq FROM candidates ORDER BY seq")?
            .query_map([], |row| row.get::<_, Option<i64>>(0))?
            .collect::<std::result::Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|seq| match seq {
                None => original_count == 0,
                Some(seq) => match u64::try_from(*seq) {
                    Ok(seq) => visibility.contains(seq),
                    Err(_) => true,
                },
            })
            .collect::<Vec<_>>();
        assert_eq!(rows, expected, "cuts: {:?}", visibility.cuts);
        assert_eq!(visibility.count(), original_count);
    }
    Ok(())
}

#[test]
fn visibility_query_work_does_not_double_when_rewind_count_doubles() -> Result {
    let conn = Connection::open_in_memory()?;
    conn.execute_batch(
        "CREATE TABLE candidates (seq INTEGER NOT NULL);
        WITH RECURSIVE numbers(seq) AS (VALUES(10000) UNION ALL
          SELECT seq+1 FROM numbers WHERE seq<14095)
        INSERT INTO candidates SELECT seq FROM numbers;",
    )?;
    let work = |count: usize| -> Result<i32> {
        let visibility = HistoryVisibility {
            cuts: (0..count as u64)
                .map(|index| (index * 3, index * 3 + 1))
                .collect(),
        };
        let mut statement = conn.prepare(&format!(
            "SELECT count(*) FROM candidates WHERE {}",
            visibility.sql("seq")
        ))?;
        assert_eq!(statement.query_row([], |row| row.get::<_, i64>(0))?, 4096);
        Ok(statement.get_status(rusqlite::StatementStatus::VmStep))
    };
    let half = work(MAX_REWINDS / 2)?;
    let full = work(MAX_REWINDS)?;
    assert!(
        full < half * 3 / 2,
        "128 cuts: {half} steps; 256 cuts: {full} steps"
    );
    Ok(())
}
