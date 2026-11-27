//! Native structural admission and compaction recovery past the raised bounds.
use super::*;
const STRUCTURAL_CALLS: usize = 1281;
// Restarts and committed summaries replace the confirmed prefix. Leave room
// for conservative admission so these journeys isolate structural row bounds.
const STRUCTURAL_CONTEXT_TOKENS: u64 = 500_000;
const FIRST_BATCHES: usize = STRUCTURAL_CALLS.div_ceil(32);

fn selection_artifact(
    root: &Path,
    descriptor: &grapt_types::CompactionDescriptor,
) -> serde_json::Value {
    let grapt_types::RetainedHistory::DeterministicSelection {
        descriptor: selection,
    } = descriptor.retained()
    else {
        panic!("compaction descriptor has no deterministic selection")
    };
    assert!(
        selection.length() <= 4096,
        "selection metadata exceeded its bound"
    );
    let artifacts = FilesystemArtifactStore::open(
        root.join("dogfood-artifacts"),
        b"secode-live-headless1-artifacts-v1".to_vec(),
        Box::new(SystemClock),
    )
    .unwrap_or_else(|error| panic!("compaction artifacts are unreadable: {error}"));
    let page = artifacts
        .fetch(
            selection,
            0,
            ReadBudget::new(selection.length())
                .unwrap_or_else(|| panic!("selection artifact is empty")),
        )
        .unwrap_or_else(|error| panic!("selection artifact fetch failed: {error}"));
    assert!(page.next_offset().is_none());
    serde_json::from_slice(page.bytes())
        .unwrap_or_else(|error| panic!("selection artifact is not JSON: {error}"))
}

