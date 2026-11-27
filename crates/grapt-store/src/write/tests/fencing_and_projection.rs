use super::*;
#[test]
fn an_unelected_store_cannot_obtain_a_fencing_token() -> TestResult {
    // The store is open and the database is Rust-owned; what is missing is
    // an election. Separated from the database-side `UnknownWriter` case on
    // purpose: this one is "I never asked to be the writer", and collapsing
    // the two would let a caller read a plausible refusal for the wrong
    // reason.
    let store = EventStore::open_in_memory()?;
    let aggregate = agg()?;
    let refused = store.fencing_token(&aggregate);
    assert!(
        matches!(
            refused,
            Err(WriteError::NotElected {
                family: "session",
                ..
            })
        ),
        "an unelected store holds no token, got {refused:?}"
    );
    Ok(())
}

#[test]
fn append_never_elects_on_a_callers_behalf() -> TestResult {
    // If append elected, fencing would be decorative: every writer would
    // take ownership on its way past the check and no epoch could ever be
    // stale. Two facts prove it does not — an append cannot even be
    // ATTEMPTED without a prior election (there is no token to pass), and a
    // successful append leaves the elected row exactly as the election left
    // it.
    let mut store = EventStore::open_in_memory()?;
    let aggregate = agg()?;
    assert!(store.fencing_token(&aggregate).is_err());
    let elections: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM aggregate_writer", [], |r| r.get(0))?;
    assert_eq!(elections, 0, "no election exists before one is asked for");

    store.elect_writer(&aggregate)?;
    let writer = store.fencing_token(&aggregate)?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    )?;
    let (owner, epoch): (String, i64) =
        store
            .connection()
            .query_row("SELECT owner_id, epoch FROM aggregate_writer", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
    assert_eq!(
        (owner.as_str(), epoch),
        (store.owner().as_str(), 1),
        "a successful append must not have re-elected anybody"
    );
    Ok(())
}

