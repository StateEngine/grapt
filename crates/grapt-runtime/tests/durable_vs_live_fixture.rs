//! Fixture 12 — durable-versus-live separation, EXECUTED against this candidate.
//!
//! # The law this file exists to fail
//!
//! Advisory live notices are bounded and lossy by contract
//! ([`grapt_store::MAX_PENDING_NOTICES`], [`grapt_store::Notifier::notify`]
//! returning nothing). Durable truth is complete. Therefore a consumer that
//! received **no** notices, or a **truncated** set of them, and then re-lists
//! from its own cursor over the durable log, converges on exactly the same
//! committed projection as a consumer that received every notice — and as a
//! from-scratch replay of the serialized log into a fresh database. Advisory
//! loss costs latency, never truth.
//!
//! # What is oracle-backed and what is SYNTHETIC
//!
//! The reconnect half runs the **frozen** fixture-12 corpus
//! `fixtures/rust-reconstruction/reconnect-cursor-convergence-replay`, read
//! through the shared fixture resolver rather than copied here. Its five
//! `session.input.admitted.1` rows, its watermark and its `replayedPending`
//! projection are oracle truth captured from the real oracle engine at
//! `42e4888cc0`.
//!
//! The `session.tool.terminal.1` half is **SYNTHETIC**. Fixture 12 contains no
//! tool-terminal row, and neither does any other scenario in the frozen corpus.
//! Its witness is built on a separate synthetic session id so that it can never
//! be read as fixture-12 oracle truth, and every assertion about it is labelled
//! synthetic here, in `R2F12_RESIDUALS.md` and in the author receipt.
//!
//! # What this lane does NOT claim
//!
//! There is no production `Notifier` implementor in this workspace at this
//! base. The subscriber below is a **test double**, so these witnesses prove the
//! STORE contract that makes any fanout safe — not that a production fanout
//! exists or was exercised. The bounded lossy fanout with per-subscriber
//! interests belongs to the paused `reconnect-feed` lane and the R4 API,
//! and is untouched here.
//!
//! This lane adds no authoritative `session.tool.terminal.1` projection, no
//! `ProjectionId` for one, no table, no migration and no event kind. "Replay"
//! for the tool terminal means durable-log replay plus strict decode into a
//! fresh database, which is the ruled narrow reading.

use grapt_event::session_tool_terminal::{self, ToolTerminalFact};
use grapt_inbox::{InboxEvent, InboxFold};
use grapt_store::{
    AggregateId, AppendInput, Appended, CommittedNotices, DecodedEvent, DurableNotice, EventStore,
    Notifier, ProjectionId, Projector, WriteError, MAX_PENDING_NOTICES,
};
use grapt_types::{
    EventId, EventVersion, MessageId, PhysicalAttemptId, SessionId, ToolCallId, ToolTerminalOutcome,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[path = "../../../test-support/fixtures.rs"]
mod fixtures;

type TestResult = Result<(), Box<dyn std::error::Error>>;
type Fallible<T> = Result<T, Box<dyn std::error::Error>>;

/// The aggregate family literal the store keys session rows on.
///
/// `grapt_store::election::family_of` is crate-private, so a consumer-side
/// re-list has to spell it. It is asserted rather than assumed: the first
/// witness below reads the column back for every row it appended, so a family
/// rename would fail here instead of silently selecting nothing.
const SESSION_FAMILY: &str = "session";

/// This subscriber's own projection identity. Named for the lane so it cannot
/// collide with a production projection's watermark.
const FIXTURE12_INBOX: ProjectionId = ProjectionId::new("fixture12-durable-vs-live-inbox");

// ---------------------------------------------------------------------------
// The frozen corpus
// ---------------------------------------------------------------------------

fn corpus_root() -> PathBuf {
    fixtures::fixture_path(Path::new("reconnect-cursor-convergence-replay"))
}

fn read_json(path: &Path) -> Fallible<Value> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    serde_json::from_str(&text)
        .map_err(|error| format!("{} is not valid JSON: {error}", path.display()).into())
}

