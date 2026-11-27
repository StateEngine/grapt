//! L4a: the durable `session.step.ended.2` store seam from a consumer.
//!
//! Every identity and timestamp is supplied explicitly. These tests never
//! reach for an ambient clock or id source, mirroring the runtime contract:
//! capture once per logical Step, then reuse on same-owner retry.
//!
//! The seam under test is the mirror of `step_failed`. What is proved here is
//! not that an append works — the governed path already has its own witnesses —
//! but that the SUCCESS terminal reaches durability under its own exact kind,
//! which is the thing that did not exist before this lane.

use grapt_event::session_step_ended::{self, StepEndedFact};
use grapt_store::{AggregateId, Appended, CommittedNotices, EventStore, Projector};
use grapt_types::{EventId, ProviderCompletionDisposition, SessionId, StepNumber};

type TestResult = Result<(), Box<dyn std::error::Error>>;

const CREATED_MS: i64 = 1_786_147_200_123;

fn fact(
    step: u32,
    completion: ProviderCompletionDisposition,
) -> Result<StepEndedFact, Box<dyn std::error::Error>> {
    Ok(StepEndedFact::new(
        StepNumber::new(step).ok_or("step must be positive")?,
        completion,
    ))
}

fn elected() -> Result<(EventStore, SessionId), Box<dyn std::error::Error>> {
    let session: SessionId = "ses_033602b20001".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&aggregate)?;
    Ok((store, session))
}

fn record(
    store: &mut EventStore,
    session: &SessionId,
    id: &str,
    created_ms: i64,
    ended: &StepEndedFact,
    projectors: &mut [&mut dyn Projector],
) -> Result<(Appended, CommittedNotices), Box<dyn std::error::Error>> {
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate)?;
    let event: EventId = id.parse()?;
    let tx = store.governed_transaction()?;
    let appended = grapt_store::step_ended::record(
        &tx, session, &event, writer, created_ms, ended, projectors,
    )?;
    let notices = tx.commit()?;
    Ok((appended, notices))
}

/// A Step that succeeded now writes something durable, under the exact v2 kind.
#[test]
fn the_success_terminal_uses_the_exact_v2_kind_payload_and_caller_coordinates() -> TestResult {
    let (mut store, session) = elected()?;
    let ended = fact(7, ProviderCompletionDisposition::AssertedComplete)?;
    let (appended, notices) = record(
        &mut store,
        &session,
        "evt_033602b20001",
        CREATED_MS,
        &ended,
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
        kind, "session.step.ended.2",
        "the exact v2 kind is stored, never the frozen oracle's v1"
    );

    let value: serde_json::Value = serde_json::from_str(&data)?;
    let decoded = session_step_ended::decode(&value)?;
    assert_eq!(decoded, ended, "the accepted codec owns the exact payload");
    assert!(
        value.get("sessionID").is_none(),
        "session identity belongs only to the aggregate, never the payload"
    );
    assert!(
        value.get("cost").is_none() && value.get("tokens").is_none(),
        "cost and tokens are AttemptUsage's, not a second accounting truth here"
    );
    Ok(())
}

/// Every disposition reaches durability as itself.
///
/// One disposition proving the seam would leave the other three unwitnessed at
/// the store level, and `Truncated` is exactly the one a careless writer would
/// smooth away.
#[test]
fn every_disposition_reaches_durability_as_itself() -> TestResult {
    let cases = [
        (
            ProviderCompletionDisposition::AssertedComplete,
            "evt_033602b20011",
        ),
        (
            ProviderCompletionDisposition::AssertedContinue,
            "evt_033602b20012",
        ),
        (ProviderCompletionDisposition::Truncated, "evt_033602b20013"),
        (ProviderCompletionDisposition::Unstated, "evt_033602b20014"),
    ];
    for (completion, id) in cases {
        let (mut store, session) = elected()?;
        let ended = fact(1, completion)?;
        let (_, notices) = record(&mut store, &session, id, CREATED_MS, &ended, &mut [])?;
        notices.discard();
        let data: String = store
            .connection()
            .query_row("SELECT data FROM event", [], |row| row.get(0))?;
        let value: serde_json::Value = serde_json::from_str(&data)?;
        assert_eq!(
            session_step_ended::decode(&value)?,
            ended,
            "{completion:?} did not reach durability as itself"
        );
    }
    Ok(())
}

/// A Step terminal is written once; an exact same-owner retry is idempotent
/// and a changed payload under the same identity is refused.
///
/// This is the governed append path's law rather than this seam's, and it is
/// witnessed here because the seam is what a runtime owner will retry through:
/// a second terminal for one Step must never become a second durable fact.
#[test]
fn an_exact_retry_is_idempotent_and_a_changed_payload_diverges() -> TestResult {
    let (mut store, session) = elected()?;
    let ended = fact(3, ProviderCompletionDisposition::AssertedComplete)?;
    let (first, notices) = record(
        &mut store,
        &session,
        "evt_033602b20021",
        CREATED_MS,
        &ended,
        &mut [],
    )?;
    notices.discard();
    assert_eq!(first, Appended::Committed { seq: 0 });

    let (again, notices) = record(
        &mut store,
        &session,
        "evt_033602b20021",
        CREATED_MS,
        &ended,
        &mut [],
    )?;
    notices.discard();
    assert_eq!(
        again,
        Appended::Idempotent { seq: 0 },
        "an exact same-owner retry adds no second terminal"
    );

    let changed = fact(3, ProviderCompletionDisposition::Truncated)?;
    let outcome = record(
        &mut store,
        &session,
        "evt_033602b20021",
        CREATED_MS,
        &changed,
        &mut [],
    );
    match outcome {
        Err(error) => {
            let error = error.to_string();
            assert!(
                error.contains("diverg") || error.contains("Diverg"),
                "a changed payload under one identity must be refused: {error}"
            );
        }
        Ok(_) => panic!("a changed payload under one event identity must not be accepted"),
    }

    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |row| row.get(0))?;
    assert_eq!(rows, 1, "one Step terminal is one durable row");
    Ok(())
}

/// The store refuses a step-ended payload the codec did not produce.
///
/// Keeps the append seam from becoming a second way to write these bytes: the
/// payload is the codec's, and a hand-built object is not admitted by it.
#[test]
fn a_hand_built_payload_is_not_a_fact() -> TestResult {
    let value = serde_json::json!({ "step": 1, "finish": "stop" });
    assert!(
        session_step_ended::decode(&value).is_err(),
        "the oracle-shaped payload is not this contract"
    );
    Ok(())
}
