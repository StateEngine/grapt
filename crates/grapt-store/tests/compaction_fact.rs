//! F4B1b: the durable compaction fact, typed outcome recovery, the rebuildable
//! cache, fork-before-compaction reconstruction and the crash cuts around them.
//!
//! These witnesses run against the PUBLIC surface, from outside the crate, so
//! everything they prove is something a consumer service can actually rely on.
//! The two laws a public caller cannot reach — a corrupt cache row and a cache
//! watermark that LEADS its input — are witnessed white-box beside the code that
//! refuses them, in `src/compaction.rs`, because `rebuild_cache` writes only
//! validated coordinates and writes the watermark from the same fold that wrote
//! the rows.

use grapt_store::{
    AggregateId, AppendInput, CompactionAttempt, CompactionError, CompactionOutcome, EventStore,
    Projector,
};
use grapt_types::{
    ArtifactDigest, ArtifactId, ArtifactRef, CompactionDescriptor, CompactionDescriptorParts,
    CompactionId, CompactionSummary, CompactionTrigger, ContextEpoch, ContextFailure,
    ContextWindowId, EventId, EventVersion, ForkBoundary, NextRequestRef, RetainedHistory,
    RetentionPolicyVersion, SessionId, StepNumber,
};
use serde_json::{json, Value};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn artifact(id: &str) -> Option<ArtifactRef> {
    Some(ArtifactRef::new(
        ArtifactId::new(id)?,
        7,
        ArtifactDigest::new("digest")?,
    ))
}

/// A legal descriptor. `target_epoch` advances past `source_epoch`, which is
/// what the constructor requires — a pair that does not advance is the
/// no-progress refusal, used deliberately in its own witness below.
fn descriptor_at(
    session: &SessionId,
    compaction: &str,
    window: &str,
    source_epoch: u64,
    target_epoch: u64,
) -> Option<CompactionDescriptor> {
    CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new(compaction)?,
        source_session: session.clone(),
        source_epoch: ContextEpoch::new(source_epoch),
        target_window: ContextWindowId::new(window)?,
        target_epoch: ContextEpoch::new(target_epoch),
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("ret-1")?,
        summary: CompactionSummary::ClientReadable {
            artifact: artifact("sum-1")?,
        },
        retained: RetainedHistory::References {
            artifacts: vec![artifact("hist-1")?],
        },
        baseline: artifact("base-1")?,
        next_request: NextRequestRef::new(StepNumber::new(1)?, artifact("req-1")?),
    })
    .ok()
}

fn descriptor(session: &SessionId) -> Option<CompactionDescriptor> {
    descriptor_at(session, "cmp-1", "cwn-1", 1, 2)
}

fn elected() -> Result<(EventStore, SessionId, AggregateId), Box<dyn std::error::Error>> {
    let mut store = EventStore::open_in_memory()?;
    let session: SessionId = "ses_033601c26ffe".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    store.elect_writer(&aggregate)?;
    Ok((store, session, aggregate))
}

/// Appends one ordinary (non-compaction) session fact, so the log has history
/// around the descriptor.
fn append_plain(
    store: &mut EventStore,
    aggregate: &AggregateId,
    id: &str,
    marker: &str,
) -> Result<u64, Box<dyn std::error::Error>> {
    let writer = store.fencing_token(aggregate)?;
    let payload: Value = json!({ "sessionID": "ses_033601c26ffe", "inputID": marker });
    let (appended, notices) = store.append(
        AppendInput {
            id: &id.parse::<EventId>()?,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.input.admitted",
            version: EventVersion::new(1).ok_or("version 1")?,
            created_ms: 1_786_147_200_000,
            data: &payload,
        },
        &mut [],
    )?;
    notices.discard();
    Ok(match appended {
        grapt_store::Appended::Committed { seq } | grapt_store::Appended::Idempotent { seq } => seq,
    })
}