fn read_jsonl(path: &Path) -> Fallible<Vec<Value>> {
    let text = std::fs::read_to_string(path)
        .map_err(|error| format!("{} is unreadable: {error}", path.display()))?;
    text.lines()
        .enumerate()
        .filter(|(_, line)| !line.trim().is_empty())
        .map(|(index, line)| {
            serde_json::from_str(line)
                .map_err(|error| format!("{} line {}: {error}", path.display(), index + 1).into())
        })
        .collect()
}

/// One durable row, carried verbatim: identity, sequence, versioned type,
/// creation stamp and payload.
///
/// The waiver permits normalising engine-minted ids and timestamps by ordinal.
/// This lane deliberately does NOT use that permission — it appends the frozen
/// bytes exactly, which is strictly stronger than the waiver allows and
/// therefore legal.
#[derive(Debug, Clone)]
struct FrozenEvent {
    id: EventId,
    seq: u64,
    kind_name: String,
    version: EventVersion,
    created_ms: i64,
    data: Value,
}

fn split_versioned_type(raw: &str) -> Fallible<(String, EventVersion)> {
    let (name, version) = raw
        .rsplit_once('.')
        .ok_or_else(|| format!("{raw:?} is not a versioned durable type"))?;
    let value: u32 = version
        .parse()
        .map_err(|error| format!("{raw:?} has an unparseable version: {error}"))?;
    let version =
        EventVersion::new(value).ok_or_else(|| format!("{raw:?} declares version zero"))?;
    Ok((name.to_owned(), version))
}

fn required_str<'v>(value: &'v Value, field: &str) -> Fallible<&'v str> {
    value
        .get(field)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("required string field `{field}` is missing").into())
}

fn frozen_durable_log() -> Fallible<Vec<FrozenEvent>> {
    let rows = read_jsonl(&corpus_root().join("expected/durable.jsonl"))?;
    let mut out = Vec::with_capacity(rows.len());
    for row in &rows {
        let (kind_name, version) = split_versioned_type(required_str(row, "type")?)?;
        let seq = row
            .get("seq")
            .and_then(Value::as_u64)
            .ok_or("every frozen row must carry a non-negative seq")?;
        let created_ms = row
            .get("created")
            .and_then(Value::as_i64)
            .ok_or("every frozen row must carry a created stamp")?;
        let data = row
            .get("data")
            .ok_or("every frozen row must carry a payload")?
            .clone();
        out.push(FrozenEvent {
            id: required_str(row, "id")?.parse()?,
            seq,
            kind_name,
            version,
            created_ms,
            data,
        });
    }
    Ok(out)
}

/// The scenario's session, taken from the frozen rows rather than hard-coded,
/// and proven to be the SAME session on every row.
fn frozen_session(log: &[FrozenEvent]) -> Fallible<SessionId> {
    let first = log
        .first()
        .ok_or("the frozen durable log must not be empty")?;
    let session: SessionId = required_str(&first.data, "sessionID")?.parse()?;
    for event in log {
        let each: SessionId = required_str(&event.data, "sessionID")?.parse()?;
        assert_eq!(
            each,
            session,
            "fixture 12 is a single-aggregate scenario; row {} names another session",
            event.id.as_str()
        );
    }
    Ok(session)
}

/// `expected/state.json`: the client's watermark at the scenario's cut point,
/// and the pending projection a from-scratch replay must reproduce.
#[derive(Debug)]
struct ExpectedState {
    watermark: u64,
    replayed_pending: Vec<(MessageId, u64)>,
}

fn expected_state() -> Fallible<ExpectedState> {
    let state = read_json(&corpus_root().join("expected/state.json"))?;
    let watermark = state
        .get("watermark")
        .and_then(Value::as_u64)
        .ok_or("expected/state.json must declare a watermark")?;
    let rows = state
        .get("replayedPending")
        .and_then(Value::as_array)
        .ok_or("expected/state.json must declare replayedPending")?;
    let mut replayed_pending = Vec::with_capacity(rows.len());
    for row in rows {
        let id: MessageId = required_str(row, "id")?.parse()?;
        let seq = row
            .get("admitted_seq")
            .and_then(Value::as_u64)
            .ok_or("every replayedPending row must carry admitted_seq")?;
        replayed_pending.push((id, seq));
    }
    Ok(ExpectedState {
        watermark,
        replayed_pending,
    })
}

