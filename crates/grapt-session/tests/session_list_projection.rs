use grapt_session::{
    ParentFilter, SessionListCriteria, SessionListDirection, SessionListLimits, SessionListOrder,
    SessionListProjector, SessionListScope, SessionListStore,
};
use grapt_store::{AggregateId, AppendInput, EventStore, Projector};
use grapt_types::{EventId, EventVersion, SessionId};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn version(value: u32) -> Result<EventVersion, Box<dyn std::error::Error>> {
    EventVersion::new(value).ok_or_else(|| "event version must be positive".into())
}

fn request() -> Result<SessionListCriteria, Box<dyn std::error::Error>> {
    Ok(SessionListCriteria::new(
        SessionListScope::All,
        None,
        None,
        ParentFilter::Any,
        SessionListOrder::Ascending,
        SessionListDirection::Next,
        None,
        128,
        SessionListLimits::production(),
    )?)
}

#[allow(clippy::too_many_arguments)]
fn append(
    store: &mut EventStore,
    projector: &mut SessionListProjector,
    aggregate: &AggregateId,
    event_id: &str,
    kind: &'static str,
    event_version: u32,
    created_ms: i64,
    data: &Value,
) -> TestResult {
    let event: EventId = event_id.parse()?;
    let writer = store.fencing_token(aggregate)?;
    let (_, notices) = store.append(
        AppendInput {
            id: &event,
            aggregate: aggregate.clone(),
            writer,
            kind_name: kind,
            version: version(event_version)?,
            created_ms,
            data,
        },
        &mut [projector],
    )?;
    notices.discard();
    Ok(())
}

#[test]
fn created_uses_info_updated_metadata_uses_event_time_and_activity_preserves_recency() -> TestResult
{
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_recency".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let mut projector = SessionListProjector;

    append(
        &mut store,
        &mut projector,
        &aggregate,
        "evt_recency_create",
        "session.created",
        1,
        999,
        &json!({
            "sessionID": session.as_str(),
            "info": {
                "id": session.as_str(),
                "projectID": "prj_a",
                "directory": "/work/a",
                "title": "before",
                "time": {"created": 100, "updated": 111}
            }
        }),
    )?;
    let first = SessionListStore::over(&mut store).list(&request()?)?;
    assert_eq!(first.items().len(), 1);
    assert_eq!(first.items()[0].key.updated_ms().value(), 111);

    // This durable activity is deliberately outside the recency authority set.
    append(
        &mut store,
        &mut projector,
        &aggregate,
        "evt_recency_usage",
        "session.usage.recorded",
        1,
        1_500,
        &json!({
            "sessionID": session.as_str(),
            "source": "title",
            "cost": 0,
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}}
        }),
    )?;
    assert_eq!(
        SessionListStore::over(&mut store)
            .list(&request()?)?
            .items()[0]
            .key
            .updated_ms()
            .value(),
        111
    );

    append(
        &mut store,
        &mut projector,
        &aggregate,
        "evt_recency_rename",
        "session.renamed",
        1,
        2_000,
        &json!({"sessionID": session.as_str(), "title": "after"}),
    )?;
    let renamed = SessionListStore::over(&mut store).list(&request()?)?;
    assert_eq!(renamed.items()[0].key.updated_ms().value(), 2_000);
    assert_eq!(renamed.items()[0].title, "after");
    Ok(())
}

#[test]
fn fork_move_and_revert_use_only_their_injected_durable_times() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let parent: SessionId = "ses_parent".parse()?;
    let parent_aggregate = AggregateId::Session(parent.clone());
    store.elect_writer(&parent_aggregate)?;
    let mut projector = SessionListProjector;
    append(
        &mut store,
        &mut projector,
        &parent_aggregate,
        "evt_parent_create",
        "session.created",
        1,
        100,
        &json!({"sessionID":parent.as_str(),"info":{"id":parent.as_str(),"projectID":"prj_a","directory":"/a","title":"base","time":{"created":100,"updated":100}}}),
    )?;

    let fork: SessionId = "ses_fork".parse()?;
    let fork_aggregate = AggregateId::Session(fork.clone());
    store.elect_writer(&fork_aggregate)?;
    append(
        &mut store,
        &mut projector,
        &fork_aggregate,
        "evt_fork",
        "session.forked",
        2,
        300,
        &json!({"sessionID":fork.as_str(),"parentID":parent.as_str(),"parentSeq":0}),
    )?;
    append(
        &mut store,
        &mut projector,
        &fork_aggregate,
        "evt_move",
        "session.moved",
        1,
        400,
        &json!({"sessionID":fork.as_str(),"location":{"directory":"/b"},"projectID":"prj_b"}),
    )?;
    append(
        &mut store,
        &mut projector,
        &fork_aggregate,
        "evt_revert",
        "session.revert.cleared",
        1,
        500,
        &json!({"sessionID":fork.as_str()}),
    )?;
    let page = SessionListStore::over(&mut store).list(&request()?)?;
    let item = page
        .items()
        .iter()
        .find(|item| item.key.session_id() == &fork)
        .ok_or("fork missing")?;
    assert_eq!(item.key.updated_ms().value(), 500);
    assert_eq!(item.directory, "/b");
    assert_eq!(item.project_id.as_str(), "prj_b");
    assert_eq!(item.parent_id.as_ref(), Some(&parent));
    assert_eq!(item.title, "base (fork #1)");
    Ok(())
}

