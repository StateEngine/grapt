//! Acceptance: the D1 commit-order law for the compaction family — parents
//! before children — measured against the REAL frozen clone.
//!
//! # What "parents before children" means here
//!
//! The frozen oracle records compaction as a CHOREOGRAPHY, not one fact:
//! `session.compaction.admitted`, `.started`, and then exactly one terminal,
//! `.ended` or `.failed`. A terminal is the child of its start. The law this
//! gate freezes is that within one session aggregate no terminal ever commits
//! without an open start before it, sequences never go backwards, and the
//! recorded times never regress.
//!
//! That ordering is load-bearing for every fold this lane builds on. A
//! rebuildable cache folds durable truth forward in commit order; if a terminal
//! could precede its start, a fold would have to buffer children against
//! parents that may never arrive, and "rebuild from canonical truth" would stop
//! being a single forward pass.
//!
//! # Why the clone, and why this cannot be a blocking green
//!
//! These are measured properties of real recorded history, not of code this
//! workspace can mutate — no Rust edit can move them, so no mutation row
//! attributes them and none is fabricated. The gate is `#[ignore]`d for the
//! same reason as its `clone_*` neighbours: it needs the evidence machine. It
//! is an explicit evidence-machine obligation, and under the evidence command
//! an absent clone FAILS rather than passing quietly.

use std::collections::BTreeMap;
#[path = "../../../test-support/evidence.rs"]
mod evidence;

/// The exact compaction population of the frozen clone, measured 2026-08-10.
/// Pinned as literals because a changed count means a different database, and
/// this gate's other assertions would then be describing something else.
const ADMITTED: usize = 5;
const STARTED: usize = 19;
const ENDED: usize = 16;
const FAILED: usize = 3;
const SESSIONS_WITH_COMPACTION: usize = 8;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    Admitted,
    Started,
    Ended,
    Failed,
}

impl Phase {
    /// The stored `type` string is `name.version`; the phase is the segment
    /// before the version. Matching the whole string keeps an unknown member of
    /// the family visible instead of silently folding it into a known phase.
    fn parse(stored: &str) -> Option<Self> {
        match stored {
            "session.compaction.admitted.1" => Some(Phase::Admitted),
            "session.compaction.started.1" => Some(Phase::Started),
            "session.compaction.ended.1" => Some(Phase::Ended),
            "session.compaction.failed.1" => Some(Phase::Failed),
            _ => None,
        }
    }
}

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run `cargo test -p grapt-store -- --include-ignored` on the evidence machine"]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn the_clone_commits_every_compaction_parent_before_its_child() {
    let path = evidence::frozen_clone();
    let conn = match rusqlite::Connection::open_with_flags(
        &path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ) {
        Ok(conn) => conn,
        Err(error) => panic!("open the frozen clone read-only: {error}"),
    };

    let mut statement = match conn.prepare(
        "SELECT aggregate_id, seq, created, type FROM event
         WHERE type LIKE 'session.compaction.%'
         ORDER BY aggregate_id, seq",
    ) {
        Ok(statement) => statement,
        Err(error) => panic!("prepare: {error}"),
    };
    let rows = match statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
        ))
    }) {
        Ok(rows) => rows,
        Err(error) => panic!("query: {error}"),
    };

    // BTreeMap so the aggregate order is deterministic and a failure names the
    // same session on every run.
    let mut by_session: BTreeMap<String, Vec<(i64, i64, Phase)>> = BTreeMap::new();
    for row in rows {
        let (aggregate, seq, created, stored) = match row {
            Ok(row) => row,
            Err(error) => panic!("row: {error}"),
        };
        let phase = match Phase::parse(&stored) {
            Some(phase) => phase,
            // A new member of the family would silently escape every count
            // below, so it fails here instead.
            None => panic!("unrecognised compaction kind in the frozen clone: {stored}"),
        };
        by_session
            .entry(aggregate)
            .or_default()
            .push((seq, created, phase));
    }

    assert_eq!(
        by_session.len(),
        SESSIONS_WITH_COMPACTION,
        "the frozen clone's compaction population changed; this is a different database"
    );

    let (mut admitted, mut started, mut ended, mut failed) = (0usize, 0usize, 0usize, 0usize);
    for (aggregate, events) in &by_session {
        // `open_start` is the parent-before-child carrier: a terminal is legal
        // only while a start is open, and it closes the one it settles.
        let mut open_start = false;
        let mut previous: Option<(i64, i64)> = None;
        for &(seq, created, phase) in events {
            if let Some((previous_seq, previous_created)) = previous {
                assert!(
                    seq > previous_seq,
                    "{aggregate}: sequence {seq} does not advance past {previous_seq}; \
                     within-aggregate order is monotonic"
                );
                assert!(
                    created >= previous_created,
                    "{aggregate}: recorded time regressed at seq {seq}"
                );
            }
            previous = Some((seq, created));

            match phase {
                Phase::Admitted => admitted += 1,
                Phase::Started => {
                    started += 1;
                    open_start = true;
                }
                Phase::Ended | Phase::Failed => {
                    if phase == Phase::Ended {
                        ended += 1;
                    } else {
                        failed += 1;
                    }
                    assert!(
                        open_start,
                        "{aggregate}: compaction terminal at seq {seq} commits with no open \
                         start before it — the child precedes its parent"
                    );
                    open_start = false;
                }
            }
        }
    }

    assert_eq!(
        (admitted, started, ended, failed),
        (ADMITTED, STARTED, ENDED, FAILED),
        "the frozen clone's compaction phase counts changed"
    );
    // Every start reaches exactly one terminal. Asserted separately from the
    // per-session walk because the walk proves ORDER, and this proves there is
    // no start left dangling once the whole population is counted.
    assert_eq!(
        started,
        ended + failed,
        "every compaction start must reach exactly one terminal"
    );

    eprintln!(
        "D1 COMPACTION ORDER OK: {} sessions, admitted {admitted}, started {started}, \
         ended {ended}, failed {failed}, zero children before parents",
        by_session.len()
    );
}
