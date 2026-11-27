//! R3-a witnesses: the eight required
//! admission/promotion law proofs plus real-clone projector compatibility.

use grapt_inbox::{AdmitOutcome, AdmitRequest, Inbox, InboxError, PromoteScope, WakeDisposition};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    DeliveryClaimId, DeliveryMode, EventId, MessageId, SessionId, SubagentOperationId,
};
use rusqlite::{Connection, OpenFlags};
use std::path::{Path, PathBuf};
#[path = "../../../test-support/evidence.rs"]
mod evidence;

type TestResult = Result<(), Box<dyn std::error::Error>>;

#[test]
fn pending_inventory_corroborates_admission_and_does_not_resurrect_promoted_input() -> TestResult {
    let session: SessionId = "ses_pending_catalog".parse()?;
    let input: MessageId = "msg_pending_catalog".parse()?;
    let mut store = EventStore::open_in_memory()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    let mut inbox = Inbox::new(store)?;
    admit(
        &mut inbox,
        &"evt_pending_catalog".parse()?,
        &session,
        &input,
        "tabs\tand\nlines",
        DeliveryMode::Queue,
    )?;
    let pending = inbox.pending_inputs(&session)?;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].input, input);
    assert_eq!(pending[0].text, "tabs\tand\nlines");
    assert_eq!(pending[0].created_ms, 1786060800000);
    assert!(inbox.pending_inputs(&"ses_other".parse()?)?.is_empty());
    inbox.store().connection().execute(
        "UPDATE session_pending SET text = 'corrupted' WHERE id = ?1",
        [input.as_str()],
    )?;
    assert!(inbox.pending_inputs(&session).is_err());
    inbox.store().connection().execute(
        "UPDATE session_pending SET text = ?1 WHERE id = ?2",
        ["tabs\tand\nlines", input.as_str()],
    )?;
    inbox.promote_next(
        &session,
        PromoteScope::InputAtIdle,
        &mut minter("pending_promote"),
        1786060800001,
    )?;
    assert!(inbox.pending_inputs(&session)?.is_empty());
    assert_eq!(
        inbox.promoted_input(&session, &input)?.text,
        "tabs\tand\nlines"
    );
    inbox
        .store()
        .connection()
        .execute_batch("DROP TABLE session_pending")?;
    assert!(inbox.pending_inputs(&session).is_err());
    Ok(())
}

#[path = "inbox_admission/fixtures.rs"]
mod fixtures;
use fixtures::*;

#[path = "inbox_admission/delivery_lookup.rs"]
mod delivery_lookup;

#[test]
fn exact_retry_reconciles_before_and_after_promotion() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let e = ids("rt", 4)?;
    let input: MessageId = "msg_r3a_retry".parse()?;

    let first = admit(
        &mut inbox,
        &e[0],
        &session,
        &input,
        "hello",
        DeliveryMode::Steer,
    )?;
    assert!(matches!(
        first,
        AdmitOutcome::Admitted {
            seq: 0,
            wake: WakeDisposition::AdmitOnly
        }
    ));

    // Witness 1: exact retry BEFORE promotion (fresh event id) reconciles.
    let retry = admit(
        &mut inbox,
        &e[1],
        &session,
        &input,
        "hello",
        DeliveryMode::Steer,
    )?;
    assert!(matches!(retry, AdmitOutcome::Reconciled { .. }));

    // Promote, then Witness 2: exact retry AFTER promotion still reconciles
    // (resolved from the durable admitted event, the pending row is gone).
    let mut promo = minter("rtp");
    let promoted = inbox.promote(&session, PromoteScope::Steer, &mut promo, 1786060800001)?;
    assert_eq!(promoted.len(), 1);
    let post = admit(
        &mut inbox,
        &e[2],
        &session,
        &input,
        "hello",
        DeliveryMode::Steer,
    )?;
    assert!(matches!(post, AdmitOutcome::Reconciled { .. }));

    // Exactly ONE admitted event exists for this input throughout.
    let admitted_events: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.admitted.1'
         AND json_extract(data, '$.inputID') = ?1",
        [input.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(admitted_events, 1);
    Ok(())
}