// ---------------------------------------------------------------------------
// The test-only advisory subscriber
// ---------------------------------------------------------------------------

/// A test double for the advisory path.
///
/// It exists because this workspace has NO production `Notifier` implementor at
/// this base — see the module docs. It records what the live path delivered so
/// the witnesses can assert what a live-only consumer would and would not know.
#[derive(Debug, Default)]
struct LiveSubscriber {
    seen: Vec<u64>,
    drop_reports: Vec<u32>,
}

impl Notifier for LiveSubscriber {
    fn notify(&mut self, notice: &DurableNotice) {
        self.seen.push(notice.seq());
    }

    fn dropped(&mut self, count: u32) {
        self.drop_reports.push(count);
    }
}

/// The reconnecting subscriber's read model.
///
/// The fold is in-memory — it is a CLIENT projection, not a durable table — but
/// its watermark is durable, which is what makes `catch_up` a real reconnect
/// rather than a re-read of the caller's own position.
#[derive(Debug, Default)]
struct InboxSubscriberProjection {
    fold: InboxFold,
    projected: Vec<u64>,
}

impl Projector for InboxSubscriberProjection {
    fn projection(&self) -> ProjectionId {
        FIXTURE12_INBOX
    }

    fn project(
        &mut self,
        _tx: &rusqlite::Transaction<'_>,
        event: &DecodedEvent,
    ) -> Result<(), String> {
        let decoded =
            InboxEvent::decode(event.kind_name(), event.kind_version().value(), &event.data)
                .map_err(|error| format!("{}: {error}", event.id.as_str()))?;
        if let Some(inbox) = decoded {
            self.fold.apply(inbox, event.seq.value());
        }
        self.projected.push(event.seq.value());
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Store helpers
// ---------------------------------------------------------------------------

fn store_with_elected_writer(session: &SessionId) -> Fallible<(EventStore, AggregateId)> {
    let mut store = EventStore::open_in_memory()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    Ok((store, aggregate))
}

fn append_frozen(
    store: &mut EventStore,
    aggregate: &AggregateId,
    event: &FrozenEvent,
) -> Fallible<(Appended, CommittedNotices)> {
    let writer = store.fencing_token(aggregate)?;
    let appended = store.append(
        AppendInput {
            id: &event.id,
            aggregate: aggregate.clone(),
            writer,
            kind_name: &event.kind_name,
            version: event.version,
            created_ms: event.created_ms,
            data: &event.data,
        },
        &mut [],
    )?;
    Ok(appended)
}

/// The reconnecting subscriber's own durable re-list, from ITS cursor.
///
/// Strictly greater, never greater-or-equal: the cursor names the last sequence
/// the subscriber already holds, so delivering it again is the duplicate the
/// fixture's "no duplicates or gaps" clause exists to catch. `-1` for an absent
/// cursor rather than `0`, because sequences are zero-based and a subscriber
/// that has seen nothing must still receive the event at sequence zero.
fn re_list_after(
    store: &EventStore,
    session: &SessionId,
    after: Option<u64>,
) -> Fallible<Vec<u64>> {
    let lower: i64 = match after {
        None => -1,
        Some(mark) => i64::try_from(mark)?,
    };
    let mut statement = store.connection().prepare(
        "SELECT seq FROM event
         WHERE aggregate_family = ?1 AND aggregate_id = ?2 AND seq > ?3
         ORDER BY seq ASC",
    )?;
    let rows = statement.query_map(
        rusqlite::params![SESSION_FAMILY, session.as_str(), lower],
        |row| row.get::<_, i64>(0),
    )?;
    let mut out = Vec::new();
    for row in rows {
        out.push(u64::try_from(row?)?);
    }
    Ok(out)
}

/// Serializes one aggregate's whole durable log, verbatim and kind-blind.
///
/// Kind-blind on purpose: a replay that only carries the kinds its own
/// projector understands silently drops every other durable fact, and the
/// database it rebuilds is not the one it replayed.
fn serialize_log(store: &EventStore, session: &SessionId) -> Fallible<Vec<FrozenEvent>> {
    let mut statement = store.connection().prepare(
        "SELECT id, seq, created, type, data FROM event
         WHERE aggregate_family = ?1 AND aggregate_id = ?2
         ORDER BY seq ASC",
    )?;
    let rows = statement.query_map(rusqlite::params![SESSION_FAMILY, session.as_str()], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (id, seq, created_ms, versioned_type, data_text) = row?;
        let (kind_name, version) = split_versioned_type(&versioned_type)?;
        out.push(FrozenEvent {
            id: id.parse()?,
            seq: u64::try_from(seq)?,
            kind_name,
            version,
            created_ms,
            data: serde_json::from_str(&data_text)?,
        });
    }
    Ok(out)
}

/// Replays a serialized log into `target`, proving every row is genuinely new.
///
/// The `Committed` assertion is the freshness proof. A replay pointed at the
/// database it came from reconciles every row as `Idempotent` and then agrees
/// with itself perfectly, which is exactly the shape a from-scratch replay
/// witness must be unable to mistake for success.
fn replay_log_into(
    target: &mut EventStore,
    target_aggregate: &AggregateId,
    rows: &[FrozenEvent],
) -> Fallible<()> {
    for row in rows {
        let (appended, notices) = append_frozen(target, target_aggregate, row)?;
        assert!(
            matches!(appended, Appended::Committed { .. }),
            "a from-scratch replay must COMMIT every row; {} at seq {} reconciled as {appended:?}, \
             so the target database was not empty and the replay is agreeing with itself",
            row.id.as_str(),
            row.seq
        );
        notices.discard();
    }
    Ok(())
}

/// Compares a fold against `expected/state.json` ROW BY ROW.
///
/// Row by row, never by length: a comparison that only counted would pass for a
/// fold whose `admitted_seq` values were all wrong, and `admitted_seq` is the
/// exact durable coordinate the oracle stores.
fn assert_pending_matches(fold: &InboxFold, expected: &[(MessageId, u64)], label: &str) {
    assert_eq!(
        fold.pending.len(),
        expected.len(),
        "{label}: the pending projection has {} rows, the oracle declares {}",
        fold.pending.len(),
        expected.len()
    );
    for (id, admitted_seq) in expected {
        let row = match fold.pending.get(id) {
            Some(row) => row,
            None => panic!("{label}: the oracle's pending row {id} is absent from the fold"),
        };
        assert_eq!(
            row.admitted_seq.value(),
            *admitted_seq,
            "{label}: {id} folded admitted_seq {}, the oracle declares {admitted_seq}",
            row.admitted_seq.value()
        );
    }
    assert!(
        fold.violations.is_empty(),
        "{label}: the fold reported violations: {:?}",
        fold.violations
    );
    assert!(
        fold.unmatched_promotions.is_empty(),
        "{label}: the fold reported unmatched promotions: {:?}",
        fold.unmatched_promotions
    );
}

// ---------------------------------------------------------------------------
// 1. Identity of the corpus actually executed
// ---------------------------------------------------------------------------

#[test]
fn the_executed_corpus_is_the_frozen_fixture_twelve() -> TestResult {
    let root = corpus_root();
    let manifest = read_json(&root.join("manifest.json"))?;

    assert_eq!(
        manifest["id"], "reconnect-cursor-convergence-replay",
        "this lane executes fixture 12 and nothing else"
    );
    assert_eq!(
        manifest["$schema"], "../oracle-receipt-manifest.schema.json",
        "fixture 12 is a bounded oracle-receipt capture, not a full replay manifest"
    );
    assert_eq!(
        manifest["oracle"]["commit"], "42e4888cc0",
        "the frozen oracle commit is the ratified one"
    );

    for relative in [
        "actions.jsonl",
        "expected/durable.jsonl",
        "expected/state.json",
    ] {
        assert!(
            root.join(relative).is_file(),
            "declared corpus path {relative} does not exist"
        );
    }

    // The durable half of the corpus is exactly what the witnesses below rely
    // on: five admissions, zero tool-terminal rows. The second half of that
    // sentence is why the tool-terminal witness is synthetic.
    let log = frozen_durable_log()?;
    assert_eq!(log.len(), 5, "fixture 12 declares five durable events");
    for (index, event) in log.iter().enumerate() {
        assert_eq!(
            event.seq, index as u64,
            "frozen sequences are 0..4 in order"
        );
        assert_eq!(
            event.kind_name, "session.input.admitted",
            "every frozen fixture-12 row is an admission"
        );
        assert_eq!(event.version.value(), 1);
    }

    let expected = expected_state()?;
    assert_eq!(expected.watermark, 2);
    assert_eq!(expected.replayed_pending.len(), 5);
    Ok(())
}

// ---------------------------------------------------------------------------
// 2. The separation itself: a deaf subscriber converges from durable truth
// ---------------------------------------------------------------------------

#[test]
fn a_deaf_subscriber_converges_by_re_listing_while_the_live_path_stays_short() -> TestResult {
    let log = frozen_durable_log()?;
    let session = frozen_session(&log)?;
    let expected = expected_state()?;
    let (mut store, aggregate) = store_with_elected_writer(&session)?;

    let mut live = LiveSubscriber::default();
    let mut subscriber = InboxSubscriberProjection::default();

    // Ordinals 1-3: three admissions, with the subscriber CONNECTED.
    for event in &log[..3] {
        let (appended, notices) = append_frozen(&mut store, &aggregate, event)?;
        assert!(matches!(appended, Appended::Committed { .. }));
        notices.publish(&mut live);
    }
    assert_eq!(
        live.seen,
        vec![0, 1, 2],
        "a connected subscriber is named every committed fact"
    );
    assert!(
        live.drop_reports.is_empty(),
        "a lossless publication must not report a drop"
    );

    // Ordinal 4: the initial full list, and the cursor it lands on.
    assert_eq!(
        re_list_after(&store, &session, None)?,
        vec![0, 1, 2],
        "the initial list is the full durable log"
    );
    let caught = store.catch_up(&aggregate, &mut subscriber)?;
    assert_eq!(caught.projected, 3);
    assert_eq!(
        caught.watermark,
        Some(expected.watermark),
        "the initial list lands the oracle's watermark"
    );
    assert!(
        caught.notices.is_empty(),
        "re-projecting durable facts names no new fact, so it publishes none"
    );
    caught.notices.discard();
    assert_eq!(subscriber.projected, vec![0, 1, 2]);

    // Ordinals 5-6: two more admissions while the subscriber is DISCONNECTED.
    // Its notices are discarded rather than published, which is the only thing
    // that makes the live path short.
    for event in &log[3..] {
        let (appended, notices) = append_frozen(&mut store, &aggregate, event)?;
        assert!(matches!(appended, Appended::Committed { .. }));
        notices.discard();
    }

    // The failure counterexample. Without this the positive witness below would
    // pass just as well if the notice path had secretly been complete, and the
    // separation would be unwitnessed.
    assert_eq!(
        live.seen,
        vec![0, 1, 2],
        "the live-only consumer must be MISSING seq 3 and 4; it saw {:?}",
        live.seen
    );

    // Ordinal 7: reconnect from the subscriber's own cursor.
    assert_eq!(
        re_list_after(&store, &session, Some(expected.watermark))?,
        vec![3, 4],
        "reconnect yields exactly the missed events: no duplicate at the watermark, no gap"
    );

    let caught = store.catch_up(&aggregate, &mut subscriber)?;
    assert_eq!(caught.projected, 2, "exactly the two missed events fold");
    assert_eq!(caught.watermark, Some(4));
    assert!(caught.notices.is_empty());
    caught.notices.discard();
    assert_eq!(
        subscriber.projected,
        vec![0, 1, 2, 3, 4],
        "the reconnecting subscriber folded each durable fact exactly once"
    );

    // Convergence: the deaf subscriber's projection equals the oracle's.
    assert_pending_matches(
        &subscriber.fold,
        &expected.replayed_pending,
        "reconnected subscriber",
    );
    assert_eq!(subscriber.fold.admitted, 5);
    assert_eq!(subscriber.fold.promoted, 0);
    Ok(())
}

// ---------------------------------------------------------------------------
// 3. From-scratch replay into a fresh database
// ---------------------------------------------------------------------------

#[test]
fn a_from_scratch_replay_into_a_fresh_database_reproduces_the_log_and_the_fold() -> TestResult {
    let log = frozen_durable_log()?;
    let session = frozen_session(&log)?;
    let expected = expected_state()?;

    let (mut source, source_aggregate) = store_with_elected_writer(&session)?;
    for event in &log {
        let (_, notices) = append_frozen(&mut source, &source_aggregate, event)?;
        notices.discard();
    }
    let serialized = serialize_log(&source, &session)?;
    assert_eq!(
        serialized.len(),
        log.len(),
        "the source log serializes whole"
    );

    // The replay target is a SECOND, EMPTY database. Pointing this at `source`
    // would make the comparison below trivially true.
    let (mut fresh, fresh_aggregate) = store_with_elected_writer(&session)?;
    replay_log_into(&mut fresh, &fresh_aggregate, &serialized)?;

    // The event log itself, row by row.
    let replayed = serialize_log(&fresh, &session)?;
    assert_eq!(replayed.len(), serialized.len());
    for (original, copy) in serialized.iter().zip(replayed.iter()) {
        assert_eq!(copy.id, original.id, "replayed identity must match");
        assert_eq!(copy.seq, original.seq, "replayed sequence must match");
        assert_eq!(copy.kind_name, original.kind_name);
        assert_eq!(copy.version.value(), original.version.value());
        assert_eq!(copy.created_ms, original.created_ms);
        assert_eq!(copy.data, original.data, "replayed payload must match");
    }

    // And the projection the fresh database yields.
    let mut rebuilt = InboxSubscriberProjection::default();
    let caught = fresh.catch_up(&fresh_aggregate, &mut rebuilt)?;
    assert_eq!(caught.projected, 5);
    assert_eq!(caught.watermark, Some(4));
    assert!(caught.notices.is_empty());
    caught.notices.discard();
    assert_pending_matches(
        &rebuilt.fold,
        &expected.replayed_pending,
        "from-scratch replay",
    );
    Ok(())
}

// ---------------------------------------------------------------------------
// 4. An overflowed advisory buffer is lossy AND non-fatal
// ---------------------------------------------------------------------------

/// SYNTHETIC aggregate: fixture 12 has only five events and cannot reach the
/// advisory bound. The session id is deliberately not the fixture's.
#[test]
fn an_overflowed_advisory_buffer_reports_its_drops_and_durable_truth_still_converges() -> TestResult
{
    let session: SessionId = "ses_dl_synthetic_overflow".parse()?;
    let (mut store, aggregate) = store_with_elected_writer(&session)?;

    const OVERFLOW: usize = 6;
    let total = MAX_PENDING_NOTICES + OVERFLOW;

    // The fenced token has to be taken BEFORE the transaction borrows the store.
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    for index in 0..total {
        let payload = json!({
            "sessionID": session.as_str(),
            "inputID": format!("msg_dl_overflow_{index}"),
            "input": {"type": "user", "data": {"text": "synthetic"}, "delivery": "queue"},
        });
        let id: EventId = format!("evt_dl_overflow_{index}").parse()?;
        let version = EventVersion::new(1).ok_or("version 1 is non-zero")?;
        let appended = EventStore::append_within(
            &tx,
            AppendInput {
                id: &id,
                aggregate: aggregate.clone(),
                writer,
                kind_name: "session.input.admitted",
                version,
                created_ms: 1_786_174_494_371 + index as i64,
                data: &payload,
            },
            &mut [],
        )?;
        assert!(matches!(appended, Appended::Committed { .. }));
    }
    let committed = tx.commit()?;

    let mut live = LiveSubscriber::default();
    committed.publish(&mut live);

    assert_eq!(
        live.seen.len(),
        MAX_PENDING_NOTICES,
        "the advisory buffer stops at its declared bound"
    );
    assert_eq!(
        live.drop_reports,
        vec![OVERFLOW as u32],
        "the drop is reported exactly once, carrying the exact count"
    );
    assert_eq!(
        live.seen.first().copied(),
        Some(0),
        "the RETAINED end is the earliest facts, so the reported prefix stays contiguous"
    );

    // And durable truth is complete regardless of what the advisory path lost.
    let mut subscriber = InboxSubscriberProjection::default();
    let caught = store.catch_up(&aggregate, &mut subscriber)?;
    assert_eq!(
        caught.projected as usize, total,
        "every committed fact is durable, including the {OVERFLOW} whose notices were dropped"
    );
    assert_eq!(
        subscriber.fold.pending.len(),
        total,
        "the re-listing subscriber converges on the complete durable truth"
    );
    assert!(caught.notices.is_empty());
    caught.notices.discard();
    Ok(())
}

// ---------------------------------------------------------------------------
// 5. SYNTHETIC durable ToolTerminal: replay by decode, and refuse a rewrite
// ---------------------------------------------------------------------------

/// SYNTHETIC. Fixture 12 carries no `session.tool.terminal.1` row and neither
/// does any other scenario in the frozen corpus, so this witness supplies its
/// own fact on its own session id. It proves durable-log replay plus strict
/// decode — the ruled narrow reading — and adds no projection, table,
/// migration or event kind.
#[test]
fn a_synthetic_tool_terminal_replays_by_strict_decode_into_a_fresh_database() -> TestResult {
    let session: SessionId = "ses_dl_synthetic_tool".parse()?;
    let (mut store, aggregate) = store_with_elected_writer(&session)?;

    let attempt = PhysicalAttemptId::new("pat_dl_synthetic_1").ok_or("attempt id")?;
    let call = ToolCallId::new("call_dl_synthetic_1").ok_or("tool call id")?;
    let fact = ToolTerminalFact::new(
        attempt.clone(),
        call.clone(),
        ToolTerminalOutcome::ResultMissing,
    );
    let id: EventId = "evt_dl_synthetic_terminal".parse()?;
    let created_ms = 1_786_174_494_400_i64;

    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    let appended =
        grapt_store::tool_terminal::record(&tx, &session, &id, writer, created_ms, &fact, &mut [])?;
    assert!(matches!(appended, Appended::Committed { seq: 0 }));
    tx.commit()?.discard();

    // Replay the whole durable log into a fresh database, then decode the fact
    // back OUT of that database — not out of the value we still hold.
    let serialized = serialize_log(&store, &session)?;
    let (mut fresh, fresh_aggregate) = store_with_elected_writer(&session)?;
    replay_log_into(&mut fresh, &fresh_aggregate, &serialized)?;

    let replayed = serialize_log(&fresh, &session)?;
    let terminal = match replayed
        .iter()
        .find(|row| row.kind_name == "session.tool.terminal")
    {
        Some(row) => row,
        None => panic!("the replayed database must carry the synthetic tool terminal"),
    };
    let decoded = session_tool_terminal::decode(&terminal.data)?;
    assert_eq!(
        decoded, fact,
        "a from-scratch replay must decode back to the identical committed fact"
    );
    assert_eq!(decoded.attempt(), &attempt);
    assert_eq!(decoded.call(), &call);

    // A CHANGED payload at the same coordinate is a typed divergence, never a
    // silent overwrite.
    let changed = ToolTerminalFact::new(
        attempt,
        call,
        ToolTerminalOutcome::Completed { output: None },
    );
    let writer = store.fencing_token(&aggregate)?;
    let tx = store.governed_transaction()?;
    let rewrite = grapt_store::tool_terminal::record(
        &tx,
        &session,
        &id,
        writer,
        created_ms,
        &changed,
        &mut [],
    );
    assert!(
        matches!(rewrite, Err(WriteError::Divergence { .. })),
        "a changed payload at a committed coordinate must diverge, got {rewrite:?}"
    );
    drop(tx);
    Ok(())
}