#[derive(Debug)]
struct RefuseAfterProjection;

impl Projector for RefuseAfterProjection {
    fn projection(&self) -> grapt_store::ProjectionId {
        grapt_store::ProjectionId::new("session-list-failure-witness")
    }

    fn project(
        &mut self,
        _tx: &rusqlite::Transaction<'_>,
        _event: &grapt_store::DecodedEvent,
    ) -> Result<(), String> {
        Err("injected projector refusal".to_owned())
    }
}

#[test]
fn projector_failure_commits_neither_list_row_fact_watermark_nor_notice() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_atomic".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let event: EventId = "evt_atomic".parse()?;
    let data = json!({"sessionID":session.as_str(),"info":{"id":session.as_str(),"projectID":"prj_a","directory":"/a","title":"atomic","time":{"created":1,"updated":1}}});
    let writer = store.fencing_token(&aggregate)?;
    let mut list = SessionListProjector;
    let mut refusal = RefuseAfterProjection;
    let outcome = store.append(
        AppendInput {
            id: &event,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.created",
            version: version(1)?,
            created_ms: 1,
            data: &data,
        },
        &mut [&mut list, &mut refusal],
    );
    assert!(outcome.is_err(), "the injected projector must refuse");
    let event_rows: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = ?1",
        [event.as_str()],
        |row| row.get(0),
    )?;
    let list_rows: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM session_list_projection WHERE session_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    let watermarks: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM projection_watermark
         WHERE projection = 'session-list-v1' AND aggregate_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!((event_rows, list_rows, watermarks), (0, 0, 0));
    Ok(())
}

#[test]
fn a_lagging_projection_catches_up_from_durable_truth_after_restart() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_catchup".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let event: EventId = "evt_catchup".parse()?;
    let data = json!({"sessionID":session.as_str(),"info":{"id":session.as_str(),"projectID":"prj_a","directory":"/a","title":"catch up","time":{"created":1,"updated":7}}});
    let writer = store.fencing_token(&aggregate)?;
    let (_, notices) = store.append(
        AppendInput {
            id: &event,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.created",
            version: version(1)?,
            created_ms: 1,
            data: &data,
        },
        &mut [],
    )?;
    notices.discard();
    assert!(SessionListStore::over(&mut store)
        .list(&request()?)?
        .items()
        .is_empty());
    let mut projector = SessionListProjector;
    let caught = store.catch_up(&aggregate, &mut projector)?;
    assert_eq!(caught.projected, 1);
    assert_eq!(
        SessionListStore::over(&mut store)
            .list(&request()?)?
            .items()[0]
            .key
            .updated_ms()
            .value(),
        7
    );
    Ok(())
}

#[test]
fn invalid_projected_location_refuses_the_fact_and_projection_atomically() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_invalid_location".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let mut projector = SessionListProjector;
    let event: EventId = "evt_invalid_location".parse()?;
    let writer = store.fencing_token(&aggregate)?;
    let data = json!({"sessionID":session.as_str(),"info":{"id":session.as_str(),"projectID":"prj_a","directory":"relative/path","title":"invalid","time":{"created":1,"updated":1}}});
    let outcome = store.append(
        AppendInput {
            id: &event,
            aggregate,
            writer,
            kind_name: "session.created",
            version: version(1)?,
            created_ms: 1,
            data: &data,
        },
        &mut [&mut projector],
    );
    assert!(outcome.is_err());
    let facts: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE id = ?1",
        [event.as_str()],
        |row| row.get(0),
    )?;
    let rows: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM session_list_projection WHERE session_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!((facts, rows), (0, 0));
    Ok(())
}

#[test]
fn sc01_creation_uses_injected_info_updated() -> TestResult {
    created_uses_info_updated_metadata_uses_event_time_and_activity_preserves_recency()
}
#[test]
fn sc02_activity_preserves_recency() -> TestResult {
    created_uses_info_updated_metadata_uses_event_time_and_activity_preserves_recency()
}
#[test]
fn sc21_projection_row_and_watermark_settle_together() -> TestResult {
    created_uses_info_updated_metadata_uses_event_time_and_activity_preserves_recency()
}
#[test]
fn sc24_projection_identity_keys_the_matching_watermark() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_identity".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    let mut projector = SessionListProjector;
    append(
        &mut store,
        &mut projector,
        &aggregate,
        "evt_identity",
        "session.created",
        1,
        1,
        &json!({"sessionID":session.as_str(),"info":{"id":session.as_str(),"projectID":"prj_a","directory":"/a","title":"identity","time":{"created":1,"updated":1}}}),
    )?;
    let matching: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM projection_watermark
         WHERE projection = 'session-list-v1'
           AND aggregate_family = 'session' AND aggregate_id = ?1",
        [session.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(
        matching, 1,
        "the list row must carry its matching watermark identity"
    );
    Ok(())
}