#[test]
fn conflicting_reuse_is_a_typed_conflict() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let e = ids("cf", 4)?;
    let input: MessageId = "msg_r3a_conflict".parse()?;
    admit(
        &mut inbox,
        &e[0],
        &session,
        &input,
        "original",
        DeliveryMode::Steer,
    )?;

    // Different text.
    assert!(matches!(
        admit(
            &mut inbox,
            &e[1],
            &session,
            &input,
            "DIFFERENT",
            DeliveryMode::Steer
        ),
        Err(InboxError::Conflict {
            differs: "text",
            ..
        })
    ));
    // Different delivery.
    assert!(matches!(
        admit(
            &mut inbox,
            &e[2],
            &session,
            &input,
            "original",
            DeliveryMode::Queue
        ),
        Err(InboxError::Conflict {
            differs: "delivery",
            ..
        })
    ));
    // Different session. The other session is elected too, so the refusal
    // below can only be the reconciliation conflict and never an unelected
    // writer.
    let other: SessionId = "ses_r3a_other".parse()?;
    inbox
        .store_mut()
        .elect_writer(&AggregateId::Session(other.clone()))?;
    assert!(matches!(
        admit(
            &mut inbox,
            &e[3],
            &other,
            &input,
            "original",
            DeliveryMode::Steer
        ),
        Err(InboxError::Conflict {
            differs: "session",
            ..
        })
    ));
    Ok(())
}