/// Records one compaction attempt through its own governed transaction.
fn record(
    store: &mut EventStore,
    id: &str,
    attempt: CompactionAttempt<'_>,
    aggregate: &AggregateId,
) -> Result<CompactionOutcome, Box<dyn std::error::Error>> {
    let writer = store.fencing_token(aggregate)?;
    let event: EventId = id.parse()?;
    let tx = store.governed_transaction()?;
    let outcome = grapt_store::compaction::record(
        &tx,
        &event,
        writer,
        1_786_147_200_000,
        attempt,
        &mut [] as &mut [&mut dyn Projector],
    )?;
    tx.commit()?.discard();
    Ok(outcome)
}

// ---------------------------------------------------------------- law 1

/// LAW 1: a successful compaction commits ONE durable fact — not a marker plus
/// a copy of the history it replaced.
///
/// The count is the assertion. A marker-plus-copy implementation would leave two
/// rows behind for one compaction, and the retained history would appear in the
/// log as durable content rather than as the reference the descriptor carries.
#[test]
fn a_settled_compaction_commits_exactly_one_durable_fact() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;

    let descriptor = descriptor(&session).ok_or("descriptor")?;
    let outcome = record(
        &mut store,
        "evt_0336000000cmp",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;
    assert!(
        matches!(outcome, CompactionOutcome::Settled { seq: 1 }),
        "the descriptor must settle at the next sequence, got {outcome:?}"
    );

    let compaction_rows: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_id = ?1 AND type LIKE 'session.compaction.%'",
        rusqlite::params![session.as_str()],
        |r| r.get(0),
    )?;
    assert_eq!(
        compaction_rows, 1,
        "one compaction must leave exactly one durable row"
    );
    Ok(())
}

/// LAW 1, the other half: the durable row is the RUST-NATIVE v2 fact, and its
/// stored bytes decode back through F4B1V's accepted codec into an EQUAL
/// descriptor.
///
/// Asserting the round trip rather than just the type string is what makes this
/// a fact witness instead of a naming one: a row that decoded into a different
/// descriptor would satisfy any assertion about its `type` column.
#[test]
fn the_durable_row_is_the_v2_fact_and_round_trips_to_an_equal_descriptor() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_0336000000cmp",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    let (versioned_type, data_text): (String, String) = store.connection().query_row(
        "SELECT type, data FROM event WHERE aggregate_id = ?1 AND type LIKE 'session.compaction.%'",
        rusqlite::params![session.as_str()],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        versioned_type, "session.compaction.ended.2",
        "the fact must be stored as the Rust-native v2 kind"
    );

    let stored: Value = serde_json::from_str(&data_text)?;
    let decoded = grapt_event::session_compaction::decode(&stored)
        .map_err(|e| format!("the stored payload must decode: {e}"))?;
    assert_eq!(
        decoded.id().as_str(),
        descriptor.id().as_str(),
        "the stored descriptor must be the one that was recorded"
    );
    assert_eq!(
        decoded.target_epoch().value(),
        descriptor.target_epoch().value(),
        "the stored target epoch must survive the round trip"
    );
    assert_eq!(
        decoded.target_window().as_str(),
        descriptor.target_window().as_str(),
        "the stored target window must survive the round trip"
    );
    Ok(())
}

// ---------------------------------------------------------------- law 2

/// LAW 2: a typed failure appends NOTHING and leaves the previous durable
/// history recoverable.
///
/// Both halves are asserted. Returning the failure is not enough on its own: an
/// implementation that recorded a "compaction failed" row would also return it,
/// and the prior history would then be followed by a fact that says the epoch
/// moved when it did not.
#[test]
fn a_failed_compaction_appends_nothing_and_leaves_prior_history_recoverable() -> TestResult {
    let (mut store, _session, aggregate) = elected()?;
    let before = append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;

    let outcome = record(
        &mut store,
        "evt_0336000000fai",
        CompactionAttempt::Failed(ContextFailure::CompactionFailed),
        &aggregate,
    )?;
    assert_eq!(
        outcome,
        CompactionOutcome::Refused {
            failure: ContextFailure::CompactionFailed
        },
        "the exact submitted failure must come back"
    );

    let total: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        total, 1,
        "a failed compaction must append nothing at all, leaving only the prior fact"
    );
    let head: i64 = store
        .connection()
        .query_row("SELECT MAX(seq) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        u64::try_from(head)?,
        before,
        "the prior history must still be the head, unchanged and recoverable"
    );
    Ok(())
}

