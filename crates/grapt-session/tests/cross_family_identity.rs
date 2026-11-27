//! F3B4c: a Session read answers only from the SESSION family.
//!
//! `event` is ONE log carrying a family-tagged union of rows. `aggregate_id` is
//! not a key on its own — only `(aggregate_family, aggregate_id)` is, which is
//! why that pair is `event_sequence`'s primary key and the leading pair of the
//! unique index. `F3B1A_RESIDUALS.md` item 5 recorded that reading by identity
//! alone was correct only while sessions were the only writable family; the
//! campaign family ended that precondition.
//!
//! # Why the adversary row is legitimate
//!
//! The rows below are staged through a RAW connection, before the store is
//! opened. That is not a bypass of the governed-write chokepoint — the
//! authorizer is installed per store connection, and this is a database as some
//! other writer, or a partially converted file, could genuinely present it.
//!
//! The adversary carries `aggregate_family = 'campaign'` together with the
//! session kind string `session.created.1`. Nothing forbids that: the
//! `event_family_shape` CHECK constrains the payload columns for a family, and
//! says nothing about `type`. So the `type` predicate — which happens to
//! separate the two families today, because every campaign row the store itself
//! writes carries an `agent_campaign.*` kind — is exactly the kind of accidental
//! discriminator that stops discriminating the moment a row arrives by another
//! route. The family predicate is what makes the read correct rather than lucky.

use grapt_session::SessionStore;
use grapt_store::EventStore;
use grapt_types::SessionId;
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn scratch(name: &str) -> Result<std::path::PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-xfam-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Builds the target schema by opening a store once, then stages rows through a
/// raw connection.
fn staged(db: &std::path::Path, rows: &[(&str, &str, &str, &str, Option<&str>)]) -> TestResult {
    {
        let _ = EventStore::open(db, "0.0.1")?;
    }
    let conn = Connection::open(db)?;
    conn.execute_batch("PRAGMA foreign_keys = ON")?;
    for (id, family, aggregate, kind, data) in rows {
        conn.execute(
            "INSERT OR IGNORE INTO event_sequence (aggregate_family, aggregate_id, seq, owner_id)
             VALUES (?1, ?2, 0, NULL)",
            rusqlite::params![family, aggregate],
        )?;
        // A campaign-shaped row needs its envelope columns to satisfy the family
        // CHECK. The bytes are never decoded on this path: the Session read
        // never looks inside a campaign envelope, which is the whole reason a
        // campaign row must not be able to answer for it.
        if *family == "campaign" {
            conn.execute(
                "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type,
                                    data, writer_epoch, envelope_bytes, campaign_transition_id)
                 VALUES (?1, ?2, ?3, 0, 1, ?4, NULL, 1, X'00', 'adversary-transition')",
                rusqlite::params![id, family, aggregate, kind],
            )?;
        } else {
            conn.execute(
                "INSERT INTO event (id, aggregate_family, aggregate_id, seq, created, type,
                                    data, writer_epoch, envelope_bytes, campaign_transition_id)
                 VALUES (?1, ?2, ?3, 0, 1, ?4, ?5, 1, NULL, NULL)",
                rusqlite::params![id, family, aggregate, kind, data],
            )?;
        }
    }
    Ok(())
}

#[test]
fn a_campaign_row_sharing_a_session_identity_and_kind_cannot_answer_for_that_session() -> TestResult
{
    let dir = scratch("ghost")?;
    let db = dir.join("scratch.db");
    let ghost: SessionId = "ses_04b1327e9ffeghost".parse()?;

    // ONLY a campaign-family row exists under this identity, and it carries the
    // session kind string. There is no Session here.
    staged(
        &db,
        &[(
            "evt_adversary",
            "campaign",
            ghost.as_str(),
            "session.created.1",
            None,
        )],
    )?;

    let store = EventStore::open(&db, "0.0.1")?;
    let sessions = SessionStore::over(&store);

    assert!(
        !sessions.exists(&ghost)?,
        "a campaign-family row must never make a Session exist"
    );
    match sessions.placement(&ghost) {
        Err(grapt_session::SessionStoreError::Unknown { session }) => {
            assert_eq!(session, ghost.as_str());
        }
        Err(other) => panic!(
            "expected Unknown for a session that does not exist, got {other}. A campaign              row's `data` is NULL by the family CHECK, so a family-blind read fails as a              column-type error rather than telling the truth about the Session"
        ),
        Ok(placement) => {
            panic!("a campaign-family row supplied a Session PLACEMENT: {placement:?}")
        }
    }
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_real_session_still_reads_correctly_beside_a_same_identity_campaign_aggregate() -> TestResult {
    let dir = scratch("shared")?;
    let db = dir.join("scratch.db");
    let directory = dir.join("workspace");
    std::fs::create_dir_all(&directory)?;
    let directory_text = directory.to_string_lossy().into_owned();
    let seed = serde_json::json!({"info": {"directory": directory_text}}).to_string();
    let shared: SessionId = "ses_04b1327e9ffeshared".parse()?;

    // The scope inversion for the test above: the SAME identity, now carrying a
    // genuine session seed as well. If the family predicate were merely
    // suppressing rows, this arm would break too.
    staged(
        &db,
        &[
            (
                "evt_session",
                "session",
                shared.as_str(),
                "session.created.1",
                Some(seed.as_str()),
            ),
            (
                "evt_adversary",
                "campaign",
                shared.as_str(),
                "session.created.1",
                None,
            ),
        ],
    )?;

    let store = EventStore::open(&db, "0.0.1")?;
    let sessions = SessionStore::over(&store);

    assert!(sessions.exists(&shared)?, "the real Session is found");
    let placement = sessions.placement(&shared)?;
    assert!(
        placement.directory().contains("workspace"),
        "the placement comes from the SESSION seed, got {placement:?}"
    );

    // And both rows really are present under one identity, so the assertions
    // above are about discrimination rather than about an empty table.
    let families: Vec<String> = store
        .connection()
        .prepare(
            "SELECT aggregate_family FROM event WHERE aggregate_id = ?1 ORDER BY aggregate_family",
        )?
        .query_map([shared.as_str()], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    assert_eq!(
        families,
        vec!["campaign".to_owned(), "session".to_owned()],
        "the log carries both families under one identity"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
