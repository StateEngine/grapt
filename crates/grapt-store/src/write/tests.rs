mod append_and_replay;
mod fencing_and_projection;
mod new_writer_claim;
mod reconciliation_and_schema;

use super::*;
use serde_json::json;

type TestResult = Result<(), Box<dyn std::error::Error>>;

struct CountingProjector {
    rows: u64,
    fail: bool,
}
impl Projector for CountingProjector {
    fn projection(&self) -> ProjectionId {
        ProjectionId::new("counting")
    }
    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), crate::ProjectorError> {
        if self.fail {
            return Err("boom".into());
        }
        tx.execute(
            "CREATE TABLE IF NOT EXISTS projected (id TEXT PRIMARY KEY, seq INTEGER NOT NULL)",
            [],
        )
        .map_err(|e| e.to_string())?;
        tx.execute(
            "INSERT INTO projected (id, seq) VALUES (?1, ?2)",
            rusqlite::params![event.id.as_str(), event.seq.value() as i64],
        )
        .map_err(|e| e.to_string())?;
        self.rows += 1;
        Ok(())
    }
}

fn session(id: &str) -> Result<AggregateId, Box<dyn std::error::Error>> {
    Ok(AggregateId::Session(id.parse()?))
}

/// The aggregate every test below writes to.
fn agg() -> Result<AggregateId, Box<dyn std::error::Error>> {
    session("ses_w0")
}

/// An in-memory store that has ALREADY been elected for `ses_w0`.
///
/// The election is explicit here for the same reason it is explicit in
/// production: nothing in the append path performs it. A test that forgot
/// this call gets `NotElected`, not a silently self-elected writer.
fn elected_store() -> Result<(EventStore, AggregateId), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let aggregate = agg()?;
    store.elect_writer(&aggregate)?;
    Ok((store, aggregate))
}

/// A file-backed scratch database carrying the oracle-era `session` table.
/// File-backed because two `EventStore` instances must see one database,
/// which two independent in-memory connections never do.
fn scratch_db(
    name: &str,
) -> Result<(std::path::PathBuf, std::path::PathBuf), Box<dyn std::error::Error>> {
    let dir = std::env::temp_dir().join(format!("grapt-write-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("scratch.db");
    Connection::open(&db)?.execute_batch("CREATE TABLE session (id TEXT PRIMARY KEY)")?;
    Ok((dir, db))
}

/// `EventStore::append` for the witnesses below that assert only the
/// durable outcome.
///
/// The notices are discarded EXPLICITLY rather than by an `.0` on every call
/// site: `CommittedNotices` is `#[must_use]` precisely so no caller drops a
/// published fact by accident, and a test helper that quietly threw them
/// away would be the first place that habit formed. The witnesses that care
/// about notices call `append` directly.
fn append_only(
    store: &mut EventStore,
    input: AppendInput<'_>,
    projectors: &mut [&mut dyn Projector],
) -> Result<Appended, WriteError> {
    let (appended, notices) = EventStore::append(store, input, projectors)?;
    notices.discard();
    Ok(appended)
}

fn input<'a>(
    writer: FencedWriter,
    aggregate: &AggregateId,
    id: &'a EventId,
    data: &'a Value,
) -> AppendInput<'a> {
    AppendInput {
        id,
        aggregate: aggregate.clone(),
        writer,
        kind_name: "session.input.admitted",
        version: match EventVersion::new(1) {
            Some(v) => v,
            None => panic!("version"),
        },
        created_ms: 1786060800000,
        data,
    }
}

#[derive(Debug)]
struct FixedToken(grapt_types::IdToken);

impl IdAuthority for FixedToken {
    fn mint(&self) -> grapt_types::IdToken {
        self.0.clone()
    }
}