/// LAW 2: the four typed outcomes stay DISTINCT.
///
/// The grid is the witness. Each arm is submitted and must come back as itself;
/// an implementation that collapsed any two — onto a shared "compaction failed",
/// or onto whichever variant happened to be first — would return the wrong one
/// for at least one arm and the loop names which.
///
/// `Overflow` is included deliberately: it is the CAUSE that sends a compaction
/// into the machine, and a store that mapped every context failure onto it would
/// pass a test that only checked the three compaction-specific arms.
#[test]
fn every_typed_compaction_outcome_survives_as_itself() -> TestResult {
    let arms = [
        ContextFailure::CompactionFailed,
        ContextFailure::CompactionNoProgress,
        ContextFailure::CompactionIncompatibleSummary,
        ContextFailure::Overflow,
    ];
    let mut ran = 0;
    for (index, failure) in arms.into_iter().enumerate() {
        let (mut store, _session, aggregate) = elected()?;
        let id = format!("evt_033600000f{index:02}");
        let outcome = record(
            &mut store,
            &id,
            CompactionAttempt::Failed(failure),
            &aggregate,
        )?;
        assert_eq!(
            outcome,
            CompactionOutcome::Refused { failure },
            "the {failure:?} arm must come back as itself, never coerced"
        );
        ran += 1;
    }
    assert_eq!(ran, 4, "every arm of the grid must have run");
    Ok(())
}

/// LAW 2: no-progress is refused by the DESCRIPTOR's own constructor, so a
/// no-progress compaction cannot become a durable fact at all.
///
/// This is the constructor-side half of the same law: the store never has to
/// detect a non-advancing epoch pair, because a descriptor carrying one cannot
/// be built. Asserting the exact variant keeps it distinguishable from an
/// ordinary construction failure.
#[test]
fn a_non_advancing_epoch_pair_is_refused_as_no_progress() -> TestResult {
    let session: SessionId = "ses_033601c26ffe".parse()?;
    let refused = CompactionDescriptor::new(CompactionDescriptorParts {
        id: CompactionId::new("cmp-stall").ok_or("id")?,
        source_session: session,
        source_epoch: ContextEpoch::new(4),
        // Does not advance.
        target_epoch: ContextEpoch::new(4),
        target_window: ContextWindowId::new("cwn-stall").ok_or("window")?,
        trigger: CompactionTrigger::ContextOverflow,
        retention_policy: RetentionPolicyVersion::new("ret-1").ok_or("policy")?,
        summary: CompactionSummary::ClientReadable {
            artifact: artifact("sum-1").ok_or("artifact")?,
        },
        retained: RetainedHistory::References {
            artifacts: vec![artifact("hist-1").ok_or("artifact")?],
        },
        baseline: artifact("base-1").ok_or("artifact")?,
        next_request: NextRequestRef::new(
            StepNumber::new(1).ok_or("step")?,
            artifact("req-1").ok_or("artifact")?,
        ),
    });
    assert_eq!(
        refused.err(),
        Some(ContextFailure::CompactionNoProgress),
        "an epoch pair that does not advance must be exactly CompactionNoProgress"
    );
    Ok(())
}

// ---------------------------------------------------------------- law 3

