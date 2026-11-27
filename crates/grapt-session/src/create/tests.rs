//! Witnesses for durable session create.
//!
//! These drive the real append chokepoint against an in-memory store with the
//! real session-list projector attached, so "create and its projection commit
//! together" is exercised rather than asserted.

use super::*;
use crate::selection::{select_agent, select_model, SelectionError};
use grapt_store::EventStore;
use grapt_types::{ClockAuthority, IdAuthority, IdToken, Timestamp};
use std::sync::atomic::{AtomicU64, Ordering};

#[derive(Debug)]
struct FrozenClock(i64);

#[test]
fn local_requested_session_id_is_exact_bounded_and_never_adopted(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = store();
    let clock = FrozenClock(123);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let requested: SessionId = "ses_se00112233445566778899aabb".parse()?;
    let requested_seed = seed().with_requested_id(requested.clone())?;
    let mut projector = crate::SessionListProjector;
    let created = create_session(
        &mut store,
        &authorities,
        &requested_seed,
        &mut [&mut projector],
    )?;
    assert_eq!(created.session, requested);
    assert_eq!(
        SessionStore::over(&store).record(&requested)?.session,
        requested
    );
    let projected: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM session_list_projection WHERE session_id=?1",
        [requested.as_str()],
        |row| row.get(0),
    )?;
    assert_eq!(projected, 1);
    assert!(matches!(
        create_session(
            &mut store,
            &authorities,
            &requested_seed,
            &mut [&mut projector]
        ),
        Err(SessionCreateError::Store {
            source: WriteError::AggregateClaimed { .. }
        })
    ));
    let long: SessionId = format!("ses_{}", "a".repeat(MAX_REQUESTED_ID_BYTES)).parse()?;
    assert!(matches!(
        seed().with_requested_id(long),
        Err(SessionCreateError::Seed { field: "id", .. })
    ));
    Ok(())
}

#[test]
fn local_failed_requested_create_cannot_resurrect_its_claim(
) -> Result<(), Box<dyn std::error::Error>> {
    struct RefuseProjection;
    impl Projector for RefuseProjection {
        fn projection(&self) -> grapt_store::ProjectionId {
            grapt_store::ProjectionId::new("requested-create-failure")
        }
        fn project(
            &mut self,
            _tx: &rusqlite::Transaction<'_>,
            _event: &grapt_store::DecodedEvent,
        ) -> Result<(), String> {
            Err("intentional requested-create projection failure".to_owned())
        }
    }
    let mut store = store();
    let clock = FrozenClock(123);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seed = seed().with_requested_id("ses_se00112233445566778899aabb".parse()?)?;
    assert!(matches!(
        create_session(
            &mut store,
            &authorities,
            &seed,
            &mut [&mut RefuseProjection]
        ),
        Err(SessionCreateError::Store {
            source: WriteError::Projector { name, message }
        }) if name == "requested-create-failure"
            && message == "intentional requested-create projection failure"
    ));
    let events: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(events, 0, "failed projection must roll back the seed");
    assert!(matches!(
        create_session(&mut store, &authorities, &seed, &mut []),
        Err(SessionCreateError::Store {
            source: WriteError::AggregateClaimed { .. }
        })
    ));
    Ok(())
}

#[test]
fn selections_commit_to_the_same_session_and_model_retry_adds_no_event(
) -> Result<(), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let clock = FrozenClock(123);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::SessionListProjector;
    let created = create_session(&mut store, &authorities, &seed(), &mut [&mut projector])?;
    let agent = grapt_types::AgentId::new("agt_selected").ok_or("agent identity empty")?;
    let model = json!({"providerID":"openai", "id":"selected"});
    select_agent(
        &mut store,
        &authorities,
        &created.session,
        &agent,
        &mut [&mut projector],
    )?;
    select_model(
        &mut store,
        &authorities,
        &created.session,
        &model,
        &mut [&mut projector],
    )?;
    let record = SessionStore::over(&store).record(&created.session)?;
    assert_eq!(record.agent.as_deref(), Some("agt_selected"));
    assert_eq!(record.model, Some(model.clone()));
    let event_count = |store: &EventStore| -> rusqlite::Result<i64> {
        store
            .connection()
            .query_row("SELECT count(*) FROM event", [], |row| row.get(0))
    };
    let before = event_count(&store)?;
    select_model(
        &mut store,
        &authorities,
        &created.session,
        &json!({"providerID":"openai", "id":"selected", "variant":"default"}),
        &mut [&mut projector],
    )?;
    assert_eq!(event_count(&store)?, before);
    let unknown = "ses_unknown".parse()?;
    assert!(matches!(
        select_model(
            &mut store,
            &authorities,
            &unknown,
            &model,
            &mut [&mut projector]
        ),
        Err(SelectionError::Read(
            crate::SessionStoreError::Unknown { .. }
        ))
    ));
    assert!(matches!(
        select_model(
            &mut store,
            &authorities,
            &created.session,
            &json!({"providerID":"openai", "id":"selected", "extra":true}),
            &mut [&mut projector]
        ),
        Err(SelectionError::Invalid(_))
    ));
    assert_eq!(
        event_count(&store)?,
        before,
        "refused selection wrote an event"
    );
    Ok(())
}

