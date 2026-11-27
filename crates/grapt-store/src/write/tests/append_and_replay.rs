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

#[test]
fn appends_zero_based_and_projects_in_the_same_transaction() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let d1 = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let d2 = json!({"sessionID":"ses_w0","inputID":"msg_w2"});
    let mut p = CountingProjector {
        rows: 0,
        fail: false,
    };
    assert_eq!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &d1),
            &mut [&mut p]
        )?,
        Appended::Committed { seq: 0 },
    );
    assert_eq!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w2".parse()?, &d2),
            &mut [&mut p]
        )?,
        Appended::Committed { seq: 1 },
    );
    let cursor: i64 = store.connection().query_row(
        "SELECT seq FROM event_sequence
         WHERE aggregate_family = 'session' AND aggregate_id = 'ses_w0'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(cursor, 1, "event_sequence.seq == MAX(event.seq)");
    assert_eq!(p.rows, 2);
    Ok(())
}

#[test]
fn projector_failure_rolls_back_the_whole_append() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let d = json!({"x":1});
    let mut bad = CountingProjector {
        rows: 0,
        fail: true,
    };
    assert!(matches!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_wf".parse()?, &d),
            &mut [&mut bad]
        ),
        Err(WriteError::Projector { .. })
    ));
    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    let cursor: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM event_sequence", [], |r| r.get(0))?;
    assert_eq!(
        (events, cursor),
        (0, 0),
        "nothing survives a projector failure"
    );
    Ok(())
}

#[test]
fn identical_replay_is_idempotent_and_divergence_rejects() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    )?;
    assert_eq!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &d),
            &mut []
        )?,
        Appended::Idempotent { seq: 0 },
    );
    let diverged = json!({"sessionID":"ses_w0","inputID":"msg_DIFFERENT"});
    assert!(matches!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &diverged),
            &mut []
        ),
        Err(WriteError::Divergence { .. })
    ));
    Ok(())
}

#[test]
fn conflict_identity_includes_type_and_created() -> TestResult {
    // Oracle law (bus.ts:255-266): same id/aggregate/data but a changed
    // version/kind or created is DIVERGENCE, never an idempotent retry.
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    )?;

    let id: EventId = "evt_w1".parse()?;
    let mut changed_version = input(writer, &aggregate, &id, &d);
    changed_version.version = match EventVersion::new(2) {
        Some(v) => v,
        None => panic!("version"),
    };
    assert!(matches!(
        append_only(&mut store, changed_version, &mut []),
        Err(WriteError::Divergence { .. })
    ));

    let mut changed_created = input(writer, &aggregate, &id, &d);
    changed_created.created_ms = 1786060800999;
    assert!(matches!(
        append_only(&mut store, changed_created, &mut []),
        Err(WriteError::Divergence { .. })
    ));

    // The true exact retry still reconciles.
    assert_eq!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &d),
            &mut []
        )?,
        Appended::Idempotent { seq: 0 },
    );
    Ok(())
}