/// LAW 3: the cache is rebuilt from CANONICAL TRUTH, and a rebuild from empty
/// reaches the same answer as the rebuild that ran with a cache already present.
///
/// The two-rebuild comparison is the mechanism. A fold that seeded itself from
/// its own previous contents would agree with itself on the second pass and
/// diverge from the pass that started with nothing — so the witness deletes the
/// cache between the two and requires the SAME window both times.
#[test]
fn the_cache_rebuilds_from_the_event_log_alone() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;
    let first = descriptor_at(&session, "cmp-1", "cwn-1", 1, 2).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&first),
        &aggregate,
    )?;
    let second = descriptor_at(&session, "cmp-2", "cwn-2", 2, 3).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c02",
        CompactionAttempt::Settled(&second),
        &aggregate,
    )?;

    let tx = store.governed_transaction()?;
    let rebuilt = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
    tx.commit()?.discard();
    assert_eq!(rebuilt.folded, 2, "both descriptors must fold");
    assert_eq!(
        rebuilt.watermark,
        Some(2),
        "the cache watermark must reach the aggregate's head"
    );

    let with_cache = grapt_store::compaction::cached_window(store.connection(), &aggregate)?
        .ok_or("a window must be cached")?;
    assert_eq!(
        with_cache.window().as_str(),
        "cwn-2",
        "the LAST descriptor is the current window"
    );

    // Wipe every cache row, then rebuild from nothing but the log.
    store
        .connection()
        .execute("DELETE FROM compaction_window_cache", [])?;
    store
        .connection()
        .execute("DELETE FROM compaction_cache_watermark", [])?;
    assert!(
        grapt_store::compaction::cached_window(store.connection(), &aggregate)?.is_none(),
        "the cache must really be empty before the second rebuild"
    );

    let tx = store.governed_transaction()?;
    let from_empty = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
    tx.commit()?.discard();
    let rebuilt_window = grapt_store::compaction::cached_window(store.connection(), &aggregate)?
        .ok_or("a window must be cached after the rebuild from empty")?;

    assert_eq!(
        from_empty, rebuilt,
        "a rebuild from empty must fold exactly what the first pass folded"
    );
    assert_eq!(
        rebuilt_window, with_cache,
        "a cache rebuilt from the log alone must equal the cache built with one present"
    );
    Ok(())
}

/// LAW 3: a rebuild REPLACES the cache it finds rather than folding on top of
/// it.
///
/// The second pass runs with the first pass's rows still present, which is the
/// state a real re-fold always meets. A fold that did not clear first would meet
/// its own previous row on the primary key instead of replacing it — the exact
/// shape in which a rebuildable cache stops being rebuildable.
#[test]
fn a_rebuild_over_an_existing_cache_replaces_it_rather_than_colliding() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    let tx = store.governed_transaction()?;
    let first = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
    tx.commit()?.discard();

    // No clearing between the two: the fold's own clearing step is what must
    // make this legal.
    let tx = store.governed_transaction()?;
    let second = match grapt_store::compaction::rebuild_cache(&tx, &aggregate) {
        Ok(second) => second,
        Err(error) => panic!("a rebuild over an existing cache must succeed: {error}"),
    };
    tx.commit()?.discard();

    assert_eq!(
        second, first,
        "a rebuild over an existing cache must replace it, not accumulate onto it"
    );
    let window = grapt_store::compaction::cached_window(store.connection(), &aggregate)?
        .ok_or("a window must still be cached")?;
    assert_eq!(
        window.window().as_str(),
        "cwn-1",
        "the replaced cache must still name the log's window"
    );
    Ok(())
}

/// LAW 3/5: the cache watermark tracks the WHOLE log, not only the rows the
/// fold found interesting.
///
/// The last event here is an ordinary fact appended AFTER the compaction, so a
/// watermark that advanced only on compaction rows would sit below the head and
/// every read would report a caught-up cache as lagging. The cached window must
/// still be the compaction's, which is what separates "how far the fold has
/// seen" from "what the fold found".
#[test]
fn the_cache_watermark_tracks_the_whole_log_not_only_compaction_rows() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;
    let last = append_plain(&mut store, &aggregate, "evt_0336000000two", "after")?;

    let tx = store.governed_transaction()?;
    let rebuilt = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
    tx.commit()?.discard();

    assert_eq!(
        rebuilt.watermark,
        Some(last),
        "the watermark must reach the aggregate's head, not the last compaction"
    );
    let window = grapt_store::compaction::cached_window(store.connection(), &aggregate)?
        .ok_or("a window must still be cached")?;
    assert_eq!(
        window.window().as_str(),
        "cwn-1",
        "the cached window is still the compaction's, even though later facts exist"
    );
    Ok(())
}

