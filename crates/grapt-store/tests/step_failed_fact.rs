//! R2b-2: the durable `session.step.failed.2` store seam from a consumer.
//!
//! Every identity and timestamp is supplied explicitly. These tests never
//! reach for an ambient clock or id source, mirroring the future runtime
//! contract: capture once per logical Step, then reuse on same-owner retry.

use grapt_event::session_step_failed::{self, StepFailedFact};
use grapt_store::{
    AggregateId, Appended, CommittedNotices, DecodedEvent, EventStore, ProjectionId, Projector,
    WriteError,
};
use grapt_types::{EventId, PolicyVersion, SessionId, StepNumber, StepTerminalReason};
use rusqlite::Connection;

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CREATED_MS: i64 = 1_786_147_200_123;
const STEP_FAILURES: ProjectionId = ProjectionId::new("step-failures");

fn fact(step: u32, policy: &str) -> Result<StepFailedFact, Box<dyn std::error::Error>> {
    Ok(StepFailedFact::new(
        StepNumber::new(step).ok_or("step must be positive")?,
        StepTerminalReason::PolicyLimit {
            policy: PolicyVersion::new(policy).ok_or("policy must be non-empty")?,
        },
    )?)
}

fn elected() -> Result<(EventStore, SessionId, AggregateId), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_033602b20001".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&aggregate)?;
    Ok((store, session, aggregate))
}

fn record(
    store: &mut EventStore,
    session: &SessionId,
    id: &str,
    created_ms: i64,
    failure: &StepFailedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<(Appended, CommittedNotices), Box<dyn std::error::Error>> {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let event: EventId = id.parse()?;
    let tx = store.governed_transaction()?;
    let appended = grapt_store::step_failed::record(
        &tx, session, &event, writer, created_ms, failure, projectors,
    )?;
    let notices = tx.commit()?;
    Ok((appended, notices))
}

#[test]
fn the_failure_uses_the_exact_v2_kind_payload_and_caller_coordinates() -> TestResult {
    let (mut store, session, _) = elected()?;
    let failure = fact(7, "policy-r2b2")?;
    let (appended, notices) = record(
        &mut store,
        &session,
        "evt_033602b20001",
        CREATED_MS,
        &failure,
        &mut [],
    )?;
    assert_eq!(appended, Appended::Committed { seq: 0 });
    assert_eq!(notices.len(), 1, "one committed fact yields one notice");
    notices.discard();

    let (id, family, aggregate, seq, created, kind, data): (
        String,
        String,
        String,
        i64,
        i64,
        String,
        String,
    ) = store.connection().query_row(
        "SELECT id, aggregate_family, aggregate_id, seq, created, type, data FROM event",
        [],
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ))
        },
    )?;

    assert_eq!(id, "evt_033602b20001", "the caller's EventId is reused");
    assert_eq!(family, "session", "the aggregate family is Session");
    assert_eq!(
        aggregate,
        session.as_str(),
        "the caller's session is the aggregate"
    );
    assert_eq!(seq, 0, "the first Session fact takes sequence zero");
    assert_eq!(created, CREATED_MS, "created_ms is forwarded unchanged");
    assert_eq!(
        kind, "session.step.failed.2",
        "the exact v2 kind is stored, observed {kind}"
    );

    let value: serde_json::Value = serde_json::from_str(&data)?;
    let decoded = session_step_failed::decode(&value)?;
    assert_eq!(
        decoded, failure,
        "the accepted codec owns the exact payload"
    );
    assert!(
        value.get("sessionID").is_none(),
        "session identity belongs only to the aggregate, never the payload"
    );
    Ok(())
}