impl ClockAuthority for FrozenClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0)
    }
}

#[derive(Debug, Default)]
struct CountingIds {
    next: AtomicU64,
}

impl IdAuthority for CountingIds {
    fn mint(&self) -> IdToken {
        let value = self.next.fetch_add(1, Ordering::SeqCst);
        match IdToken::new(format!("{value:012x}")) {
            Some(token) => token,
            None => panic!("a hex counter renders non-empty"),
        }
    }
}

/// An authority that mints ONE token forever.
///
/// Not a contrived shape: it is exactly what a broken or badly seeded identity
/// adapter looks like from the create path's side, and the whole point of
/// `AlreadyCreated` is that this case is caught rather than silently adopting a
/// live Session.
#[derive(Debug)]
struct RepeatingIds(&'static str);

impl IdAuthority for RepeatingIds {
    fn mint(&self) -> IdToken {
        match IdToken::new(self.0) {
            Some(token) => token,
            None => panic!("the repeated token is non-empty"),
        }
    }
}

/// An authority whose tokens cannot form a validated identity.
#[derive(Debug)]
struct UnusableIds;

impl IdAuthority for UnusableIds {
    fn mint(&self) -> IdToken {
        match IdToken::new("has-a-hyphen") {
            Some(token) => token,
            None => panic!("the token is non-empty"),
        }
    }
}

fn store() -> EventStore {
    match EventStore::open_in_memory() {
        Ok(store) => store,
        Err(error) => panic!("an in-memory store must open: {error}"),
    }
}

fn project() -> ProjectId {
    match ProjectId::new("global".to_owned()) {
        Some(project) => project,
        None => panic!("global is a non-empty project identity"),
    }
}

fn seed() -> SessionSeed {
    match SessionSeed::new(project(), "/work/df0", "dogfood zero") {
        Ok(seed) => seed,
        Err(error) => panic!("the seed must validate: {error}"),
    }
}

/// A stated launch mode is written durably beside the seed and reads back.
///
/// This drives the real create path, so it covers the append itself rather than
/// a staged row: the fact must land in the SAME transaction as the seed, and a
/// reader must see it immediately.
#[test]
fn a_stated_launch_mode_is_written_with_the_seed_and_reads_back() {
    let mut store = store();
    let clock = FrozenClock(1_784_090_756_114);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let seed = seed().with_launch(SessionLaunch::Headless);
    let created = match create_session(&mut store, &authorities, &seed, &mut []) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    match SessionStore::over(&store).record(&created.session) {
        Ok(record) => assert_eq!(record.launch, Some(SessionLaunch::Headless)),
        Err(error) => panic!("the record must read back: {error}"),
    }
    // The seed stays sequence zero; the launch fact follows it.
    assert_eq!(
        created.seq, 0,
        "the seed remains the aggregate's first fact"
    );
}

/// A create that states no mode records none, leaving absence distinct from a
/// windowed launch so the reader's fallback can decide.
#[test]
fn an_unstated_launch_mode_records_no_fact() {
    let mut store = store();
    let clock = FrozenClock(1_784_090_756_114);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let created = match create_session(&mut store, &authorities, &seed(), &mut []) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    match SessionStore::over(&store).record(&created.session) {
        Ok(record) => assert_eq!(record.launch, None),
        Err(error) => panic!("the record must read back: {error}"),
    }
}

/// The success cut: one seed, one aggregate, one sequence-zero fact, and the
/// read view agrees immediately.
#[test]
fn create_seeds_the_aggregate_and_the_read_view_sees_it() {
    let mut store = store();
    let clock = FrozenClock(1_784_090_756_114);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let created = match create_session(&mut store, &authorities, &seed(), &mut []) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    assert_eq!(created.seq, 0, "the seed is the aggregate's first fact");
    assert_eq!(created.created_ms, 1_784_090_756_114);

    let view = SessionStore::over(&store);
    match view.exists(&created.session) {
        Ok(exists) => assert!(exists),
        Err(error) => panic!("the read view must answer: {error}"),
    }
    match view.placement(&created.session) {
        Ok(placement) => assert_eq!(placement.directory(), "/work/df0"),
        Err(error) => panic!("placement must read back: {error}"),
    }
    match view.record(&created.session) {
        Ok(record) => {
            assert_eq!(record.title, "dogfood zero");
            assert_eq!(record.created_ms, 1_784_090_756_114);
            assert_eq!(record.parent, None);
        }
        Err(error) => panic!("the record must read back: {error}"),
    }
}

/// THE DUPLICATE-IDENTITY REFUSAL. An authority that repeats a token would
/// otherwise hand the second caller the FIRST caller's live Session. Create
/// refuses by name rather than reporting success over somebody else's data.
#[test]
fn a_repeated_identity_refuses_rather_than_adopting_the_existing_session() {
    let mut store = store();
    let clock = FrozenClock(1_784_090_756_114);
    let ids = RepeatingIds("df0fixed");
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let first = match create_session(&mut store, &authorities, &seed(), &mut []) {
        Ok(created) => created,
        Err(error) => panic!("the first create must succeed: {error}"),
    };
    let second = create_session(&mut store, &authorities, &seed(), &mut []);
    match second {
        Err(SessionCreateError::AlreadyCreated { session }) => {
            assert_eq!(session, first.session.as_str());
        }
        other => panic!("a repeated identity must refuse, got {other:?}"),
    }
    // And the refusal wrote NOTHING: the aggregate still holds one fact.
    let count: i64 = match store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session' AND aggregate_id = ?1",
        [first.session.as_str()],
        |row| row.get(0),
    ) {
        Ok(count) => count,
        Err(error) => panic!("the count must read: {error}"),
    };
    assert_eq!(count, 1);
}