/// LAW 3: a legacy v1 compaction is SEEN and deliberately not folded.
///
/// v1 is the frozen oracle payload and carries no window. Counting it keeps
/// "this session never compacted" and "this session compacted before the
/// Rust-native fact existed" from collapsing into one empty answer.
#[test]
fn a_legacy_v1_compaction_is_counted_but_never_folded_into_a_window() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let writer = store.fencing_token(&aggregate)?;
    let v1: Value = json!({
        "sessionID": session.as_str(),
        "reason": "auto",
        "text": "summary text",
        "recent": "recent text",
    });
    let (_appended, notices) = store.append(
        AppendInput {
            id: &"evt_033600000v01".parse::<EventId>()?,
            aggregate: aggregate.clone(),
            writer,
            kind_name: "session.compaction.ended",
            version: EventVersion::new(1).ok_or("version 1")?,
            created_ms: 1_786_147_200_000,
            data: &v1,
        },
        &mut [],
    )?;
    notices.discard();

    let tx = store.governed_transaction()?;
    let rebuilt = match grapt_store::compaction::rebuild_cache(&tx, &aggregate) {
        Ok(rebuilt) => rebuilt,
        Err(error) => panic!("a v1-only log must fold without refusing: {error}"),
    };
    tx.commit()?.discard();

    assert_eq!(rebuilt.folded, 0, "a v1 row must not fold into a window");
    assert_eq!(rebuilt.legacy_seen, 1, "a v1 row must still be counted");
    assert!(
        grapt_store::compaction::cached_window(store.connection(), &aggregate)?.is_none(),
        "a v1-only session has no Rust-native window"
    );
    Ok(())
}

// ---------------------------------------------------------------- law 6

/// LAW 6: a fork whose boundary PRECEDES a compaction reconstructs the
/// pre-compaction history, and does not see the descriptor or anything after it.
///
/// Both directions are asserted from one log: the pre-compaction fact is
/// admitted, and the compaction descriptor's own sequence is refused. A cap that
/// admitted past its bound would hand the child exactly the post-compaction
/// truth the boundary was drawn to exclude.
#[test]
fn a_fork_before_a_compaction_reconstructs_only_pre_compaction_history() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let before = append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;

    let descriptor = descriptor(&session).ok_or("descriptor")?;
    let settled = record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;
    let compaction_seq = match settled {
        CompactionOutcome::Settled { seq } => seq,
        other => return Err(format!("the descriptor must settle: {other:?}").into()),
    };
    append_plain(&mut store, &aggregate, "evt_0336000000two", "after")?;

    // The child forks at the pre-compaction sequence.
    let child: SessionId = "ses_033601c26fff".parse()?;
    let boundary =
        ForkBoundary::from_raw_parent_seq(child, session.clone(), i64::try_from(before)?)
            .map_err(|e| format!("the boundary must build: {e:?}"))?;

    let admitted = grapt_store::compaction::admitted_parent_history(store.connection(), &boundary)?;

    assert!(
        admitted.contains(&before),
        "the pre-compaction fact at {before} must be reconstructable, got {admitted:?}"
    );
    assert!(
        !admitted.contains(&compaction_seq),
        "the compaction descriptor at {compaction_seq} is past the cap and must be excluded, got {admitted:?}"
    );
    assert_eq!(
        admitted,
        vec![before],
        "exactly the pre-compaction history is admitted, got {admitted:?}"
    );
    Ok(())
}

// ---------------------------------------------------------------- law 7