#[test]
fn a_stored_row_the_replay_path_refuses_cannot_settle_as_idempotent() -> TestResult {
    // A cold-review probe, made permanent.
    //
    // The pre-check loads a stored row by event id and decides reconcile
    // versus diverge. Before that correction it compared the stored TYPE
    // STRING and the canonical data hash without ever asking the closed
    // decoder whether the row was decodable at all — the kind was consulted
    // on no path a well-formed row could take. So a row carrying the
    // unmodelled kind `session.imaginary.7` settled as `Idempotent` even
    // though `decode_for_replay` refuses that exact row.
    //
    // The staged row carries a REAL elected epoch, so the no-new-v1 gate is
    // not what refuses it: this witness isolates the decode pre-check.
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let data = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let data_text = grapt_event::canonical_string(&data);
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 0)?;
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_w1",
            seq: 0,
            created_ms: 1786060800000,
            versioned_type: "session.imaginary.7",
            data_text: &data_text,
            writer_epoch: Some(i64::from(writer.epoch().value())),
        },
    )?;

    // Side one: the closed decoder refuses this exact row.
    let direct = crate::decode::decode_row(
        "evt_w1",
        "ses_w0",
        0,
        1786060800000,
        "session.imaginary.7",
        &data_text,
    );
    assert!(
        direct.is_err(),
        "the probe is pointless unless replay refuses this row, got {direct:?}"
    );

    // Side two: the pre-check must reach the same verdict. An exact retry
    // of that unmodelled kind — same aggregate, family, type, created,
    // epoch and data, so every comparison field matches — is the case that
    // settled Idempotent.
    let id: EventId = "evt_w1".parse()?;
    let mut retry = input(writer, &aggregate, &id, &data);
    retry.kind_name = "session.imaginary";
    retry.version = match EventVersion::new(7) {
        Some(v) => v,
        None => panic!("version"),
    };
    let out = append_only(&mut store, retry, &mut []);
    assert!(
        matches!(out, Err(WriteError::Decode(_))),
        "a row the replay path refuses must refuse here too, not reconcile: got {out:?}"
    );
    Ok(())
}

#[test]
fn corrupt_cursor_refuses_with_a_typed_error() -> TestResult {
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, -5)?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    assert!(matches!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &d),
            &mut []
        ),
        Err(WriteError::CorruptSequence { value: -5, .. })
    ));
    Ok(())
}

/// A deterministic authority that mints one known token, so a witness can
/// name the exact bytes it expects to find in the store's owner.
///
/// The token is built once by the test body, where `IdToken::new`'s `Option`
/// can be resolved with `?`; the trait's `mint` is infallible and the crate
/// denies `unwrap`/`expect`.
#[derive(Debug)]
struct FixedToken(grapt_types::IdToken);

impl IdAuthority for FixedToken {
    fn mint(&self) -> grapt_types::IdToken {
        self.0.clone()
    }
}

/// The store-seam witness for the injected identity.
///
/// The other two identity witnesses prove that `OsRandomIds` mints distinct
/// tokens and that two DISTINCT owners transfer ownership. Neither pins the
/// link between them: if `WriterOwnerId::mint` ignored its `ids` argument and
/// went back to a process-global counter, two owners minted in one process
/// would STILL differ, so both would stay green while the injected token was
/// silently unused. That is the resurrection this witness exists to kill.
///
/// It drives both public injection seams — `open_with_ids` on a file-backed
/// database and `open_in_memory_with_ids` — with two DIFFERENT known tokens,
/// and asserts each resulting store's owner carries its own injected token.
/// An implementation that does not consume `ids` cannot satisfy it.
#[test]
fn both_store_seams_carry_the_injected_identity_token_into_the_owner() -> TestResult {
    let first = grapt_types::IdToken::new("witness-token-alpha")
        .ok_or("the witness token must be constructible")?;
    let second = grapt_types::IdToken::new("witness-token-beta")
        .ok_or("the witness token must be constructible")?;

    let dir = std::env::temp_dir().join(format!("grapt-seam-ids-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("seam.db");

    let on_disk = EventStore::open_with_ids(&db, "0.0.1", &FixedToken(first.clone()))?;
    let in_memory = EventStore::open_in_memory_with_ids(&FixedToken(second.clone()))?;

    assert!(
        on_disk.owner().as_str().contains(first.as_str()),
        "open_with_ids did not consume the injected token: owner {} carries no {}",
        on_disk.owner(),
        first.as_str()
    );
    assert!(
        in_memory.owner().as_str().contains(second.as_str()),
        "open_in_memory_with_ids did not consume the injected token: owner {} carries no {}",
        in_memory.owner(),
        second.as_str()
    );
    assert_ne!(
        on_disk.owner(),
        in_memory.owner(),
        "two seams injected with different tokens produced one owner"
    );

    drop(on_disk);
    drop(in_memory);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