/// An identity authority that cannot produce a valid token is a TYPED failure,
/// never a repaired identity and never a panic.
#[test]
fn an_unusable_identity_token_is_a_typed_refusal() {
    let mut store = store();
    let clock = FrozenClock(1);
    let ids = UnusableIds;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    match create_session(&mut store, &authorities, &seed(), &mut []) {
        Err(SessionCreateError::Identity { .. }) => {}
        other => panic!("an unusable token must refuse by type, got {other:?}"),
    }
}

/// Create and its projection commit TOGETHER. With the session-list projector
/// attached, the list row exists the moment create returns.
#[test]
fn create_and_its_list_projection_commit_in_one_transaction() {
    // `open_in_memory` prepares the schema, so the projection table is
    // already there: this witness needs no migration step of its own.
    let mut store = store();
    let clock = FrozenClock(1_784_090_756_114);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let mut projector = crate::list::SessionListProjector;
    let created = match create_session(&mut store, &authorities, &seed(), &mut [&mut projector]) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    let title: String = match store.connection().query_row(
        "SELECT title FROM session_list_projection WHERE session_id = ?1",
        [created.session.as_str()],
        |row| row.get(0),
    ) {
        Ok(title) => title,
        Err(error) => panic!("the projected row must exist: {error}"),
    };
    assert_eq!(title, "dogfood zero");
}

/// A seed carrying every optional coordinate round-trips through the record
/// read, and a model that is not an object is refused rather than stored.
#[test]
fn optional_seed_coordinates_survive_and_a_non_object_model_refuses() {
    let mut store = store();
    let clock = FrozenClock(7);
    let ids = CountingIds::default();
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    let parent = match create_session(&mut store, &authorities, &seed(), &mut []) {
        Ok(created) => created.session,
        Err(error) => panic!("the parent must be created: {error}"),
    };
    let full = match seed()
        .with_slug("quick-otter")
        .and_then(|seed| seed.with_agent("build"))
        .and_then(|seed| seed.with_model(serde_json::json!({"id": "m1", "providerID": "p1"})))
    {
        Ok(seed) => seed.with_parent(parent.clone()),
        Err(error) => panic!("the full seed must validate: {error}"),
    };
    let created = match create_session(&mut store, &authorities, &full, &mut []) {
        Ok(created) => created,
        Err(error) => panic!("create must succeed: {error}"),
    };
    let record = match SessionStore::over(&store).record(&created.session) {
        Ok(record) => record,
        Err(error) => panic!("the record must read back: {error}"),
    };
    assert_eq!(record.slug.as_deref(), Some("quick-otter"));
    assert_eq!(record.agent.as_deref(), Some("build"));
    assert_eq!(record.parent, Some(parent));
    assert!(record.model.is_some());

    match seed().with_model(serde_json::json!("m1")) {
        Err(SessionCreateError::Seed { field, .. }) => assert_eq!(field, "model"),
        other => panic!("a non-object model must refuse, got {other:?}"),
    }
}

