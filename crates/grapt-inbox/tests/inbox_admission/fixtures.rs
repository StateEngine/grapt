//! Shared inbox admission fixtures.
use super::*;

pub(super) fn minter(prefix: &'static str) -> impl FnMut() -> EventId {
    let mut n = 0usize;
    move || {
        n += 1;
        match format!("evt_{prefix}{n}").parse() {
            Ok(id) => id,
            Err(e) => panic!("mint: {e}"),
        }
    }
}

pub(super) fn ids(prefix: &str, n: usize) -> Result<Vec<EventId>, Box<dyn std::error::Error>> {
    (0..n)
        .map(|i| Ok(format!("evt_{prefix}{i}").parse()?))
        .collect()
}

pub(super) fn admit<'a>(
    inbox: &mut Inbox,
    event_id: &'a EventId,
    session: &'a SessionId,
    input: &'a MessageId,
    text: &'a str,
    delivery: DeliveryMode,
) -> Result<AdmitOutcome, InboxError> {
    inbox.admit(AdmitRequest {
        event_id,
        session,
        input,
        text,
        delivery,
        resume: false,
        created_ms: 1786060800000,
    })
}

pub(super) fn fresh() -> Result<(Inbox, SessionId), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_r3a".parse()?;
    let mut store = EventStore::open_in_memory()?;
    // Election is explicit because the append path never elects on a caller's
    // behalf: a store handed to the inbox unelected refuses every admission
    // with `NotElected` instead of silently taking the aggregate.
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Ok((Inbox::new(store)?, session))
}

/// `fresh`, but backed by a file so a second connection can reach the same
/// database. Needed only where a fixture must stage state the store's own
/// connection now refuses.
pub(super) fn fresh_on(path: &Path) -> Result<(Inbox, SessionId), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_r3a".parse()?;
    let mut store = EventStore::open(path, "test")?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Ok((Inbox::new(store)?, session))
}

pub(super) fn scratch_dir(name: &str) -> Result<PathBuf, Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-r3a-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

pub(super) fn ensure_delivery_authority_fixture(conn: &Connection) -> TestResult {
    conn.execute_batch(
        "CREATE TABLE subagent_operation (
             operation_id TEXT PRIMARY KEY,
             family_operation_id TEXT NOT NULL,
             parent_session_id TEXT NOT NULL,
             target_input TEXT,
             claim_generation INTEGER,
             run_generation INTEGER,
             status TEXT NOT NULL,
             output TEXT,
             artifact_id TEXT,
             artifact_length INTEGER,
             artifact_digest TEXT,
             artifact_disposition TEXT,
             error TEXT,
             reason TEXT,
             evidence TEXT,
             claim_route TEXT,
             target_parent TEXT,
             delivery TEXT NOT NULL,
             delivery_settlement TEXT,
             claim_id TEXT
         );",
    )?;
    Ok(())
}

/// Prepare connections and retain the same writer epoch before racing admissions.
/// Migration/election failure must produce a diagnosis before any worker exists.
/// Each worker has a bounded start channel; losing a participant cannot strand
/// its peer at a barrier. Results are bounded too, before we join finished work.
pub(super) fn concurrent_admissions(
    db: &Path,
    prefix: &str,
    input: &str,
) -> Result<Vec<AdmitOutcome>, Box<dyn std::error::Error>> {
    let owner = EventStore::open(db, "0.0.1")?.owner().clone();
    let session: SessionId = "ses_r3a".parse()?;
    let input: MessageId = input.parse()?;
    let mut prepared = Vec::new();
    for worker in 0..2 {
        let store = EventStore::open_as(db, "0.0.1", &owner)?;
        let mut inbox = Inbox::new(store)?;
        inbox
            .store_mut()
            .elect_writer(&AggregateId::Session(session.clone()))?;
        let event: EventId = format!("evt_{prefix}{worker}").parse()?;
        prepared.push((inbox, event));
    }
    let (results, receive) = std::sync::mpsc::sync_channel(2);
    let (arrivals, arrived) = std::sync::mpsc::sync_channel(2);
    let mut starts = Vec::new();
    let mut handles = Vec::new();
    for (mut inbox, event) in prepared {
        let (start, ready) = std::sync::mpsc::sync_channel(1);
        starts.push(start);
        let results = results.clone();
        let arrivals = arrivals.clone();
        let session = session.clone();
        let input = input.clone();
        handles.push(std::thread::spawn(move || {
            let _ = arrivals.send(());
            let outcome = ready
                .recv_timeout(std::time::Duration::from_secs(30))
                .map_err(|error| format!("admission start unavailable: {error}"))
                .and_then(|()| {
                    admit(
                        &mut inbox,
                        &event,
                        &session,
                        &input,
                        "same",
                        DeliveryMode::Steer,
                    )
                    .map_err(|error| error.to_string())
                });
            let _ = results.send(outcome);
        }));
    }
    drop(results);
    drop(arrivals);
    for _ in 0..2 {
        arrived.recv_timeout(std::time::Duration::from_secs(30))?;
    }
    for start in starts {
        start.send(())?;
    }
    let mut outcomes = Vec::new();
    for _ in 0..2 {
        outcomes.push(
            receive
                .recv_timeout(std::time::Duration::from_secs(30))?
                .map_err(std::io::Error::other)?,
        );
    }
    for handle in handles {
        handle
            .join()
            .map_err(|_| std::io::Error::other("admission worker panicked"))?;
    }
    Ok(outcomes)
}