#[test]
fn a_same_owner_retry_reconciles_once_while_changed_coordinates_diverge() -> TestResult {
    let (mut store, session, _) = elected()?;
    let failure = fact(3, "policy-original")?;
    let (first, first_notices) = record(
        &mut store,
        &session,
        "evt_033602b20002",
        CREATED_MS,
        &failure,
        &mut [],
    )?;
    assert_eq!(first, Appended::Committed { seq: 0 });
    assert_eq!(first_notices.len(), 1);
    first_notices.discard();

    let (retry, retry_notices) = record(
        &mut store,
        &session,
        "evt_033602b20002",
        CREATED_MS,
        &failure,
        &mut [],
    )?;
    assert_eq!(retry, Appended::Idempotent { seq: 0 });
    assert!(
        retry_notices.is_empty(),
        "an exact retry must not mint a second notice"
    );
    retry_notices.discard();

    let changed_time = record(
        &mut store,
        &session,
        "evt_033602b20002",
        CREATED_MS + 1,
        &failure,
        &mut [],
    );
    assert!(
        matches!(changed_time, Err(ref error) if matches!(error.downcast_ref::<WriteError>(), Some(WriteError::Divergence { .. }))),
        "reusing an EventId with a different created_ms must diverge: {changed_time:?}"
    );

    let changed_payload = fact(4, "policy-other")?;
    let changed_fact = record(
        &mut store,
        &session,
        "evt_033602b20002",
        CREATED_MS,
        &changed_payload,
        &mut [],
    );
    assert!(
        matches!(changed_fact, Err(ref error) if matches!(error.downcast_ref::<WriteError>(), Some(WriteError::Divergence { .. }))),
        "reusing an EventId with different canonical data must diverge: {changed_fact:?}"
    );

    let count: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(count, 1, "all retries and conflicts leave one durable fact");
    Ok(())
}