#[test]
fn a_stale_writer_is_refused_before_any_sequence_is_consumed() -> TestResult {
    // Two store instances on ONE file are two owners, exactly as two
    // processes would be. A elects (epoch 1) and keeps holding it; B takes
    // ownership (epoch 2); A's next append must refuse.
    //
    // The refusal alone is not the law. The law is that the refusal happens
    // BEFORE the sequence is allocated, so the live append that follows
    // proves the gap was never consumed: it lands at seq 0, not seq 1.
    let (dir, db) = scratch_db("fence")?;
    let aggregate = agg()?;
    let mut a = EventStore::open(&db, "0.0.1")?;
    let mut b = EventStore::open(&db, "0.0.1")?;
    assert_ne!(a.owner(), b.owner(), "two instances are two owners");

    a.elect_writer(&aggregate)?;
    let stale = a.fencing_token(&aggregate)?;
    assert_eq!(
        b.elect_writer(&aggregate)?,
        Election::Transferred {
            epoch: match WriterEpoch::new(2) {
                Some(v) => v,
                None => panic!("epoch 2"),
            },
            previous: WriterEpoch::FIRST,
        }
    );

    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let refused = append_only(
        &mut a,
        input(stale, &aggregate, &"evt_stale".parse()?, &d),
        &mut [],
    );
    assert!(
        matches!(
            refused,
            Err(WriteError::Election(ElectionError::StaleWriter {
                submitted: 1,
                elected: 2,
                ..
            }))
        ),
        "the lost owner must be refused, got {refused:?}"
    );

    let live = b.fencing_token(&aggregate)?;
    assert_eq!(
        append_only(
            &mut b,
            input(live, &aggregate, &"evt_live".parse()?, &d),
            &mut []
        )?,
        Appended::Committed { seq: 0 },
        "the refused append consumed no sequence"
    );
    let rows: i64 = b
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(rows, 1, "the refused append wrote no row");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn two_connections_of_one_owner_do_not_fence_each_other() -> TestResult {
    // Ownership, not connection count, is what the epoch fences. A writer
    // that opened a second handle and thereby locked out its own first one
    // could never run a contended append, so re-election by the SAME owner
    // must retain the epoch and both connections must keep writing.
    let (dir, db) = scratch_db("one-owner")?;
    let aggregate = agg()?;
    let mut first = EventStore::open(&db, "0.0.1")?;
    let mut second = EventStore::open_as(&db, "0.0.1", first.owner())?;
    assert_eq!(first.owner(), second.owner());

    assert_eq!(
        first.elect_writer(&aggregate)?,
        Election::Elected {
            epoch: WriterEpoch::FIRST
        }
    );
    assert_eq!(
        second.elect_writer(&aggregate)?,
        Election::Retained {
            epoch: WriterEpoch::FIRST
        },
        "one owner's second connection is not an ownership transfer"
    );

    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let one = first.fencing_token(&aggregate)?;
    let two = second.fencing_token(&aggregate)?;
    assert_eq!(
        append_only(
            &mut first,
            input(one, &aggregate, &"evt_c1".parse()?, &d),
            &mut []
        )?,
        Appended::Committed { seq: 0 }
    );
    assert_eq!(
        append_only(
            &mut second,
            input(two, &aggregate, &"evt_c2".parse()?, &d),
            &mut []
        )?,
        Appended::Committed { seq: 1 },
        "the second connection of the same owner is still a legal writer"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn an_unelected_aggregate_refuses_a_fabricated_token() -> TestResult {
    // A token held for ONE aggregate is not authority over another. The
    // database has no election for the second aggregate, so the refusal is
    // `UnknownWriter` rather than `StaleWriter`: an unknown writer is not a
    // lost one, and defaulting it to "epoch 1 is fine" would let an
    // unelected owner write an aggregate's first event.
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let other = session("ses_w9")?;
    let d = json!({"sessionID":"ses_w9","inputID":"msg_w1"});
    let refused = append_only(
        &mut store,
        input(writer, &other, &"evt_w9".parse()?, &d),
        &mut [],
    );
    assert!(
        matches!(
            refused,
            Err(WriteError::Election(ElectionError::UnknownWriter { .. }))
        ),
        "an unelected aggregate must refuse, got {refused:?}"
    );
    let rows: i64 =
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM event_sequence", [], |r| r.get(0))?;
    assert_eq!(rows, 0, "the refusal allocated no cursor");
    Ok(())
}

#[test]
fn a_stale_writer_is_refused_before_the_stored_row_is_read() -> TestResult {
    // The ordering claim, made observable.
    //
    // Inside one transaction "refused after allocating" and "refused before
    // allocating" leave the same durable trace, because the rollback erases
    // either one — so the sequence count cannot separate them. What DOES
    // separate them is which refusal the caller receives: a writer with no
    // authority over this aggregate must be told it is stale, not handed a
    // diagnostic about a row it was never entitled to read.
    //
    // Both faults are staged at once: the stored row is undecodable AND the
    // writer is stale. Only the fence's position decides the verdict.
    let (dir, db) = scratch_db("fence-order")?;
    let aggregate = agg()?;
    let mut a = EventStore::open(&db, "0.0.1")?;
    a.elect_writer(&aggregate)?;
    let stale = a.fencing_token(&aggregate)?;
    hostile_rows::install_cursor(a.connection(), a.gate(), &aggregate, 0)?;
    hostile_rows::install_event(
        a.connection(),
        a.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_w1",
            seq: 0,
            created_ms: 1786060800000,
            versioned_type: "session.imaginary.7",
            data_text: "{}",
            writer_epoch: Some(1),
        },
    )?;

    let mut b = EventStore::open(&db, "0.0.1")?;
    b.elect_writer(&aggregate)?;

    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let refused = append_only(
        &mut a,
        input(stale, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    );
    assert!(
        matches!(
            refused,
            Err(WriteError::Election(ElectionError::StaleWriter { .. }))
        ),
        "the fence must decide before the stored row is decoded, got {refused:?}"
    );
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn the_committed_row_carries_its_elected_epoch() -> TestResult {
    // The epoch is a persisted coordinate of the stored fact, not merely an
    // admission check that is forgotten afterwards. Without this, a row
    // written under epoch 2 would be indistinguishable from one written
    // under epoch 1, and the divergence law below could not exist.
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    )?;
    let (family, epoch): (String, Option<i64>) = store.connection().query_row(
        "SELECT aggregate_family, writer_epoch FROM event WHERE id = 'evt_w1'",
        [],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    assert_eq!(
        (family.as_str(), epoch),
        ("session", Some(1)),
        "the committed row records its family and its elected epoch"
    );
    Ok(())
}

#[test]
fn a_replay_under_a_different_epoch_diverges() -> TestResult {
    // The writer epoch is inside the v2 envelope's canonical bytes, so the
    // same event id under a different epoch names a DIFFERENT fact.
    // Reconciling it as `Idempotent` would let a new owner silently adopt a
    // lost owner's write as its own.
    //
    // The successor is CURRENT, not stale — it holds epoch 2 and epoch 2 is
    // elected — so the fence passes and the identity comparison is what
    // decides. Every other comparison field is held identical on purpose:
    // id, family, aggregate, type, created and data all match, so only the
    // epoch can produce the verdict.
    let (dir, db) = scratch_db("epoch-diverge")?;
    let aggregate = agg()?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});

    let mut first = EventStore::open(&db, "0.0.1")?;
    first.elect_writer(&aggregate)?;
    let held = first.fencing_token(&aggregate)?;
    append_only(
        &mut first,
        input(held, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    )?;

    // Control, taken while epoch 1 is still elected: the identical retry
    // under the SAME epoch reconciles. Without it the divergence below
    // would also pass for a store that refused every retry.
    assert_eq!(
        append_only(
            &mut first,
            input(held, &aggregate, &"evt_w1".parse()?, &d),
            &mut []
        )?,
        Appended::Idempotent { seq: 0 },
    );

    let mut successor = EventStore::open(&db, "0.0.1")?;
    successor.elect_writer(&aggregate)?;
    let later = successor.fencing_token(&aggregate)?;
    assert_ne!(
        later.epoch(),
        held.epoch(),
        "the retry must carry a different epoch or it proves nothing"
    );

    let out = append_only(
        &mut successor,
        input(later, &aggregate, &"evt_w1".parse()?, &d),
        &mut [],
    );
    assert!(
        matches!(out, Err(WriteError::Divergence { .. })),
        "a retry under a different epoch is a different fact, got {out:?}"
    );

    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn a_new_unfenced_row_is_refused_by_the_database() -> TestResult {
    // The no-new-v1 law, enforced where a Rust-side check cannot reach: the
    // INSERT never goes near `append_within`. `writer_epoch IS NULL` is
    // exactly `EventFencing::LegacyUnfenced`, and no new one may be written.
    let (store, aggregate) = elected_store()?;
    hostile_rows::install_cursor(store.connection(), store.gate(), &aggregate, 0)?;
    let refused = hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_unfenced",
            seq: 0,
            created_ms: 1786060800000,
            versioned_type: "session.input.admitted.1",
            data_text: "{}",
            writer_epoch: None,
        },
    );
    let message = match refused {
        Ok(_) => panic!("a new unfenced row must be refused by the database"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("no new v1 write"),
        "the refusal must name the law it enforces, got {message}"
    );
    let rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM event", [], |r| r.get(0))?;
    assert_eq!(rows, 0, "the aborted insert left nothing behind");

    // The same statement with a real epoch is accepted, so the trigger
    // refuses the missing FENCING and not the raw insert itself.
    hostile_rows::install_event(
        store.connection(),
        store.gate(),
        hostile_rows::ForeignEvent {
            aggregate: &aggregate,
            id: "evt_fenced",
            seq: 0,
            created_ms: 1786060800000,
            versioned_type: "session.input.admitted.1",
            data_text: "{}",
            writer_epoch: Some(1),
        },
    )?;
    Ok(())
}

#[test]
fn catch_up_refuses_a_leading_watermark_instead_of_reporting_it_caught_up() -> TestResult {
    // The failure mode this closes is SILENT, which is why it needs its own
    // witness. A watermark past the durable head selects no rows, so a
    // recovery pass that read the stored position directly would fold
    // nothing, return `projected: 0`, and look exactly like a healthy
    // caught-up projection — discarding "may lag but never lead" at the one
    // moment it is supposed to be enforced.
    //
    // It lives here rather than in `tests/projection_watermark.rs` because
    // staging the leading watermark needs the store's own governed lift.
    let (mut store, aggregate) = elected_store()?;
    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    let writer = store.fencing_token(&aggregate)?;
    let mut counter = CountingProjector {
        rows: 0,
        fail: false,
    };
    append_only(
        &mut store,
        input(writer, &aggregate, &"evt_lead".parse()?, &d),
        &mut [&mut counter],
    )?;

    // Head is 0; the projection claims 5.
    hostile_rows::install_watermark(
        store.connection(),
        store.gate(),
        counter.projection(),
        &aggregate,
        5,
    )?;
    match store.catch_up(&aggregate, &mut counter) {
        Err(WriteError::WatermarkLeads {
            watermark: 5,
            head: Some(0),
            ..
        }) => {}
        other => panic!("a recovery pass must refuse a leading watermark: {other:?}"),
    }
    // And the refusal folded nothing: the projector was not run again.
    assert_eq!(counter.rows, 1, "a refused catch-up must project nothing");
    Ok(())
}

#[test]
fn open_refuses_an_incompatible_preexisting_watermark_table() -> TestResult {
    // Same hazard as the pre-fencing event table, for F3B2's own table.
    // `CREATE TABLE IF NOT EXISTS` accepts a preexisting table of ANY shape,
    // so a database carrying a one-dimensional `projection_watermark` would
    // open happily and then key every projection and every aggregate to one
    // row — silently reporting one projection's progress as another's.
    let dir = std::env::temp_dir().join(format!("grapt-open-wm-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir)?;
    let db = dir.join("watermark.db");
    {
        let conn = Connection::open(&db)?;
        conn.execute_batch(
            "CREATE TABLE session (id TEXT PRIMARY KEY);
             CREATE TABLE projection_watermark (projection TEXT PRIMARY KEY, seq INTEGER NOT NULL)",
        )?;
    }
    // The MESSAGE, not merely the refusal: the store creates several tables
    // during preparation and any of them could be what fails. This asserts
    // the column check on this exact table is what refused.
    let message = match EventStore::open(&db, "0.0.1") {
        Ok(_) => panic!("an incompatible watermark table must refuse to open"),
        Err(error) => error.to_string(),
    };
    assert!(
        message.contains("incompatible preexisting table projection_watermark")
            && message.contains("missing column aggregate_family"),
        "the watermark COLUMN check must be what refuses, got {message}"
    );
    let v: u32 = Connection::open(&db)?.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    assert_eq!(v, 0, "the refusal left the database unowned");
    let _ = std::fs::remove_dir_all(&dir);
    Ok(())
}

#[test]
fn the_store_key_is_family_scoped() -> TestResult {
    // Identity alone is not a key. A mail stream whose identity string equals
    // a session's must not share that session's cursor, or one aggregate's
    // appends would renumber the other's.
    let (mut store, aggregate) = elected_store()?;
    let writer = store.fencing_token(&aggregate)?;
    let collide = AggregateId::Mail(
        grapt_types::remote_mail::MailStreamId::try_new("ses_w0")
            .ok()
            .ok_or("mail stream id")?,
    );
    assert_eq!(
        crate::election::identity_of(&collide),
        crate::election::identity_of(&aggregate),
        "the two identities must collide or this proves nothing"
    );
    // A cursor already stands at 41 for the colliding MAIL key.
    hostile_rows::install_cursor(store.connection(), store.gate(), &collide, 41)?;

    let d = json!({"sessionID":"ses_w0","inputID":"msg_w1"});
    assert_eq!(
        append_only(
            &mut store,
            input(writer, &aggregate, &"evt_w1".parse()?, &d),
            &mut []
        )?,
        Appended::Committed { seq: 0 },
        "the session's first event is seq 0, not 42"
    );
    let mail_cursor: i64 = store.connection().query_row(
        "SELECT seq FROM event_sequence
         WHERE aggregate_family = 'mail' AND aggregate_id = 'ses_w0'",
        [],
        |r| r.get(0),
    )?;
    assert_eq!(mail_cursor, 41, "the mail cursor was not disturbed");
    Ok(())
}