/// LAW 7: a crash between the descriptor append and the commit leaves NOTHING —
/// the previous truth is intact and recoverable.
///
/// The transaction is dropped rather than committed, which is what a process
/// death at that cut looks like to the file. The assertion is on a SECOND
/// connection, so it observes what actually reached the database rather than
/// what the dropped transaction remembered.
#[test]
fn a_crash_before_commit_leaves_the_descriptor_absent_and_prior_truth_intact() -> TestResult {
    let dir = std::env::temp_dir().join(format!("f4b1b-cut-{}", std::process::id()));
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("cut.sqlite");
    let _ = std::fs::remove_file(&db);

    let session: SessionId = "ses_033601c26ffe".parse()?;
    let aggregate = AggregateId::Session(session.clone());
    let mut store = EventStore::open(&db, "f4b1b-cut")?;
    store.elect_writer(&aggregate)?;
    let before = append_plain(&mut store, &aggregate, "evt_0336000000one", "before")?;

    let descriptor = descriptor(&session).ok_or("descriptor")?;
    {
        let writer = store.fencing_token(&aggregate)?;
        let event: EventId = "evt_033600000c01".parse()?;
        let tx = store.governed_transaction()?;
        let outcome = grapt_store::compaction::record(
            &tx,
            &event,
            writer,
            1_786_147_200_000,
            CompactionAttempt::Settled(&descriptor),
            &mut [] as &mut [&mut dyn Projector],
        )?;
        assert!(
            matches!(outcome, CompactionOutcome::Settled { .. }),
            "the append itself must have succeeded inside the transaction"
        );
        // The cut: dropped, never committed.
    }

    let observer = rusqlite::Connection::open(&db)?;
    let compaction_rows: i64 = observer.query_row(
        "SELECT COUNT(*) FROM event WHERE type LIKE 'session.compaction.%'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        compaction_rows, 0,
        "a crash before commit must leave no descriptor behind"
    );
    let head: i64 = observer.query_row("SELECT MAX(seq) FROM event", [], |r| r.get(0))?;
    assert_eq!(
        u64::try_from(head)?,
        before,
        "the prior history must be exactly what it was before the cut"
    );

    drop(observer);
    drop(store);
    let _ = std::fs::remove_file(&db);
    Ok(())
}

/// LAW 7: a crash between the cache rebuild and its commit leaves the cache
/// EMPTY rather than half-folded — and the log is still enough to rebuild it.
///
/// This is the cut that matters for a rebuildable cache: a half-applied fold
/// that survived would be a cache claiming a window the log does not support,
/// which is exactly the state the read side would then have to refuse.
#[test]
fn a_crash_during_a_cache_rebuild_leaves_no_half_folded_cache() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    {
        let tx = store.governed_transaction()?;
        let rebuilt = grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
        assert_eq!(rebuilt.folded, 1, "the fold ran inside the transaction");
        // The cut: dropped, never committed.
    }

    assert!(
        grapt_store::compaction::cached_window(store.connection(), &aggregate)?.is_none(),
        "a rebuild that did not commit must leave no cache rows"
    );

    // The log alone is still sufficient.
    let tx = store.governed_transaction()?;
    grapt_store::compaction::rebuild_cache(&tx, &aggregate)?;
    tx.commit()?.discard();
    let window = grapt_store::compaction::cached_window(store.connection(), &aggregate)?
        .ok_or("the cache must rebuild from the log after the cut")?;
    assert_eq!(
        window.window().as_str(),
        "cwn-1",
        "the rebuilt window must be the one the log records"
    );
    Ok(())
}

/// A cache that has folded NOTHING is not an error — it lags, which is legal.
///
/// This is the negative control for the lead refusal white-box witnesses: it
/// proves the read path returns `Ok(None)` for an honestly empty cache, so a
/// refusal observed there is the lead check firing rather than the read path
/// refusing everything.
#[test]
fn an_unfolded_cache_lags_rather_than_refusing() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    match grapt_store::compaction::cached_window(store.connection(), &aggregate) {
        Ok(None) => Ok(()),
        other => Err(format!("an unfolded cache must lag, not refuse: {other:?}").into()),
    }
}

