//! Rollback safety: an older reader must skip an unfamiliar lifecycle fact,
//! never refuse the Session that carries it.
//!
//! A service rolled back to a build that predates the launch-mode fact will meet
//! stores written by a newer one. The unsafe failure is not "the mode is
//! unknown": it is a reader that treats the unfamiliar row as a corrupt Session
//! and hides a Session the user can still open.
//!
//! This build knows `session.launched`, so it cannot be its own older reader.
//! The older reader is therefore simulated honestly in two parts:
//!
//! 1. A kind this build does NOT know stands in for what `session.launched` is
//!    to an older build, and the read posture must skip it with the cursor
//!    advancing — not abort, and not lose the rows after it.
//! 2. The stored Session still answers in full when that unfamiliar row sits
//!    beside its seed, which is what "skip the event, not the Session" means.
//!
//! The rows are staged through a raw connection before the store opens, which is
//! how a database written by a newer build genuinely presents itself to an older
//! one; it is not a way around the governed write path.

use grapt_store::EventStore;
use grapt_types::SessionId;
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-launch-skip-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("workspace"))?;
    Ok(dir)
}

/// Stage one session aggregate's rows at their own sequences.
fn stage(db: &std::path::Path, session: &SessionId, rows: &[(&str, &str)]) -> TestResult {
    {
        let _ = EventStore::open(db, "0.0.1")?;
    }
    let conn = Connection::open(db)?;
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    for (seq, (kind, data)) in rows.iter().enumerate() {
        let seq = i64::try_from(seq)?;
        conn.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES ('session', ?1, ?2, NULL)",
            rusqlite::params![session.as_str(), seq],
        )?;
        conn.execute(
            "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type,
                                data, writer_epoch, envelope_bytes, campaign_transition_id)
             VALUES (?1, 'session', ?2, ?3, 1, ?4, ?5, 1, NULL, NULL)",
            rusqlite::params![format!("evt_stage{seq}"), session.as_str(), seq, kind, data],
        )?;
    }
    Ok(())
}

#[test]
fn an_older_reader_skips_an_unfamiliar_lifecycle_fact_and_still_answers_the_session() -> TestResult
{
    let dir = scratch("skip")?;
    let db = dir.join("scratch.db");
    let directory = dir.join("workspace").to_string_lossy().into_owned();
    let session: SessionId = "ses_04b1327e9ffelaunch".parse()?;

    let seed = serde_json::json!({
        "sessionID": session.as_str(),
        "info": {
            "id": session.as_str(),
            "projectID": "global",
            "directory": directory,
            "title": "launch skip",
            "time": {"created": 1, "updated": 1},
        },
    })
    .to_string();
    // What `session.launched.1` looks like to a build that predates it: a kind
    // it cannot name. Keeping it shaped exactly like our fact — a small JSON
    // object under the same aggregate — is what makes the skip meaningful.
    let older_reader_saw = serde_json::json!({
        "sessionID": session.as_str(),
        "launch": "headless",
    })
    .to_string();
    stage(
        &db,
        &session,
        &[
            ("session.created.1", seed.as_str()),
            ("session.launched.future", older_reader_saw.as_str()),
        ],
    )?;

    // 1. The unfamiliar kind is a SKIP whose cursor still advances, so rows
    // after it are not lost either.
    match grapt_event::decode_for_read(
        "evt_stage1",
        session.as_str(),
        1,
        1,
        "session.launched.future",
        &older_reader_saw,
        grapt_event::LegacyDataLimit::MEASURED_DEFAULT,
    )? {
        grapt_event::ReadOutcome::Decoded(_) => {
            panic!("an unnamed kind must not decode: that is the older-reader path this guards")
        }
        grapt_event::ReadOutcome::SkippedUnknownKind { seq, .. } => {
            assert_eq!(seq.value(), 1, "the skip must still advance the cursor");
        }
    }

    // 2. The Session itself is still answerable rather than reported corrupt.
    let store = EventStore::open(&db, "0.0.1")?;
    let sessions = grapt_session::SessionStore::over(&store);
    let record = sessions.record(&session)?;
    assert_eq!(record.session, session);
    assert_eq!(record.title, "launch skip");

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn this_build_records_and_reads_back_the_launch_mode_it_writes() -> TestResult {
    let dir = scratch("roundtrip")?;
    let db = dir.join("scratch.db");
    let directory = dir.join("workspace").to_string_lossy().into_owned();
    let session: SessionId = "ses_04b1327e9fferecord".parse()?;

    let seed = serde_json::json!({
        "sessionID": session.as_str(),
        "info": {
            "id": session.as_str(),
            "projectID": "global",
            "directory": directory,
            "title": "launch roundtrip",
            "time": {"created": 1, "updated": 1},
        },
    })
    .to_string();
    let launched = serde_json::json!({
        "sessionID": session.as_str(),
        "launch": "headless",
    })
    .to_string();
    stage(
        &db,
        &session,
        &[
            ("session.created.1", seed.as_str()),
            ("session.launched.1", launched.as_str()),
        ],
    )?;

    let store = EventStore::open(&db, "0.0.1")?;
    let record = grapt_session::SessionStore::over(&store).record(&session)?;
    assert_eq!(
        record.launch,
        Some(grapt_types::SessionLaunch::Headless),
        "the durable launch fact must fold into the record"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
