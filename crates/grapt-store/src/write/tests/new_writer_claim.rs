use super::*;
#[test]
fn local_new_writer_claim_cannot_transfer_an_existing_owner() -> TestResult {
    let (dir, db) = scratch_db("new-writer-owner")?;
    let aggregate = agg()?;
    let mut first = EventStore::open(&db, "new-writer-claim")?;
    let mut other = EventStore::open(&db, "new-writer-claim")?;
    first.claim_new_writer(&aggregate)?;
    let writer = first.fencing_token(&aggregate)?;
    assert!(matches!(
        other.claim_new_writer(&aggregate),
        Err(WriteError::AggregateClaimed { .. })
    ));
    assert!(matches!(
        other.fencing_token(&aggregate),
        Err(WriteError::NotElected { .. })
    ));
    // Refusing the duplicate must leave the original writer usable.
    let data = json!({"sessionID":"ses_w0","inputID":"msg_new_claim"});
    assert!(matches!(
        append_only(
            &mut first,
            input(writer, &aggregate, &"evt_new_claim".parse()?, &data),
            &mut []
        )?,
        Appended::Committed { seq: 0 }
    ));
    drop((first, other));
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn local_new_writer_claim_without_a_seed_survives_reopen() -> TestResult {
    let (dir, db) = scratch_db("new-writer-reopen")?;
    let aggregate = agg()?;
    let mut first = EventStore::open(&db, "new-writer-claim")?;
    first.claim_new_writer(&aggregate)?;
    let events: i64 = first
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(events, 0, "claim cut must precede the seed write");
    drop(first);
    let mut reopened = EventStore::open(&db, "new-writer-claim")?;
    assert!(matches!(
        reopened.claim_new_writer(&aggregate),
        Err(WriteError::AggregateClaimed { .. })
    ));
    assert!(matches!(
        reopened.fencing_token(&aggregate),
        Err(WriteError::NotElected { .. })
    ));
    drop(reopened);
    std::fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn local_new_writer_claim_refuses_history_without_an_election_row() -> TestResult {
    for retained in ["event", "event_sequence"] {
        let (dir, db) = scratch_db(&format!("new-writer-history-{retained}"))?;
        let aggregate = agg()?;
        let mut first = EventStore::open(&db, "new-writer-claim")?;
        let mut other = EventStore::open(&db, "new-writer-claim")?;
        first.elect_writer(&aggregate)?;
        let writer = first.fencing_token(&aggregate)?;
        let data = json!({"sessionID":"ses_w0","inputID":"msg_new_claim"});
        append_only(
            &mut first,
            input(writer, &aggregate, &"evt_new_claim".parse()?, &data),
            &mut [],
        )?;
        // Deliberate scratch-only corruption from outside the guarded connection:
        // neither history carrier may be mistaken for a fresh identity.
        let hostile = Connection::open(&db)?;
        hostile.execute_batch("PRAGMA foreign_keys=OFF; DELETE FROM aggregate_writer;")?;
        hostile.execute_batch(if retained == "event" {
            "DELETE FROM event_sequence;"
        } else {
            "DELETE FROM event;"
        })?;
        assert!(matches!(
            other.claim_new_writer(&aggregate),
            Err(WriteError::AggregateClaimed { .. })
        ));
        drop((first, other, hostile));
        std::fs::remove_dir_all(dir)?;
    }
    Ok(())
}

#[test]
fn local_new_writer_claim_race_has_one_winner() -> TestResult {
    let (dir, db) = scratch_db("new-writer-race")?;
    let stores = [
        EventStore::open(&db, "new-writer-claim")?,
        EventStore::open(&db, "new-writer-claim")?,
    ];
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
    let handles = stores.map(|mut store| {
        let barrier = std::sync::Arc::clone(&barrier);
        std::thread::spawn(move || {
            let aggregate = agg().unwrap_or_else(|error| panic!("claim identity: {error}"));
            let _released = barrier.wait();
            match store.claim_new_writer(&aggregate) {
                Ok(_) => true,
                Err(WriteError::AggregateClaimed { .. }) => false,
                other => panic!("claim race must have a typed result: {other:?}"),
            }
        })
    });
    let wins = handles
        .into_iter()
        .map(|handle| {
            handle
                .join()
                .unwrap_or_else(|_| panic!("claim worker panicked"))
        })
        .filter(|won| *won)
        .count();
    assert_eq!(wins, 1);
    std::fs::remove_dir_all(dir)?;
    Ok(())
}