#[test]
fn concurrent_same_id_admission_yields_no_duplicate() -> TestResult {
    let dir = scratch_dir("concurrent")?;
    let db = dir.join("inbox.db");
    // Both threads must write as ONE owner. Two independently opened stores
    // are two OWNERS of this session, so the second election transfers
    // ownership and leaves the first holding a stale epoch — and from there the
    // fence, not BEGIN IMMEDIATE, decides the outcome: the stale thread is
    // either refused outright as a stale writer, or arrives second and
    // reconciles off the winner's pending row without ever presenting its
    // token, which passes these assertions for the wrong reason. Sharing the
    // minted identity keeps both at the same retained first epoch, so both pass
    // the fence and BEGIN IMMEDIATE is the only thing left that can separate
    // them — which is the law this test exists to prove.
    let outcomes = concurrent_admissions(&db, "conc", "msg_r3a_conc")?;
    let mut admitted = 0;
    let mut reconciled = 0;
    for outcome in outcomes {
        match outcome {
            AdmitOutcome::Admitted { .. } => admitted += 1,
            AdmitOutcome::Reconciled { .. } => reconciled += 1,
        }
    }
    assert_eq!(
        (admitted, reconciled),
        (1, 1),
        "one durable admission, one reconciliation"
    );
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.admitted.1'",
        [],
        |r| r.get(0),
    )?;
    let pending: i64 = conn.query_row("SELECT COUNT(*) FROM session_pending", [], |r| r.get(0))?;
    assert_eq!(
        (events, pending),
        (1, 1),
        "no duplicate event or pending row"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn concurrent_operation_deliveries_share_one_atomic_last_slot() -> TestResult {
    let dir = scratch_dir("operation-delivery-capacity")?;
    let db = dir.join("inbox.db");
    let owner = {
        let store = EventStore::open(&db, "0.0.1")?;
        Inbox::ensure_tables(store.connection())?;
        ensure_delivery_authority_fixture(store.connection())?;
        for worker in 0..2 {
            store.connection().execute(
                "INSERT INTO subagent_operation
                 (operation_id, family_operation_id, parent_session_id, target_input,
                  claim_generation, run_generation, status, output,
                  artifact_id, artifact_length, artifact_digest, artifact_disposition,
                  claim_route, target_parent, delivery, claim_id)
                 VALUES (?1, ?2, 'ses_delivery_capacity', ?3, 7, 7, 'completed', 'x',
                         ?4, 1, ?5, 'complete', 'synthetic',
                         'ses_delivery_capacity', 'delivering', ?6)",
                rusqlite::params![
                    format!("sub_delivery_{worker}"),
                    format!("op_delivery_{worker}"),
                    format!("msg_delivery_{worker}"),
                    format!("art_delivery_{worker}"),
                    format!("digest_delivery_{worker}"),
                    format!("dcl_delivery_{worker}"),
                ],
            )?;
        }
        store.owner().clone()
    };
    let mut handles = Vec::new();
    for worker in 0..2 {
        let path = db.clone();
        let owner = owner.clone();
        handles.push(std::thread::spawn(move || -> Result<bool, String> {
            let mut store =
                EventStore::open_as(&path, "0.0.1", &owner).map_err(|error| error.to_string())?;
            let session: SessionId = "ses_delivery_capacity"
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?;
            store
                .elect_writer(&AggregateId::Session(session.clone()))
                .map_err(|error| error.to_string())?;
            Inbox::ensure_tables(store.connection()).map_err(|error| error.to_string())?;
            let held = store.held_writers();
            let tx = store
                .governed_transaction()
                .map_err(|error| error.to_string())?;
            let event_id: EventId = format!("evt_delivery_{worker}")
                .parse()
                .map_err(|error: grapt_types::IdError| error.to_string())?;
            let subagent = SubagentOperationId::new(format!("sub_delivery_{worker}"))
                .ok_or_else(|| "subagent id".to_owned())?;
            let claim = DeliveryClaimId::new(format!("dcl_delivery_{worker}"))
                .ok_or_else(|| "claim id".to_owned())?;
            let result = Inbox::admit_subagent_operation_delivery_within(
                &tx,
                &held,
                &event_id,
                (&subagent, &claim),
                (1, 1),
                1786060800000,
            );
            match result {
                Ok(_) => {
                    tx.commit().map_err(|error| error.to_string())?.discard();
                    Ok(true)
                }
                Err(InboxError::DeliveryCapacity { .. }) => Ok(false),
                Err(error) => Err(error.to_string()),
            }
        }));
    }
    let mut admitted = 0;
    let mut refused = 0;
    for handle in handles {
        match handle.join() {
            Ok(Ok(true)) => admitted += 1,
            Ok(Ok(false)) => refused += 1,
            Ok(Err(error)) => panic!("operation delivery failed: {error}"),
            Err(_) => panic!("operation delivery thread panicked"),
        }
    }
    assert_eq!((admitted, refused), (1, 1));
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let (events, pending): (i64, i64) = conn.query_row(
        "SELECT
             (SELECT COUNT(*) FROM event WHERE type = 'session.input.admitted.1'),
             (SELECT COUNT(*) FROM session_operation_delivery_pending)",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    assert_eq!((events, pending), (1, 1));
    Ok(())
}

#[test]
fn arbitrary_json_has_no_public_transactional_admission_escape() -> TestResult {
    let source = include_str!("../src/admission.rs");
    assert!(
        !source.contains("pub fn admit_within("),
        "arbitrary JSON admission would bypass the typed operation-delivery boundary"
    );
    assert!(!source.contains("pub struct OperationDeliveryRequest"));
    assert!(!source.contains("pub fn admit_operation_delivery_within("));
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_no_forge".parse()?;
    store.elect_writer(&AggregateId::Session(session.clone()))?;
    Inbox::ensure_tables(store.connection())?;
    ensure_delivery_authority_fixture(store.connection())?;
    let held = store.held_writers();
    let tx = store.governed_transaction()?;
    let event_id: EventId = "evt_no_forge".parse()?;
    let subagent = SubagentOperationId::new("sub_no_forge").ok_or("subagent id")?;
    let claim = DeliveryClaimId::new("dcl_no_forge").ok_or("claim id")?;
    assert!(matches!(
        Inbox::admit_subagent_operation_delivery_within(
            &tx,
            &held,
            &event_id,
            (&subagent, &claim),
            (1, 16),
            1,
        ),
        Err(InboxError::MissingOperationDeliveryAuthority { .. })
    ));
    let durable: i64 = tx.query_row(
        "SELECT (SELECT COUNT(*) FROM event) +
                (SELECT COUNT(*) FROM session_operation_delivery_pending)",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(durable, 0, "forged class leaves zero durable changes");
    Ok(())
}

#[test]
fn rollback_at_admission_and_promotion_cuts_is_atomic() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let e = ids("rb", 4)?;

    // Admission cut: force the projector INSERT to fail (drop the projection
    // table); the event append must roll back with it.
    inbox
        .store()
        .connection()
        .execute_batch("DROP TABLE session_pending")?;
    let input: MessageId = "msg_r3a_rollback".parse()?;
    assert!(admit(
        &mut inbox,
        &e[0],
        &session,
        &input,
        "x",
        DeliveryMode::Steer
    )
    .is_err());
    let events: i64 =
        inbox
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        events, 0,
        "admission projector failure rolls back the event"
    );

    // Restore, admit, then force the promotion projector to fail (message id
    // already occupied): promoted event + pending consumption roll back.
    inbox.store().connection().execute_batch(
        "CREATE TABLE session_pending (
            id TEXT PRIMARY KEY, session_id TEXT NOT NULL, delivery TEXT NOT NULL,
            text TEXT NOT NULL, admitted_seq INTEGER NOT NULL,
            input_type TEXT NOT NULL DEFAULT 'user',
            description TEXT, metadata_json TEXT)",
    )?;
    admit(
        &mut inbox,
        &e[1],
        &session,
        &input,
        "x",
        DeliveryMode::Steer,
    )?;
    inbox.store().connection().execute(
        "INSERT INTO session_message (id, session_id, type, text, delivery, seq)
         VALUES (?1, 'ses_r3a', 'user', 'occupied', 'steer', 99)",
        [input.as_str()],
    )?;
    let mut promo = minter("rbp");
    assert!(inbox
        .promote(&session, PromoteScope::Steer, &mut promo, 0)
        .is_err());
    let pending: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM session_pending WHERE id = ?1",
        [input.as_str()],
        |r| r.get(0),
    )?;
    let promoted_events: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.promoted.1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        (pending, promoted_events),
        (1, 0),
        "promotion cut rolled back atomically"
    );
    Ok(())
}