fn retained_call_count(root: &Path, session: &SessionId) -> usize {
    let connection = rusqlite::Connection::open_with_flags(
        root.join("database.sqlite3"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap_or_else(|error| panic!("retained call evidence is unreadable: {error}"));
    let (_, _, descriptor) = grapt_store::compaction::latest_descriptor(&connection, session)
        .unwrap_or_else(|error| panic!("compaction descriptor is unreadable: {error}"))
        .unwrap_or_else(|| panic!("first structural journey committed no descriptor"));
    let selection = selection_artifact(root, &descriptor);
    let sequence = |field: &str| {
        selection[field]
            .as_u64()
            .and_then(|value| i64::try_from(value).ok())
            .unwrap_or_else(|| panic!("selection {field} is not a SQLite sequence"))
    };
    let count: i64 = connection
        .query_row(
            "SELECT COUNT(*) FROM (SELECT 1 FROM event
             WHERE aggregate_family = 'session' AND aggregate_id = ?1
               AND (seq >= ?2 OR seq < 0) AND seq <= ?3
               AND type IN ('session.tool.input.started.1', 'session.tool.input.started.2')
             LIMIT 1281)",
            (session.as_str(), sequence("from"), sequence("source_head")),
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("retained call count is unreadable: {error}"));
    let count = usize::try_from(count)
        .unwrap_or_else(|error| panic!("retained call count is invalid: {error}"));
    assert!((1..STRUCTURAL_CALLS - 2).contains(&count));
    count
}

fn append_execution_pairs(root: &Path, session: &SessionId, start: usize, count: usize) {
    let mut store = EventStore::open(
        &root.join("database.sqlite3"),
        &format!("swr-j03-events-{start}"),
    )
    .unwrap_or_else(|error| panic!("event-bound evidence store is unreadable: {error}"));
    store
        .elect_writer(&AggregateId::Session(session.clone()))
        .unwrap_or_else(|error| panic!("event-bound writer election failed: {error}"));
    let ids = OsRandomIds::seed()
        .unwrap_or_else(|error| panic!("event-bound identity authority failed: {error}"));
    let clock = SystemClock;
    let authorities = DeterministicAuthorities::new(&clock, &ids);
    seed_execution_pairs(&mut store, &authorities, session, start, count)
        .unwrap_or_else(|error| panic!("event-bound fixture seed failed: {error}"));
}

fn seed_execution_pairs(
    store: &mut EventStore,
    authorities: &DeterministicAuthorities<'_>,
    session: &SessionId,
    start: usize,
    count: usize,
) -> Result<(), String> {
    if count == 0 {
        return Ok(());
    }
    let first = ExecutionId::new(format!("exe_{start:x}")).ok_or("execution")?;
    grapt_session::execution::begin(store, authorities, session, &first)?;
    grapt_session::execution::settle(
        store,
        authorities,
        session,
        &first,
        grapt_session::execution::Outcome::Succeeded,
    )?;
    // Compact IDs keep this row-bound fixture below the unchanged 4 MiB data
    // wall. Batch only its prehistory; admission and compaction run natively.
    let aggregate = AggregateId::Session(session.clone());
    let writer = store.fencing_token(&aggregate).map_err(|e| e.to_string())?;
    let tx = store.governed_transaction().map_err(|e| e.to_string())?;
    for index in start + 1..start.saturating_add(count) {
        let execution = ExecutionId::new(format!("exe_{index:x}")).ok_or("execution")?;
        let data =
            serde_json::json!({"sessionID":session.as_str(),"executionID":execution.as_str()});
        assert!(
            data.to_string().len() <= 64,
            "execution prehistory must leave room below the unchanged 4 MiB window wall"
        );
        for kind in ["session.execution.started", "session.execution.succeeded"] {
            let event = format!("evt_{}", authorities.mint().as_str())
                .parse::<grapt_types::EventId>()
                .map_err(|e| e.to_string())?;
            let appended = EventStore::append_within(
                &tx,
                grapt_store::AppendInput {
                    id: &event,
                    aggregate: aggregate.clone(),
                    writer,
                    kind_name: kind,
                    version: grapt_types::EventVersion::new(1).ok_or("version")?,
                    created_ms: authorities.now().as_millis(),
                    data: &data,
                },
                &mut [],
            )
            .map_err(|e| e.to_string())?;
            if !matches!(appended, grapt_store::Appended::Committed { .. }) {
                return Err("fixture event was not committed".into());
            }
        }
    }
    tx.commit().map_err(|e| e.to_string())?.discard();
    let last = ExecutionId::new(format!("exe_{:x}", start + count - 1)).ok_or("execution")?;
    let read = grapt_session::execution::read(store.connection(), session, &last)?;
    assert_eq!(read.len(), 2);
    assert_eq!(
        read[1].outcome,
        Some(grapt_session::execution::Outcome::Succeeded)
    );
    Ok(())
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn swr_j03_structural_call_bound_compacts_and_rebuilds_the_ordinary_request() {
    let _journey = process_journey_custody();
    let root = root("swr-structural-compaction");
    prepare(&root, 500_000);
    prepare_model_inventory(&root, STRUCTURAL_CONTEXT_TOKENS);
    fs::write(root.join("t"), b"exact retained fact")
        .unwrap_or_else(|error| panic!("fixture write failed: {error}"));
    let calls: Vec<&'static str> = (1..=STRUCTURAL_CALLS)
        .map(|ordinal| &*Box::leak(format!("call_c_{ordinal:03}").into_boxed_str()))
        .collect();
    let mut scripts: Vec<Script> = calls
        .chunks(32)
        .map(|batch| Script::ToolReads {
            calls: batch.iter().map(|call| (*call, "t")).collect(),
        })
        .collect();
    // The structural refusal opens no physical attempt. The next provider call
    // is the separately supervised compaction operation; only then is the
    // rebuilt ordinary attempt allowed to open.
    scripts.push(Script::SuccessText(OBJECTIVE_SUMMARY));
    scripts.push(Script::Success);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    let session = json(address, "POST", "/api/session", "{}");
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_compact","text":"finish across the structural bound","delivery":"queue","resume":true}"#,
    );
    // Pace windows matching the provider's 32-call batches. A 60-second
    // progress allowance must not require eight responses to finish at once.
    for batch in calls.chunks(32) {
        super::super::compaction_continuity::approve_batch(address, id, batch);
    }
    permission_residuals::wait_session_idle(address, id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        FIRST_BATCHES + 2,
        "the structural refusal did not produce exactly one compaction call and one rebuilt attempt"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[FIRST_BATCHES]
            .contains("Output exactly the Markdown structure shown inside <template>"),
        "the boundary skipped the ordinary summary provider operation"
    );
    let rebuilt = &bodies[FIRST_BATCHES + 1];
    let exact_tail = calls
        .last()
        .copied()
        .unwrap_or_else(|| panic!("fixture call list is empty"));
    assert!(
        rebuilt.contains("Model-written continuation summary"),
        "the rebuilt request did not label its fallible summary"
    );
    assert!(
        rebuilt.contains(exact_tail) && rebuilt.contains("exact retained fact"),
        "the rebuilt request lost the newest exact working tail: {rebuilt}"
    );
    assert!(
        rebuilt.contains("finish across the structural bound"),
        "the rebuilt request lost the authoritative current input"
    );
    child.stop_after_witness(&root);
    drop(child);
    // The working tail can retain several complete response groups. Count its
    // actual calls so history, new reads and the final failure cross exactly
    // the second 1,281-call window instead of compacting mid-script.
    let session_id = id
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("created Session identity is invalid: {error}"));
    let retained_calls = retained_call_count(&root, &session_id);
    let second_count = STRUCTURAL_CALLS - retained_calls - 2;
    let second_calls: Vec<&'static str> = (1..=second_count)
        .map(|ordinal| &*Box::leak(format!("call_c_again_{ordinal:03}").into_boxed_str()))
        .collect();
    let second_batches = second_calls.len().div_ceil(32);
    let mut scripts = vec![Script::ToolHistory {
        call: "call_history_recover",
        target: "call_c_001",
    }];
    scripts.extend(second_calls.chunks(32).map(|batch| Script::ToolReads {
        calls: batch.iter().map(|call| (*call, "t")).collect(),
    }));
    scripts.push(Script::ToolRead {
        call: "call_c_unresolved",
        path: "missing.txt",
    });
    scripts.push(Script::SuccessText(
        "## Objective\n- Recovered exact fact: exact retained fact. The missing read remains unresolved.",
    ));
    scripts.push(Script::Success);
    provider.append(scripts);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_recover","text":"recover the omitted fact and continue","delivery":"queue","resume":true}"#,
    );
    let recovery_calls: Vec<&str> = std::iter::once("call_history_recover")
        .chain(second_calls.iter().copied())
        .collect();
    for batch in recovery_calls.chunks(32) {
        super::super::compaction_continuity::approve_batch(address, id, batch);
    }
    permission_residuals::wait_session_idle(address, id);
    assert_eq!(
        provider.calls.load(Ordering::SeqCst),
        FIRST_BATCHES + second_batches + 6,
        "the restart journey did not execute history, a second compaction and its rebuilt attempt"
    );
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[FIRST_BATCHES + 3].contains("exact retained fact"),
        "the actual history result did not recover the exact omitted fact: {}",
        bodies[FIRST_BATCHES + 3]
    );
    let second_summary = FIRST_BATCHES + second_batches + 4;
    assert!(
        bodies[second_summary]
            .contains("Output exactly the Markdown structure shown inside <template>"),
        "the second structural bound skipped the runtime compactor"
    );
    let second_rebuilt = &bodies[second_summary + 1];
    assert!(
        second_rebuilt.contains("Model-written continuation summary")
            && second_rebuilt.contains("call_c_unresolved")
            && second_rebuilt.contains("Tool failed: EffectFailed")
            && second_rebuilt.contains("nearest existing parent")
            && second_rebuilt.contains("missing.txt")
            && second_rebuilt.contains("exact retained fact")
            && second_rebuilt.contains("recover the omitted fact and continue"),
        "the second rebuilt request lost summary, unresolved failure, recovered fact or current input: {second_rebuilt}"
    );
    child.stop_after_witness(&root);
    drop(child);
    let store = EventStore::open(&root.join("database.sqlite3"), "swr-j03-diagnosis")
        .unwrap_or_else(|error| panic!("compaction evidence store is unreadable: {error}"));
    let session = id
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("created Session identity is invalid: {error}"));
    let (_, _, descriptor) =
        grapt_store::compaction::latest_descriptor(store.connection(), &session)
            .unwrap_or_else(|error| panic!("compaction descriptor is unreadable: {error}"))
            .unwrap_or_else(|| panic!("ordinary structural journey committed no descriptor"));
    assert_eq!(
        descriptor.retention_policy().as_str(),
        "scc-original-users-v4"
    );
    let selection = selection_artifact(&root, &descriptor);
    assert_eq!(selection["admission"]["trigger"], "structuralBound");
    assert_eq!(selection["admission"]["estimateBasis"], "structural-window");
    assert_eq!(selection["admission"]["estimate"], 1281);
    assert_eq!(selection["admission"]["budget"], 1280);
    assert!(selection["admission"]["removedBytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    assert!(selection["admission"]["retainedBytes"]
        .as_u64()
        .is_some_and(|bytes| bytes > 0));
    let compactions: i64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM event WHERE aggregate_family = 'session'
             AND aggregate_id = ?1 AND type = 'session.compaction.ended.2'",
            [session.as_str()],
            |row| row.get(0),
        )
        .unwrap_or_else(|error| panic!("compaction count is unreadable: {error}"));
    assert_eq!(
        compactions, 2,
        "the joined journey did not commit two compactions"
    );
    drop(store);
    process_fixture::cleanup(&root);
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn swr_j03_promoted_and_event_bounds_each_run_the_actual_compactor() {
    let _journey = process_journey_custody();
    const PROMOTED_INPUTS: usize = 1281;
    const EARLY_EXECUTION_PAIRS: usize = 500;
    const LATE_EXECUTION_PAIRS: usize = 24_501;
    let root = root("swr-promoted-event-compaction");
    prepare(&root, 700_000);
    prepare_model_inventory(&root, STRUCTURAL_CONTEXT_TOKENS);
    let mut scripts = vec![Script::Success; PROMOTED_INPUTS - 1];
    // The promoted bound's summary and its rebuilt attempt, the event-cut
    // input, then the event bound's summary and its rebuilt attempt.
    scripts.extend([
        Script::SuccessText(OBJECTIVE_SUMMARY),
        Script::Success,
        Script::Success,
        Script::SuccessText(OBJECTIVE_SUMMARY),
        Script::Success,
    ]);
    let provider = ProviderFixture::start(scripts);
    let mut child = spawn(&root, provider.address);
    let mut address = await_ready(&root, &mut child);
    // Fifty thousand execution facts need compact, valid identities to test
    // the row boundary independently of the unchanged 4 MiB data boundary.
    let session = json(address, "POST", "/api/session", r#"{"id":"ses_swr_rows"}"#);
    let id = session["data"]["id"]
        .as_str()
        .unwrap_or_else(|| panic!("missing Session id"));
    assert_eq!(id, "ses_swr_rows");
    // A queued prompt acknowledges input admission. An idle gap before its
    // wake starts does not prove that this particular input has executed.
    let wait_for_completion = |address, expected| {
        let called = permission_residuals::wait_until(
            client_allowance::CLI_REQUEST_ALLOWANCE,
            permission_residuals::SETTLE_POLL,
            || provider.calls.load(Ordering::SeqCst) >= expected,
        );
        assert!(called.is_some(), "provider call {expected} did not start");
        permission_residuals::wait_session_idle(address, id);
        assert_eq!(provider.calls.load(Ordering::SeqCst), expected);
    };
    for index in 0..PROMOTED_INPUTS - 1 {
        // Persisted admission counts must survive restarts. Keep each native
        // process within its unchanged serve window on a busy workstation.
        if index > 0 && index % 256 == 0 {
            child.stop_after_witness(&root);
            drop(child);
            child = spawn(&root, provider.address);
            address = await_ready(&root, &mut child);
        }
        json(
            address,
            "POST",
            &format!("/api/session/{id}/prompt"),
            &format!(
                r#"{{"id":"msg_swr_user_{index:03}","text":"promoted input {index}","delivery":"queue","resume":true}}"#
            ),
        );
        wait_for_completion(address, index + 1);
    }
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_user_1280","text":"promoted input 1280","delivery":"queue","resume":true}"#,
    );
    wait_for_completion(address, PROMOTED_INPUTS + 1);
    assert_eq!(provider.calls.load(Ordering::SeqCst), PROMOTED_INPUTS + 1);
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[PROMOTED_INPUTS - 1]
            .contains("Output exactly the Markdown structure shown inside <template>"),
        "1,281 promoted inputs skipped the actual compactor"
    );
    assert!(
        bodies[PROMOTED_INPUTS].contains("Model-written continuation summary")
            && bodies[PROMOTED_INPUTS].contains("promoted input 1280"),
        "the promoted-input rebuild lost its labelled summary or current authority"
    );
    child.stop_after_witness(&root);
    drop(child);

    let session = id
        .parse::<SessionId>()
        .unwrap_or_else(|error| panic!("created Session identity is invalid: {error}"));
    append_execution_pairs(&root, &session, 0, EARLY_EXECUTION_PAIRS);

    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_event_cut","text":"establish the safe event-window cut","delivery":"queue","resume":true}"#,
    );
    wait_for_completion(address, PROMOTED_INPUTS + 2);
    assert_eq!(provider.calls.load(Ordering::SeqCst), PROMOTED_INPUTS + 2);
    child.stop_after_witness(&root);
    drop(child);

    append_execution_pairs(&root, &session, EARLY_EXECUTION_PAIRS, LATE_EXECUTION_PAIRS);
    let mut child = spawn(&root, provider.address);
    let address = await_ready(&root, &mut child);
    json(
        address,
        "POST",
        &format!("/api/session/{id}/prompt"),
        r#"{"id":"msg_swr_event_bound","text":"continue after 50002 valid suffix events","delivery":"queue","resume":true}"#,
    );
    wait_for_completion(address, PROMOTED_INPUTS + 4);
    assert_eq!(provider.calls.load(Ordering::SeqCst), PROMOTED_INPUTS + 4);
    let bodies = locked(&provider.bodies).clone();
    assert!(
        bodies[PROMOTED_INPUTS + 2]
            .contains("Output exactly the Markdown structure shown inside <template>"),
        "the 50,001-event boundary skipped the actual compactor"
    );
    assert!(
        bodies[PROMOTED_INPUTS + 3].contains("Model-written continuation summary")
            && bodies[PROMOTED_INPUTS + 3].contains("continue after 50002 valid suffix events"),
        "the event-bound rebuild lost its labelled summary or current authority"
    );
    child.stop_after_witness(&root);
    drop(child);
    process_fixture::cleanup(&root);
}
