use grapt_form::{
    catalog::FormState,
    global::{CallDispatch, GlobalFormBinding},
    global_catalog::global_form,
    FormService,
};
use grapt_store::{AggregateId, EventStore};
use grapt_types::{
    CasOutcome, FormId, FormSuspensionReason, LocationRef, McpElicitationId, ToolCallId,
};
use serde_json::json;
type TestResult = Result<(), Box<dyn std::error::Error>>;
type LegacyRow = (String, String, String, String, String, String);

#[test]
fn a_stale_global_form_writer_is_refused_before_decoding_or_sequence_allocation() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-global-form-fence-{}.sqlite3",
        std::process::id()
    ));
    let owned_file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&path)?;
    drop(owned_file);
    let mut store = EventStore::open(&path, "global-form-fence-witness")?;
    FormService::initialize(&mut store)?;
    let id: FormId = "frm_fenced".parse()?;
    create(&mut store, &id, &binding()?)?;
    let aggregate = AggregateId::Form(id.clone());
    let stale = store.fencing_token(&aggregate)?;
    let mut replacement = EventStore::open(&path, "global-form-fence-witness")?;
    replacement.elect_writer(&aggregate)?;
    let event = "evt_stale_global".parse()?;
    let error = store
        .append(
            grapt_store::AppendInput {
                id: &event,
                writer: stale,
                aggregate: aggregate.clone(),
                kind_name: "form.replied",
                version: grapt_types::EventVersion::new(2).ok_or("version")?,
                created_ms: 2,
                data: &json!(null),
            },
            &mut [],
        )
        .err()
        .ok_or("stale writer appended")?;
    assert!(
        matches!(
            error,
            grapt_store::WriteError::Election(grapt_store::ElectionError::StaleWriter {
                family: "form",
                ..
            })
        ),
        "{error}"
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='form'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    FormService::answer_global_on(
        &mut replacement,
        &"evt_current_global".parse()?,
        &id,
        &binding()?,
        &json!({"name":"Ada"}),
        3,
    )?;
    let next: i64 = replacement.connection().query_row(
        "SELECT seq FROM event WHERE id='evt_current_global'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(next, 1, "the refused stale writer consumed a sequence");
    drop(store);
    drop(replacement);
    std::fs::remove_file(path)?;
    Ok(())
}

#[test]
fn global_capacity_refuses_the_next_form_and_recovers_after_settlement() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let owner = binding()?;
    for index in 0..128 {
        FormService::check_global_capacity(&store)?;
        let id: FormId = format!("frm_capacity_{index}").parse()?;
        store.elect_writer(&AggregateId::Form(id.clone()))?;
        FormService::create_global_on(
            &mut store,
            &format!("evt_capacity_{index}").parse()?,
            &id,
            &owner,
            &json!({"id":id.as_str(),"sessionID":"global","title":"Name","fields":[{"key":"name","type":"string"}]}),
            1,
        )?;
    }
    assert!(FormService::check_global_capacity(&store).is_err());
    let id: FormId = "frm_capacity_refused".parse()?;
    store.elect_writer(&AggregateId::Form(id.clone()))?;
    let refusal = FormService::create_global_on(&mut store,&"evt_capacity_refused".parse()?,&id,&owner,
        &json!({"id":id.as_str(),"sessionID":"global","title":"Name","fields":[{"key":"name","type":"string"}]}),1).err().ok_or("excess form accepted")?;
    assert!(refusal.to_string().contains("pending form bound"));
    assert_eq!(
        grapt_form::global_catalog::pending_global_forms(&store)?.len(),
        128
    );
    FormService::cancel_global_on(
        &mut store,
        &"evt_capacity_release".parse()?,
        &"frm_capacity_0".parse()?,
        &owner,
        2,
    )?;
    FormService::check_global_capacity(&store)?;
    Ok(())
}