#[test]
fn steer_before_queue_one_queue_at_idle_and_pending_emptiness() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let e = ids("ord", 5)?;
    let steer_a: MessageId = "msg_r3a_sa".parse()?;
    let queue_a: MessageId = "msg_r3a_qa".parse()?;
    let queue_b: MessageId = "msg_r3a_qb".parse()?;
    let steer_b: MessageId = "msg_r3a_sb".parse()?;
    admit(
        &mut inbox,
        &e[0],
        &session,
        &steer_a,
        "s1",
        DeliveryMode::Steer,
    )?;
    admit(
        &mut inbox,
        &e[1],
        &session,
        &queue_a,
        "q1",
        DeliveryMode::Queue,
    )?;
    admit(
        &mut inbox,
        &e[2],
        &session,
        &queue_b,
        "q2",
        DeliveryMode::Queue,
    )?;
    admit(
        &mut inbox,
        &e[3],
        &session,
        &steer_b,
        "s2",
        DeliveryMode::Steer,
    )?;

    // Steer scope: both steers, admission order, queues untouched.
    let mut promo = minter("ordp");
    let steers = inbox.promote(&session, PromoteScope::Steer, &mut promo, 0)?;
    assert_eq!(
        steers.iter().map(|p| p.input.as_str()).collect::<Vec<_>>(),
        vec!["msg_r3a_sa", "msg_r3a_sb"],
        "steers promote before queues, in admission order"
    );

    // Idle boundary: exactly ONE queued input promotes.
    let one = inbox.promote(&session, PromoteScope::InputAtIdle, &mut promo, 0)?;
    assert_eq!(
        one.iter().map(|p| p.input.as_str()).collect::<Vec<_>>(),
        vec!["msg_r3a_qa"],
        "one queued input at the idle boundary"
    );

    // The second queue is still pending; a second idle boundary takes it,
    // after which pending is EMPTY.
    let two = inbox.promote(&session, PromoteScope::InputAtIdle, &mut promo, 0)?;
    assert_eq!(two.len(), 1);
    let remaining: i64 =
        inbox
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM session_pending", [], |r| r.get(0))?;
    assert_eq!(remaining, 0, "pending contains only unconsumed work");
    let messages: i64 =
        inbox
            .store()
            .connection()
            .query_row("SELECT COUNT(*) FROM session_message", [], |r| r.get(0))?;
    assert_eq!(
        messages, 4,
        "every promotion created exactly one visible message"
    );
    Ok(())
}