#[test]
fn a_new_owner_cannot_adopt_the_old_owners_fact_or_consume_a_sequence() -> TestResult {
    let dir = std::env::temp_dir().join(format!("r2b2-owner-{}", std::process::id()));
    let db = dir.join("owner.sqlite");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;

    let session: SessionId = "ses_033602b20003".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let mut old_owner = EventStore::open(&db, "r2b2-old-owner")?;
    old_owner.elect_writer(&aggregate)?;
    let failure = fact(5, "policy-owner")?;
    record(
        &mut old_owner,
        &session,
        "evt_033602b20003",
        CREATED_MS,
        &failure,
        &mut [],
    )?
    .1
    .discard();

    let mut new_owner = EventStore::open(&db, "r2b2-new-owner")?;
    new_owner.elect_writer(&aggregate)?;
    let adopted = record(
        &mut new_owner,
        &session,
        "evt_033602b20003",
        CREATED_MS,
        &failure,
        &mut [],
    );
    assert!(
        matches!(adopted, Err(ref error) if matches!(error.downcast_ref::<WriteError>(), Some(WriteError::Divergence { .. }))),
        "a new epoch must diverge instead of adopting the old owner's fact: {adopted:?}"
    );

    let (next, notices) = record(
        &mut new_owner,
        &session,
        "evt_033602b20004",
        CREATED_MS + 1,
        &fact(6, "policy-next")?,
        &mut [],
    )?;
    assert_eq!(
        next,
        Appended::Committed { seq: 1 },
        "the refused adoption must not consume sequence one"
    );
    notices.discard();
    drop(new_owner);
    drop(old_owner);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

struct FailureProjector {
    fail: bool,
}

impl Projector for FailureProjector {
    fn projection(&self) -> ProjectionId {
        STEP_FAILURES
    }

    fn project(
        &mut self,
        tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), grapt_store::ProjectorError> {
        tx.execute(
            "CREATE TABLE IF NOT EXISTS step_failure_projection (id TEXT PRIMARY KEY, seq INTEGER NOT NULL)",
            [],
        )
        .map_err(|error| error.to_string())?;
        tx.execute(
            "INSERT INTO step_failure_projection (id, seq) VALUES (?1, ?2)",
            rusqlite::params![
                event.id.as_str(),
                i64::try_from(event.seq.value()).map_err(|error| error.to_string())?
            ],
        )
        .map_err(|error| error.to_string())?;
        if self.fail {
            return Err("step-failure projector refused after writing".into());
        }
        Ok(())
    }
}

#[test]
fn the_step_failure_runs_its_projector_and_watermark_in_the_same_transaction() -> TestResult {
    let (mut store, session, _) = elected()?;
    let failure = fact(8, "policy-project")?;
    let mut projector = FailureProjector { fail: false };
    let (appended, notices) = record(
        &mut store,
        &session,
        "evt_033602b20005",
        CREATED_MS,
        &failure,
        &mut [&mut projector],
    )?;
    assert_eq!(appended, Appended::Committed { seq: 0 });
    notices.discard();

    let projected: i64 = match store.connection().query_row(
        "SELECT COUNT(*) FROM step_failure_projection WHERE id = 'evt_033602b20005' AND seq = 0",
        [],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("the step-failure projector did not run: {error}"),
    };
    assert_eq!(projected, 1, "the fact's projector row must commit");

    let watermark: i64 = store.connection().query_row(
        "SELECT seq FROM projection_watermark WHERE projection = ?1 AND aggregate_family = 'session' AND aggregate_id = ?2",
        rusqlite::params![STEP_FAILURES.as_str(), session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(watermark, 0, "the projector watermark commits with its row");
    Ok(())
}

#[test]
fn a_projector_failure_rolls_back_the_fact_rows_watermark_and_sequence() -> TestResult {
    let (mut store, session, _) = elected()?;
    let failure = fact(9, "policy-project-fail")?;
    let mut failing = FailureProjector { fail: true };
    let attempt = record(
        &mut store,
        &session,
        "evt_033602b20006",
        CREATED_MS,
        &failure,
        &mut [&mut failing],
    );
    assert!(
        matches!(attempt, Err(ref error) if matches!(error.downcast_ref::<WriteError>(), Some(WriteError::Projector { .. }))),
        "the projector refusal must be carried as a typed store error: {attempt:?}"
    );

    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    let cursors: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM event_sequence", [], |row| row.get(0))?;
    let watermarks: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM projection_watermark", [], |row| {
                row.get(0)
            })?;
    assert_eq!(events, 0, "the failed projector leaves no fact");
    assert_eq!(
        cursors, 0,
        "the failed projector leaves no consumed sequence"
    );
    assert_eq!(watermarks, 0, "the failed projector leaves no watermark");
    assert!(
        store
            .connection()
            .prepare("SELECT id FROM step_failure_projection")
            .is_err(),
        "the projector's own table creation and row must roll back"
    );

    let mut working = FailureProjector { fail: false };
    let (retried, notices) = record(
        &mut store,
        &session,
        "evt_033602b20006",
        CREATED_MS,
        &failure,
        &mut [&mut working],
    )?;
    assert_eq!(retried, Appended::Committed { seq: 0 });
    notices.discard();
    Ok(())
}

#[test]
fn a_crash_before_commit_leaves_only_the_previous_step_fact() -> TestResult {
    let dir = std::env::temp_dir().join(format!("r2b2-step-cut-{}", std::process::id()));
    let db = dir.join("cut.sqlite");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;

    let session: SessionId = "ses_033602b20002".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let mut store = EventStore::open(&db, "r2b2-cut")?;
    store.elect_writer(&aggregate)?;
    record(
        &mut store,
        &session,
        "evt_033602b20007",
        CREATED_MS,
        &fact(10, "policy-before")?,
        &mut [],
    )?
    .1
    .discard();

    {
        let writer = store.fencing_token(&aggregate)?;
        let event: EventId = "evt_033602b20008".parse()?;
        let failure = fact(11, "policy-cut")?;
        let tx = store.governed_transaction()?;
        let outcome = grapt_store::step_failed::record(
            &tx,
            &session,
            &event,
            writer,
            CREATED_MS + 1,
            &failure,
            &mut [],
        )?;
        assert_eq!(outcome, Appended::Committed { seq: 1 });
        // Crash cut: the transaction is dropped without commit.
    }

    let observer = Connection::open(&db)?;
    let rows: i64 = observer.query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    let head: i64 = observer.query_row("SELECT MAX(seq) FROM event", [], |row| row.get(0))?;
    assert_eq!(rows, 1, "the uncommitted failure must be absent");
    assert_eq!(head, 0, "the previous durable fact must remain the head");
    drop(observer);

    let (retried, notices) = record(
        &mut store,
        &session,
        "evt_033602b20008",
        CREATED_MS + 1,
        &fact(11, "policy-cut")?,
        &mut [],
    )?;
    assert_eq!(
        retried,
        Appended::Committed { seq: 1 },
        "retry after the pre-commit cut takes the unconsumed sequence"
    );
    notices.discard();

    drop(store);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}