#[test]
fn concurrent_reply_and_cancel_commit_exactly_one_terminal() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id: FormId = "frm_concurrent".parse()?;
    let owner = binding()?;
    create(&mut store, &id, &owner)?;
    let store = std::sync::Arc::new(std::sync::Mutex::new(store));
    let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
    let mut workers = Vec::new();
    for cancel in [false, true] {
        let store = std::sync::Arc::clone(&store);
        let barrier = std::sync::Arc::clone(&barrier);
        let id = id.clone();
        let owner = owner.clone();
        workers.push(std::thread::spawn(move || -> Result<CasOutcome, String> {
            barrier.wait();
            let mut store = store.lock().map_err(|_| "store poisoned")?;
            let event = if cancel {
                "evt_concurrent_cancel"
            } else {
                "evt_concurrent_reply"
            }
            .parse()
            .map_err(|_| "event identity")?;
            if cancel {
                FormService::cancel_global_on(&mut store, &event, &id, &owner, 2)
            } else {
                FormService::answer_global_on(
                    &mut store,
                    &event,
                    &id,
                    &owner,
                    &json!({"name":"Ada"}),
                    2,
                )
            }
            .map_err(|error| error.to_string())
        }));
    }
    barrier.wait();
    let outcomes = workers
        .into_iter()
        .map(|worker| worker.join().map_err(|_| "worker panicked".to_string())?)
        .collect::<Result<Vec<_>, String>>()?;
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == CasOutcome::Applied)
            .count(),
        1
    );
    assert_eq!(
        outcomes
            .iter()
            .filter(|outcome| **outcome == CasOutcome::AlreadyTerminal)
            .count(),
        1
    );
    let store = store.lock().map_err(|_| "store poisoned")?;
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='form'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 2);
    Ok(())
}

#[test]
fn replay_rebuilds_global_state_from_the_same_canonical_form_facts() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id: FormId = "frm_global".parse()?;
    let owner = binding()?;
    let aggregate = AggregateId::Form(id.clone());
    store.elect_writer(&aggregate)?;
    let info = json!({"id":id.as_str(),"sessionID":"global","title":"Name","fields":[{"key":"name","type":"string","required":true}]});
    let binding = json!({"elicitation":owner.elicitation.as_str(),"directory":owner.location.directory(),"connection":owner.connection.as_str()});
    for (index, (kind, mut data)) in [
        ("form.created", json!({"form":info})),
        ("form.suspended", json!({"reason":"process-loss"})),
        ("form.replied", json!({"answer":{"name":"Ada"}})),
    ]
    .into_iter()
    .enumerate()
    {
        data["formID"] = json!(id.as_str());
        data["owner"] = binding.clone();
        let event = format!("evt_replay_{index}").parse()?;
        let (_, notices) = store.append(
            grapt_store::AppendInput {
                id: &event,
                writer: store.fencing_token(&aggregate)?,
                aggregate: aggregate.clone(),
                kind_name: kind,
                version: grapt_types::EventVersion::new(2).ok_or("version")?,
                created_ms: 1,
                data: &data,
            },
            &mut [],
        )?;
        notices.discard();
    }
    assert!(
        global_form(&store, &id).is_err(),
        "missing projection cannot be reported as an absent form"
    );
    let caught = FormService::catch_up_global_on(&mut store, &id)?;
    assert_eq!(caught.projected, 3);
    caught.notices.discard();
    let after = global_form(&store, &id)?.ok_or("form")?;
    assert_eq!(after.binding, owner);
    assert_eq!(after.info, info);
    assert_eq!(after.state, FormState::Answered(json!({"name":"Ada"})));
    Ok(())
}

fn binding() -> Result<GlobalFormBinding, Box<dyn std::error::Error>> {
    Ok(GlobalFormBinding {
        elicitation: McpElicitationId::new("elicitation-one").ok_or("id")?,
        connection: McpElicitationId::new("process-one-connection-one").ok_or("connection")?,
        location: LocationRef::new("/global-form-test", None)?,
        // DELIBERATELY LEGACY. Leaving the shared fixture without a dispatch
        // keeps this whole existing battery running against the absent-field
        // shape, so the compatibility claim is carried by every pre-existing
        // row rather than only by the new ones below.
        call: None,
    })
}