#[test]
fn promotion_rejects_unknown_pending_delivery_instead_of_stranding_it() -> TestResult {
    let (mut inbox, session) = fresh()?;
    let event = ids("closed", 1)?;
    let input: MessageId = "msg_r3a_closed".parse()?;
    admit(
        &mut inbox,
        &event[0],
        &session,
        &input,
        "must not disappear",
        DeliveryMode::Steer,
    )?;
    inbox.store().connection().execute(
        "UPDATE session_pending SET delivery = 'later' WHERE id = ?1",
        [input.as_str()],
    )?;

    let mut mint = minter("closedp");
    assert!(matches!(
        inbox.promote(&session, PromoteScope::InputAtIdle, &mut mint, 0),
        Err(InboxError::Corruption { .. })
    ));
    let pending: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM session_pending WHERE id = ?1",
        [input.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(
        pending, 1,
        "corrupt durable work remains visible for repair"
    );
    let promotions: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.promoted.1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(promotions, 0, "refusal writes no promotion event");
    Ok(())
}

#[test]
#[ignore = "evidence-bearing: needs the frozen clone; run with --include-ignored on the evidence machine"]
fn real_clone_events_replay_through_the_write_projector() -> TestResult {
    // Clone-compat witness: every admitted/promoted event in the REAL clone
    // replays through MY projector into a fresh projection, and the final
    // pending set matches the clone's own session_pending table exactly.
    let path = evidence::frozen_clone();

    let log = grapt_store::EventLog::open_read_only(&path)?;
    let store = EventStore::open_in_memory()?;
    let inbox = Inbox::new(store)?;
    let conn = inbox.store().connection();

    // Order matters; apply the projector's SQL law directly per event.
    let mut applied = 0u64;
    let mut failures: Vec<String> = Vec::new();
    log.for_each_in_replay_order(|e| {
        let decoded =
            grapt_inbox::InboxEvent::decode(e.kind.name(), e.kind.version().value(), &e.data);
        match decoded {
            Ok(Some(grapt_inbox::InboxEvent::Admitted {
                session,
                input,
                delivery,
            })) => {
                let text = e
                    .data
                    .pointer("/input/data/text")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_default();
                let delivery = match delivery {
                    DeliveryMode::Steer => "steer",
                    DeliveryMode::Queue => "queue",
                };
                if let Err(err) = conn.execute(
                    "INSERT INTO session_pending (id, session_id, delivery, text, admitted_seq)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    rusqlite::params![
                        input.as_str(),
                        session.as_str(),
                        delivery,
                        text,
                        e.seq.value() as i64
                    ],
                ) {
                    failures.push(format!("{}: {err}", input));
                }
                applied += 1;
            }
            Ok(Some(grapt_inbox::InboxEvent::Promoted { input, .. })) => {
                if let Err(err) = conn.execute(
                    "DELETE FROM session_pending WHERE id = ?1",
                    [input.as_str()],
                ) {
                    failures.push(format!("{}: {err}", input));
                }
                applied += 1;
            }
            // The frozen clone predates manual compaction, so it carries no
            // compaction event to mirror; a compaction row it left pending
            // would still fail the parity assertion below.
            Ok(Some(grapt_inbox::InboxEvent::CompactionAdmitted { .. }))
            | Ok(Some(grapt_inbox::InboxEvent::CompactionSettled { .. }))
            | Ok(Some(grapt_inbox::InboxEvent::OperationDeliveryAdmitted { .. }))
            | Ok(Some(grapt_inbox::InboxEvent::AgentMessageAdmitted(_)))
            | Ok(Some(grapt_inbox::InboxEvent::SyntheticAdmitted { .. }))
            | Ok(Some(grapt_inbox::InboxEvent::LegacySyntheticAdmitted { .. })) => {}
            Ok(None) => {}
            Err(err) => failures.push(format!("{}: {err}", e.id.as_str())),
        }
    })?;
    assert!(
        failures.is_empty(),
        "projector replay failures: {failures:?}"
    );
    assert_eq!(
        applied,
        887 + 885,
        "all real admissions and promotions applied"
    );

    // Parity with the clone's own session_pending.
    let clone_conn = Connection::open_with_flags(&path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let mut clone_rows: Vec<(String, i64)> = clone_conn
        .prepare("SELECT id, admitted_seq FROM session_pending ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    let mut mine: Vec<(String, i64)> = conn
        .prepare("SELECT id, admitted_seq FROM session_pending ORDER BY id")?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<Result<_, _>>()?;
    clone_rows.sort();
    mine.sort();
    assert_eq!(
        mine, clone_rows,
        "final pending set matches the real clone exactly"
    );
    Ok(())
}

#[test]
fn contending_admitters_serialize_inside_the_transaction() -> TestResult {
    // Review A witness: bounded start signals place both callers in the same window;
    // BEGIN IMMEDIATE serializes them and the loser deterministically
    // reconciles inside its own transaction — never a projector PK error.
    let dir = scratch_dir("barrier")?;
    let db = dir.join("inbox.db");
    // As above, both threads write as ONE owner, and here forced contention makes the
    // alternative worse than wrong: with two owners the loser of the election
    // enters the forced window already fenced, so the window would decide
    // between a stale-writer refusal and a reconciliation the fence caused —
    // never the serialization this test names. One owner keeps the fence out of
    // the window entirely.
    let mut outcomes = concurrent_admissions(&db, "bar", "msg_r3a_barrier")?;
    outcomes.sort_by_key(|o| matches!(o, AdmitOutcome::Reconciled { .. }));
    assert!(
        matches!(outcomes[0], AdmitOutcome::Admitted { .. })
            && matches!(outcomes[1], AdmitOutcome::Reconciled { .. }),
        "exactly one Admitted and one Reconciled under forced contention: {outcomes:?}"
    );
    let conn = Connection::open_with_flags(&db, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let events: i64 = conn.query_row(
        "SELECT COUNT(*) FROM event WHERE type = 'session.input.admitted.1'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(events, 1);
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn post_promotion_corruption_is_typed_never_reconciled_or_readmitted() -> TestResult {
    use grapt_inbox::InboxError;

    // Case 1: projected message DELETED — retry must be Corruption.
    let (mut inbox, session) = fresh()?;
    let e = ids("cor", 6)?;
    let input: MessageId = "msg_r3a_corrupt".parse()?;
    admit(
        &mut inbox,
        &e[0],
        &session,
        &input,
        "hello",
        DeliveryMode::Steer,
    )?;
    let mut promo = minter("corp");
    inbox.promote(&session, PromoteScope::Steer, &mut promo, 0)?;
    inbox.store().connection().execute(
        "DELETE FROM session_message WHERE id = ?1",
        [input.as_str()],
    )?;
    assert!(matches!(
        admit(
            &mut inbox,
            &e[1],
            &session,
            &input,
            "hello",
            DeliveryMode::Steer
        ),
        Err(InboxError::Corruption { .. })
    ));

    // Case 2: admitted event JSON corrupted — Corruption, not fresh admission.
    //
    // `event` is a GOVERNED table, so this corruption cannot be staged through
    // the inbox's own store: its connection carries the write chokepoint's
    // denial and refuses the UPDATE at statement preparation. That refusal is
    // the point of F3B1b, so the fixture stages the damage the only way a
    // hostile state can now arrive — from OUTSIDE the store's connection, which
    // is exactly what a foreign process would be. See `governed_writes.rs` for
    // the witness that the store's own connection refuses this same statement.
    let dir = scratch_dir("corrupt-event")?;
    let db = dir.join("inbox.db");
    let (mut inbox2, session2) = fresh_on(&db)?;
    let input2: MessageId = "msg_r3a_corrupt2".parse()?;
    admit(
        &mut inbox2,
        &e[2],
        &session2,
        &input2,
        "hello",
        DeliveryMode::Steer,
    )?;
    let mut promo2 = minter("corq");
    inbox2.promote(&session2, PromoteScope::Steer, &mut promo2, 0)?;
    Connection::open(&db)?.execute(
        "UPDATE event SET data = '{broken' WHERE type = 'session.input.admitted.1'",
        [],
    )?;
    assert!(matches!(
        admit(
            &mut inbox2,
            &e[3],
            &session2,
            &input2,
            "hello",
            DeliveryMode::Steer
        ),
        Err(InboxError::Corruption { .. })
    ));

    drop(inbox2);
    let _ = std::fs::remove_dir_all(&dir);

    // Case 3: projected message text MISMATCHES the admitted event.
    let (mut inbox3, session3) = fresh()?;
    let input3: MessageId = "msg_r3a_corrupt3".parse()?;
    admit(
        &mut inbox3,
        &e[4],
        &session3,
        &input3,
        "hello",
        DeliveryMode::Steer,
    )?;
    let mut promo3 = minter("corr");
    inbox3.promote(&session3, PromoteScope::Steer, &mut promo3, 0)?;
    inbox3.store().connection().execute(
        "UPDATE session_message SET text = 'tampered' WHERE id = ?1",
        [input3.as_str()],
    )?;
    assert!(matches!(
        admit(
            &mut inbox3,
            &e[5],
            &session3,
            &input3,
            "hello",
            DeliveryMode::Steer
        ),
        Err(InboxError::Corruption { .. })
    ));
    Ok(())
}

#[test]
fn promotion_never_moves_an_input_across_sessions() -> TestResult {
    // Review C witness: the projector consumes a pending row only when the
    // promoted event's session matches; a cross-session promotion attempt
    // fails and rolls back, leaving the pending row where it belongs.
    let (mut inbox, session) = fresh()?;
    let e = ids("xs", 1)?;
    let input: MessageId = "msg_r3a_cross".parse()?;
    admit(
        &mut inbox,
        &e[0],
        &session,
        &input,
        "mine",
        DeliveryMode::Steer,
    )?;

    // A promotion under a DIFFERENT session finds nothing eligible — the
    // plan is session-scoped — so nothing promotes and pending is intact.
    // The other session is elected as well, so an empty promotion is the
    // session scoping and not a writer this store was never elected for.
    let other: SessionId = "ses_r3a_other".parse()?;
    inbox
        .store_mut()
        .elect_writer(&AggregateId::Session(other.clone()))?;
    let mut promo = minter("xsp");
    let promoted = inbox.promote(&other, PromoteScope::InputAtIdle, &mut promo, 0)?;
    assert!(promoted.is_empty());
    let pending: i64 = inbox.store().connection().query_row(
        "SELECT COUNT(*) FROM session_pending WHERE id = ?1 AND session_id = ?2",
        rusqlite::params![input.as_str(), session.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(pending, 1, "the input stays in its own session");
    Ok(())
}