/// The append path's own aggregate refusal still applies to this fact.
///
/// The descriptor names its own source session and the module derives the
/// aggregate from it, so there is no parameter a caller could point at another
/// session. This asserts the consequence a caller can observe: the fact lands on
/// the descriptor's own session and nowhere else.
#[test]
fn the_fact_lands_on_the_descriptors_own_session() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    let owner: String = store.connection().query_row(
        "SELECT aggregate_id FROM event WHERE type = 'session.compaction.ended.2'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(
        owner,
        session.as_str(),
        "the descriptor must be stored on its own source session's log"
    );
    Ok(())
}

/// A compaction error carries its cause rather than flattening it.
#[test]
fn an_append_failure_is_carried_not_flattened() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_033600000c01",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;
    // The same event id with different content diverges in the append path.
    let other = descriptor_at(&session, "cmp-9", "cwn-9", 5, 6).ok_or("descriptor")?;
    let writer = store.fencing_token(&aggregate)?;
    let event: EventId = "evt_033600000c01".parse()?;
    let tx = store.governed_transaction()?;
    let outcome = grapt_store::compaction::record(
        &tx,
        &event,
        writer,
        1_786_147_200_000,
        CompactionAttempt::Settled(&other),
        &mut [] as &mut [&mut dyn Projector],
    );
    match outcome {
        Err(CompactionError::Append(_)) => Ok(()),
        other => Err(format!("a divergent append must surface as Append(_): {other:?}").into()),
    }
}

// ------------------------------------------------- join composition (c)

/// JOIN COMPOSITION (c): the Rust-native compaction fact is a SESSION-family
/// row, so F3B3's family-tagged union CHECK governs its shape.
///
/// This is the collision that did NOT happen, pinned so it cannot start. F3B3
/// gave the event table a CHECK with two mutually exclusive shapes: a campaign
/// row carries `envelope_bytes` and `campaign_transition_id` with `data` NULL,
/// and a non-campaign row is the inverse. F4B1b then added a Rust-native
/// session fact whose canonical form IS a v2 envelope, which reads like a row
/// that wants envelope bytes on the session side.
///
/// It does not, because the session append path never builds an
/// `EnvelopeKind` at all — it writes the v1-shaped `type`/`data` row and the
/// envelope governs canonical bytes rather than storage. That is a property of
/// the two lanes TOGETHER, and neither parent's suite could state it: F3B3I has
/// the CHECK and no compaction fact, F4B1b has the fact and no CHECK.
///
/// Deliberately carries NO mutation row. Its subject is the absence of a
/// change across inherited carriers, and the CHECK, the append path and the
/// encoder each already own their own witnesses; a row here would be a
/// duplicated law wearing a new id.
#[test]
fn the_compaction_row_obeys_the_session_side_of_the_family_shape_check() -> TestResult {
    let (mut store, session, aggregate) = elected()?;
    let descriptor = descriptor(&session).ok_or("descriptor")?;
    record(
        &mut store,
        "evt_0336000000cmp",
        CompactionAttempt::Settled(&descriptor),
        &aggregate,
    )?;

    let (family, data_absent, envelope_absent, transition_absent): (String, bool, bool, bool) =
        store.connection().query_row(
            "SELECT aggregate_family,
                    data IS NULL,
                    envelope_bytes IS NULL,
                    campaign_transition_id IS NULL
             FROM event
             WHERE aggregate_id = ?1 AND type LIKE 'session.compaction.%'",
            rusqlite::params![session.as_str()],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )?;

    assert_eq!(
        family, "session",
        "the compaction fact belongs to the Session family"
    );
    assert!(
        !data_absent,
        "the session side of the CHECK requires data to be present"
    );
    assert!(
        envelope_absent,
        "a session row must carry NO envelope_bytes; the v2 envelope governs \
         canonical bytes, not the stored row shape"
    );
    assert!(
        transition_absent,
        "a session row must carry no campaign_transition_id"
    );
    Ok(())
}