fn create(store: &mut EventStore, id: &FormId, binding: &GlobalFormBinding) -> TestResult {
    store.elect_writer(&AggregateId::Form(id.clone()))?;
    FormService::create_global_on(
        store,
        &"evt_create_global".parse()?,
        id,
        binding,
        &json!({"id":id.as_str(), "sessionID":"global", "title":"Name",
            "metadata":{"kind":"mcp-elicitation"},
            "fields":[{"key":"name", "type":"string", "required":true}]}),
        1,
    )?;
    Ok(())
}

#[test]
fn global_forms_settle_without_a_fake_session_and_cannot_settle_twice() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id = "frm_global".parse()?;
    let owner = binding()?;
    create(&mut store, &id, &owner)?;
    assert!(grapt_form::catalog::pending_forms(&store)?.is_empty());
    let record = global_form(&store, &id)?.ok_or("record")?;
    assert_eq!(record.binding, owner);
    assert_eq!(record.state, FormState::Pending);
    assert_eq!(
        FormService::answer_global_on(
            &mut store,
            &"evt_reply_global".parse()?,
            &id,
            &owner,
            &json!({"name":"Ada"}),
            2
        )?,
        CasOutcome::Applied
    );
    assert_eq!(
        FormService::cancel_global_on(&mut store, &"evt_late_cancel".parse()?, &id, &owner, 3)?,
        CasOutcome::AlreadyTerminal
    );
    assert_eq!(
        global_form(&store, &id)?.ok_or("record")?.state,
        FormState::Answered(json!({"name":"Ada"}))
    );
    assert!(
        store.fencing_token(&AggregateId::Form(id)).is_err(),
        "terminal forms must release their cached writer epoch"
    );
    Ok(())
}

#[test]
fn wrong_binding_and_invalid_answer_leave_the_global_form_pending() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id = "frm_global".parse()?;
    let owner = binding()?;
    create(&mut store, &id, &owner)?;
    for changed in [
        GlobalFormBinding {
            location: LocationRef::new("/another-location", None)?,
            ..owner.clone()
        },
        GlobalFormBinding {
            connection: McpElicitationId::new("replacement").ok_or("id")?,
            ..owner.clone()
        },
    ] {
        let error = FormService::answer_global_on(
            &mut store,
            &"evt_wrong_global".parse()?,
            &id,
            &changed,
            &json!({"name":"Ada"}),
            2,
        )
        .err()
        .ok_or("wrong binding accepted")?;
        assert!(error.to_string().contains("binding mismatch"), "{error}");
    }
    assert!(FormService::answer_global_on(
        &mut store,
        &"evt_invalid_global".parse()?,
        &id,
        &owner,
        &json!({}),
        3
    )
    .is_err());
    assert_eq!(
        global_form(&store, &id)?.ok_or("record")?.state,
        FormState::Pending
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='form'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    Ok(())
}

#[test]
fn process_loss_suspends_and_a_later_answer_preserves_the_original_binding() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id = "frm_global".parse()?;
    let owner = binding()?;
    create(&mut store, &id, &owner)?;
    FormService::suspend_on(
        &mut store,
        &"evt_suspend_global".parse()?,
        &id,
        FormSuspensionReason::ProcessLoss,
        2,
    )?;
    assert_eq!(
        global_form(&store, &id)?.ok_or("record")?.state,
        FormState::Suspended(FormSuspensionReason::ProcessLoss)
    );
    assert_eq!(
        FormService::answer_global_on(
            &mut store,
            &"evt_after_loss".parse()?,
            &id,
            &owner,
            &json!({"name":"Ada"}),
            3
        )?,
        CasOutcome::Applied
    );
    assert_eq!(global_form(&store, &id)?.ok_or("record")?.binding, owner);
    Ok(())
}