/// Agent is bounded like every other durable text coordinate.
///
/// It carried an emptiness check and nothing else, which made it the one seed
/// string with no ceiling — not a smaller version of the rule, the absence of
/// it. The value reaches durable canonical bytes and the bounded list page, so
/// an unbounded agent is an unbounded durable write.
#[test]
fn an_agent_past_its_bound_is_refused() {
    match seed().with_agent("a".repeat(MAX_AGENT_BYTES + 1)) {
        Err(SessionCreateError::Seed { field, reason }) => {
            assert_eq!(field, "agent");
            assert!(reason.contains("the bound is"), "got: {reason}");
        }
        other => panic!("an oversized agent must refuse, got {other:?}"),
    }
    // And the bound itself is admitted, so the guard refuses OVERSIZE rather
    // than refusing agents.
    match seed().with_agent("a".repeat(MAX_AGENT_BYTES)) {
        Ok(_) => {}
        Err(error) => panic!("an agent AT the bound must validate: {error}"),
    }
    match seed().with_agent("") {
        Err(SessionCreateError::Seed { field, .. }) => assert_eq!(field, "agent"),
        other => panic!("an empty agent must still refuse, got {other:?}"),
    }
}

/// The model is validated MEMBER BY MEMBER against the frozen oracle's shape.
///
/// "Is it an object" was the whole check, so every one of these reached
/// `info` and was cloned into the durable event. The oracle stores exactly
/// `{id, providerID, variant?}` and each member is a non-empty string; nothing
/// else is a model selection.
#[test]
fn a_model_that_is_not_the_oracle_s_shape_is_refused() {
    let refusals = [
        serde_json::json!({"providerID": "p1"}),
        serde_json::json!({"id": "m1"}),
        serde_json::json!({"id": "", "providerID": "p1"}),
        serde_json::json!({"id": 7, "providerID": "p1"}),
        serde_json::json!({"id": "m1", "providerID": "p1", "variant": 3}),
        serde_json::json!({"id": "m1", "providerID": "p1", "smuggled": "x"}),
    ];
    for model in refusals {
        match seed().with_model(model.clone()) {
            Err(SessionCreateError::Seed { field, .. }) => assert_eq!(field, "model"),
            other => panic!("{model} must refuse, got {other:?}"),
        }
    }
    // The oracle's own shape, with and without the optional variant.
    for model in [
        serde_json::json!({"id": "m1", "providerID": "p1"}),
        serde_json::json!({"id": "m1", "providerID": "p1", "variant": "high"}),
    ] {
        match seed().with_model(model.clone()) {
            Ok(_) => {}
            Err(error) => panic!("{model} is the oracle's shape: {error}"),
        }
    }
}

/// The model carries explicit BYTE and DEPTH bounds before it is cloned.
///
/// A member-shape check alone does not bound anything: `variant` is a string
/// and a string can be a megabyte, and a byte bound alone does not stop a small
/// deeply nested object from costing stack in every consumer that walks it.
/// Both are checked BEFORE `info` clones the value into the durable event, so
/// the cost is refused rather than paid and then discovered.
#[test]
fn a_model_past_its_byte_or_depth_bound_is_refused() {
    let oversized = serde_json::json!({
        "id": "m1",
        "providerID": "p1",
        "variant": "v".repeat(MAX_MODEL_BYTES),
    });
    match seed().with_model(oversized) {
        Err(SessionCreateError::Seed { field, reason }) => {
            assert_eq!(field, "model");
            assert!(reason.contains("the bound is"), "got: {reason}");
        }
        other => panic!("an oversized model must refuse, got {other:?}"),
    }
    // Nested, and SMALL: the byte bound alone would pass this one, which is
    // why the depth bound is a separate coordinate rather than a consequence.
    let nested = serde_json::json!({
        "id": "m1",
        "providerID": "p1",
        "variant": {"deeper": "still"},
    });
    match seed().with_model(nested) {
        Err(SessionCreateError::Seed { field, reason }) => {
            assert_eq!(field, "model");
            // The DEPTH bound, named. Asserting only "refused" would be green
            // against a deleted depth check, because the member walk refuses
            // the same input for being a non-string `variant` — two guards,
            // one input, and only the reason tells them apart.
            assert!(reason.contains("nests"), "got: {reason}");
        }
        other => panic!("a nested model must refuse on DEPTH, got {other:?}"),
    }
}