#[test]
fn projection_failure_rolls_back_the_reply_and_preserves_the_next_sequence() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id = "frm_global".parse()?;
    let owner = binding()?;
    create(&mut store, &id, &owner)?;
    store.connection().execute_batch("CREATE TRIGGER refuse_form_reply BEFORE UPDATE ON form_request BEGIN SELECT RAISE(ABORT, 'witness persistence failure'); END;")?;
    let error = FormService::answer_global_on(
        &mut store,
        &"evt_failed_reply".parse()?,
        &id,
        &owner,
        &json!({"name":"Ada"}),
        2,
    )
    .err()
    .ok_or("failed projection was accepted")?;
    assert!(
        error.to_string().contains("witness persistence failure"),
        "{error}"
    );
    assert_eq!(
        global_form(&store, &id)?.ok_or("form")?.state,
        FormState::Pending
    );
    let count: i64 = store.connection().query_row(
        "SELECT COUNT(*) FROM event WHERE aggregate_family='form'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(count, 1);
    store
        .connection()
        .execute_batch("DROP TRIGGER refuse_form_reply")?;
    assert_eq!(
        FormService::answer_global_on(
            &mut store,
            &"evt_good_reply".parse()?,
            &id,
            &owner,
            &json!({"name":"Ada"}),
            3
        )?,
        CasOutcome::Applied
    );
    let seq: i64 = store.connection().query_row(
        "SELECT seq FROM event WHERE id='evt_good_reply'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(seq, 1);
    Ok(())
}

#[test]
fn legacy_projection_upgrade_preserves_session_rows_byte_for_byte() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    store.connection().execute_batch("CREATE TABLE form_request(id TEXT PRIMARY KEY, session_id TEXT NOT NULL, title TEXT NOT NULL, status TEXT NOT NULL, suspended_reason TEXT, answer TEXT);
        INSERT INTO form_request VALUES('frm_legacy','ses_legacy','Name — legacy','answered','process-loss','{\"name\":\"Ada\"}');")?;
    let read = |store: &EventStore| -> Result<LegacyRow, rusqlite::Error> {
        store.connection().query_row(
            "SELECT id,session_id,title,status,suspended_reason,answer FROM form_request",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
    };
    let before = read(&store)?;
    FormService::initialize(&mut store)?;
    assert_eq!(read(&store)?, before);
    FormService::initialize(&mut store)?;
    assert_eq!(read(&store)?, before);
    let binding: Option<String> =
        store
            .connection()
            .query_row("SELECT global_binding FROM form_request", [], |row| {
                row.get(0)
            })?;
    assert!(binding.is_none());
    Ok(())
}

/// The dispatch is OBSERVATIONAL: it records which tool call raised the form,
/// and it never decides who may settle it.
///
/// Both halves are asserted because both are ways to get this wrong. Folding
/// the field into binding equality makes a legacy form unsettleable — a form
/// created before the field existed compares `None` against `Some` and is
/// refused, from a field that owns nothing. Dropping it from the comparison
/// entirely admits two different dispatches claiming one form, which is a real
/// conflict.
#[test]
fn a_legacy_binding_still_settles_while_a_conflicting_dispatch_is_refused() -> TestResult {
    let mut store = EventStore::open_in_memory()?;
    FormService::initialize(&mut store)?;
    let id: FormId = "frm_global".parse()?;
    // Created WITHOUT a dispatch: exactly the shape every global form event
    // written before this field existed carries.
    let legacy = binding()?;
    assert!(legacy.call.is_none());
    create(&mut store, &id, &legacy)?;

    let dispatched = GlobalFormBinding {
        call: Some(CallDispatch {
            call: ToolCallId::new("call_one").ok_or("call")?,
            generation: 1,
            attempt: None,
        }),
        ..legacy.clone()
    };
    // A DIFFERENT dispatch on the same connection is a conflict, and the
    // refusal must name the dispatch rather than blame the connection.
    let conflicting = GlobalFormBinding {
        call: Some(CallDispatch {
            call: ToolCallId::new("call_two").ok_or("call")?,
            generation: 1,
            attempt: None,
        }),
        ..legacy.clone()
    };
    // The same call id under a DIFFERENT attempt is also a different dispatch:
    // a generation is minted per start, so this is a second admission, not the
    // same one seen twice.
    let reattempted = GlobalFormBinding {
        call: Some(CallDispatch {
            call: ToolCallId::new("call_one").ok_or("call")?,
            generation: 2,
            attempt: None,
        }),
        ..legacy.clone()
    };

    // Absence is compatible in EITHER direction, so a legacy form settles under
    // code that now carries a dispatch.
    assert!(legacy.compatible_with(&dispatched));
    assert!(dispatched.compatible_with(&legacy));
    assert!(!dispatched.compatible_with(&conflicting));
    assert!(!dispatched.compatible_with(&reattempted));
    // Ownership coordinates still decide ownership.
    assert!(!dispatched.compatible_with(&GlobalFormBinding {
        connection: McpElicitationId::new("replacement").ok_or("id")?,
        ..dispatched.clone()
    }));

    // And through the real write path: the legacy form answers under a
    // dispatch-carrying binding rather than being bricked by it.
    FormService::answer_global_on(
        &mut store,
        &"evt_legacy_answer".parse()?,
        &id,
        &dispatched,
        &json!({"name":"Ada"}),
        2,
    )?;
    assert_eq!(
        global_form(&store, &id)?.ok_or("record")?.state,
        FormState::Answered(json!({"name":"Ada"}))
    );
    Ok(())
}

/// A REPEATED CALL ID ACROSS SESSIONS AND ATTEMPTS IS LEGAL, and must not bind.
///
/// The product enforces call-id uniqueness only within one `McpSession`'s
/// in-flight and remembered sets, and that remembered ledger evicts. Nothing
/// refuses the same id on a second connection, in a second process, or after
/// eviction. So this drives the case directly: the SAME id,
/// twice, distinguished only by the scope it is carried with.
#[test]
fn a_repeated_call_id_across_connections_and_attempts_does_not_bind() -> TestResult {
    let repeated = ToolCallId::new("call_j04_journey_journey_echo").ok_or("call")?;
    let base = binding()?;

    let first = GlobalFormBinding {
        call: Some(CallDispatch {
            call: repeated.clone(),
            generation: 1,
            attempt: None,
        }),
        ..base.clone()
    };
    // Same id, second connection — the shape two journeys against one fixture
    // actually produce, and the one the delivered corpus contains.
    let second_connection = GlobalFormBinding {
        connection: McpElicitationId::new("process-two-connection-one").ok_or("connection")?,
        call: Some(CallDispatch {
            call: repeated.clone(),
            generation: 1,
            attempt: None,
        }),
        ..base.clone()
    };
    // Same id, same connection, second attempt.
    let second_attempt = GlobalFormBinding {
        call: Some(CallDispatch {
            call: repeated.clone(),
            generation: 2,
            attempt: None,
        }),
        ..base.clone()
    };

    assert!(
        !first.compatible_with(&second_connection),
        "a repeated call id on another connection must not bind"
    );
    assert!(
        !first.compatible_with(&second_attempt),
        "a repeated call id under another attempt must not bind"
    );
    // The identity is the TRIPLE, so the same triple is the same dispatch.
    assert!(first.compatible_with(&GlobalFormBinding {
        call: Some(CallDispatch {
            call: repeated,
            generation: 1,
            attempt: None,
        }),
        ..base
    }));
    Ok(())
}

/// Old payloads decode, and a binding with no dispatch is written EXACTLY as it
/// was before the field existed.
///
/// The byte identity is the part that matters: if a dispatch-less binding
/// serialised differently, every pre-existing global form event would disagree
/// with what today's code writes for the same facts, and the compatibility
/// claim would be about decode only.
#[test]
fn a_dispatchless_binding_round_trips_byte_identically_and_corruption_is_refused() -> TestResult {
    let legacy = json!({
        "elicitation": "elicitation-one",
        "directory": "/global-form-test",
        "connection": "process-one-connection-one"
    });
    let decoded = GlobalFormBinding::decode(&legacy)?;
    assert!(
        decoded.call.is_none(),
        "an absent dispatch is absent, not defaulted"
    );
    assert_eq!(
        serde_json::to_string(&decoded.wire())?,
        serde_json::to_string(&legacy)?,
        "a dispatch-less binding must serialise exactly as a pre-field one did"
    );

    let carried = GlobalFormBinding {
        call: Some(CallDispatch {
            call: ToolCallId::new("call_one").ok_or("call")?,
            generation: 7,
            attempt: None,
        }),
        ..decoded
    };
    let round = GlobalFormBinding::decode(&carried.wire())?;
    assert_eq!(
        round, carried,
        "a carried dispatch must survive a round trip"
    );

    // PRESENT BUT UNREADABLE IS CORRUPTION, not absence. A binding that carries
    // a dispatch it cannot parse must refuse rather than quietly become one
    // that never had a dispatch.
    for broken in [
        json!({"elicitation":"e","directory":"/d","connection":"c","dispatch":"not-an-object"}),
        json!({"elicitation":"e","directory":"/d","connection":"c","dispatch":{"call":"c1"}}),
        json!({"elicitation":"e","directory":"/d","connection":"c",
               "dispatch":{"call":"c1","generation":-1}}),
        json!({"elicitation":"e","directory":"/d","connection":"c","dispatch":null}),
    ] {
        assert!(
            GlobalFormBinding::decode(&broken).is_err(),
            "a corrupt dispatch decoded instead of refusing: {broken}"
        );
    }
    Ok(())
}

#[test]
fn a_dispatch_with_an_attempt_round_trips_exactly() -> TestResult {
    let binding = GlobalFormBinding {
        elicitation: McpElicitationId::new("elicitation-one").ok_or("id")?,
        connection: McpElicitationId::new("process-one-connection-one").ok_or("connection")?,
        location: LocationRef::new("/global-form-test", None)?,
        call: Some(CallDispatch {
            call: ToolCallId::new("call_rt").ok_or("call")?,
            generation: 9,
            attempt: Some("pat_rt".to_owned()),
        }),
    };
    let wire = binding.wire();
    let back = GlobalFormBinding::decode(&wire).map_err(|e| format!("{e} over {wire}"))?;
    assert_eq!(back, binding, "wire was {wire}");
    Ok(())
}

#[test]
fn recovery_skips_a_corrupt_global_form_and_keeps_the_healthy_one() -> TestResult {
    // Bad evidence, and a value SQLite cannot read as its declared type.
    for (index, damage) in [
        "UPDATE form_request SET global_binding = 'not json' WHERE id = 'frm_broken'",
        "UPDATE form_request SET suspended_reason = 7 WHERE id = 'frm_broken'",
    ]
    .into_iter()
    .enumerate()
    {
        recovery_skips_damage(index, damage)?;
    }
    Ok(())
}

fn recovery_skips_damage(index: usize, damage: &str) -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-form-recovery-skip-{}-{index}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "test")?;
    FormService::initialize(&mut store)?;
    let owner = binding()?;
    for (form, event) in [("frm_broken", "evt_broken"), ("frm_healthy", "evt_healthy")] {
        let id: FormId = form.parse()?;
        store.elect_writer(&AggregateId::Form(id.clone()))?;
        FormService::create_global_on(
            &mut store,
            &event.parse()?,
            &id,
            &owner,
            &json!({"id":form, "sessionID":"global", "title":"Name",
                "metadata":{"kind":"mcp-elicitation"},
                "fields":[{"key":"name", "type":"string", "required":true}]}),
            1,
        )?;
    }
    drop(store);
    // The store refuses ungoverned writes, so the damage comes from a separate
    // connection, as a foreign writer would leave it.
    rusqlite::Connection::open(&path)?.execute(damage, [])?;
    let store = EventStore::open(&path, "test")?;

    assert!(grapt_form::global_catalog::pending_global_forms(&store).is_err());
    let (records, skipped) = grapt_form::global_catalog::pending_global_forms_for_recovery(&store)?;
    let healthy: Vec<&str> = records.iter().map(|record| record.id.as_str()).collect();
    assert_eq!(healthy, ["frm_healthy"]);
    assert_eq!(skipped.len(), 1);
    assert_eq!(
        skipped[0].id.as_ref().map(FormId::as_str),
        Some("frm_broken")
    );
    drop(store);
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn recovery_reaches_a_pending_form_behind_a_full_pass_of_suspended_ones() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-form-recovery-order-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "test")?;
    FormService::initialize(&mut store)?;
    let owner = binding()?;
    for form in ["frm_suspended", "frm_waiting"] {
        let id: FormId = form.parse()?;
        store.elect_writer(&AggregateId::Form(id.clone()))?;
        FormService::create_global_on(
            &mut store,
            &format!("evt_create_{form}").parse()?,
            &id,
            &owner,
            &json!({"id":form, "sessionID":"global", "title":"Name",
                "fields":[{"key":"name", "type":"string"}]}),
            1,
        )?;
    }
    FormService::suspend_on(
        &mut store,
        &"evt_suspend_first".parse()?,
        &"frm_suspended".parse()?,
        FormSuspensionReason::ProcessLoss,
        2,
    )?;
    drop(store);
    // Past the capacity a writer of this build enforces: 128 more suspended
    // forms, all ahead of the pending one in row order.
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute_batch(
        "CREATE TEMP TABLE copy AS SELECT * FROM form_request WHERE id = 'frm_suspended'",
    )?;
    for index in 0..128 {
        raw.execute("UPDATE copy SET id = ?1", [format!("frm_copy_{index:03}")])?;
        raw.execute("INSERT INTO form_request SELECT * FROM copy", [])?;
    }
    raw.execute(
        "UPDATE form_request SET rowid = (SELECT MAX(rowid) + 1 FROM form_request)
           WHERE id = 'frm_waiting'",
        [],
    )?;
    drop(raw);
    let store = EventStore::open(&path, "test")?;
    let (records, _) = grapt_form::global_catalog::pending_global_forms_for_recovery(&store)?;
    assert!(
        records
            .iter()
            .any(|record| record.id.as_str() == "frm_waiting"),
        "the pending form waited behind suspended ones"
    );
    drop(store);
    let _ = std::fs::remove_file(&path);
    Ok(())
}

#[test]
fn unreadable_pending_rows_never_displace_a_healthy_suspended_form() -> TestResult {
    let path = std::env::temp_dir().join(format!(
        "grapt-form-recovery-budget-{}.db",
        std::process::id()
    ));
    let _ = std::fs::remove_file(&path);
    let mut store = EventStore::open(&path, "test")?;
    FormService::initialize(&mut store)?;
    let id: FormId = "frm_suspended".parse()?;
    create(&mut store, &id, &binding()?)?;
    FormService::suspend_on(
        &mut store,
        &"evt_suspend_older".parse()?,
        &id,
        FormSuspensionReason::ProcessLoss,
        2,
    )?;
    drop(store);
    // Pending rows whose identity no build can read, as a foreign writer or
    // a disk could leave them.
    let raw = rusqlite::Connection::open(&path)?;
    raw.execute_batch(
        "CREATE TEMP TABLE copy AS SELECT * FROM form_request WHERE id = 'frm_suspended';
         UPDATE copy SET status = 'pending'",
    )?;
    for index in 0..128_u32 {
        raw.execute("UPDATE copy SET id = ?1", [index.to_be_bytes().to_vec()])?;
        raw.execute("INSERT INTO form_request SELECT * FROM copy", [])?;
    }
    drop(raw);
    let store = EventStore::open(&path, "test")?;
    let (records, skipped) = grapt_form::global_catalog::pending_global_forms_for_recovery(&store)?;
    assert!(
        records.iter().any(|record| record.id == id),
        "unreadable pending rows displaced the suspended form"
    );
    assert_eq!(skipped.len(), 128, "every unreadable row is named");
    drop(store);
    let _ = std::fs::remove_file(&path);
    Ok(())
}
