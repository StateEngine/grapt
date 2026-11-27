//! Lifetime transcript growth must not consume a single page's read allowance.
use super::*;
use serde_json::Value;

fn bind_artifacts(
    fixture: &mut Fixture,
    app: &Arc<crate::dogfood::DogfoodApplication>,
) -> Result<(), String> {
    fixture.inbox = Some(Arc::clone(&app.inbox));
    fixture.artifacts = Some(Arc::new(
        FilesystemArtifactStore::open(
            fixture._scratch.0.join("dogfood-artifacts"),
            crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
            Box::new(FixedClock),
        )
        .map_err(|error| error.to_string())?,
    ));
    Ok(())
}

fn compact(fixture: &Fixture, ordinal: u32) -> Result<(), String> {
    let id =
        grapt_types::CompactionId::new(format!("history-paging-{ordinal}")).ok_or("compaction")?;
    let prompt = lock(fixture.inbox()?)
        .promoted_input(&fixture.session, &fixture.input)
        .map_err(|error| error.to_string())?
        .text;
    let selected = fixture
        .continuation(fixture.input.clone())?
        .prepare_summary(&prompt, &id, 128_000)?;
    super::super::summary_tests::commit_selection(fixture, &selected, id)
}

fn traverse(
    app: &crate::dogfood::DogfoodApplication,
    session: &SessionId,
    order: &str,
) -> Result<Vec<Value>, String> {
    let mut query = json!({"limit":"7", "order":order});
    let mut rows = Vec::new();
    let mut tied_boundary = false;
    loop {
        let (response, _) = page(app, session, query)?;
        let batch = response["data"].as_array().ok_or("history page")?;
        if batch.is_empty() {
            break;
        }
        assert!(batch.len() <= 7);
        if let Some(last) = rows.last() {
            let last: &Value = last;
            tied_boundary |= last["time"]["created"] == batch[0]["time"]["created"];
        }
        rows.extend(batch.iter().cloned());
        assert!(rows.len() <= 1000, "paging repeated or failed to advance");
        query = json!({"limit":"7", "cursor":response["cursor"]["next"]});
    }
    assert!(
        tied_boundary,
        "fixture did not cross rows sharing created_ms"
    );
    Ok(rows)
}

#[test]
#[expect(clippy::too_many_lines, reason = "legacy")]
fn more_than_128_inputs_failures_and_notices_page_end_to_end_with_compactions() -> Result<(), String>
{
    let (mut fixture, app, _driver) = long_session("history-lifetime-pages")?;
    bind_artifacts(&mut fixture, &app)?;
    for ordinal in 1..=150u32 {
        let input: MessageId = format!("msg_history_long_{ordinal}")
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(
            &app.inbox,
            &fixture.session,
            &input,
            &format!("history input {ordinal}"),
        )?;
        fixture.input = input;
        text_step(&app, &fixture, STEPS + ordinal)?;
        let execution = grapt_types::ExecutionId::new(format!("exe_history_long_{ordinal}"))
            .ok_or("execution")?;
        let authorities = grapt_types::DeterministicAuthorities::new(&*app.clock, &*app.ids);
        {
            let mut held = lock(&app.inbox);
            grapt_session::execution::begin(
                held.store_mut(),
                &authorities,
                &fixture.session,
                &execution,
            )?;
            grapt_session::execution::settle_failed(
                held.store_mut(),
                &authorities,
                &fixture.session,
                &execution,
                grapt_session::execution::Failure::Generic,
            )?;
            grapt_session::notice::record_context_repeated(
                held.store_mut(),
                &authorities,
                &fixture.session,
                &execution,
            )?;
        }
        if ordinal % 30 == 0 {
            compact(&fixture, ordinal)?;
        }
    }
    let ascending = traverse(&app, &fixture.session, "asc")?;
    assert_eq!(
        ascending.iter().filter(|row| row["type"] == "user").count(),
        151
    );
    assert_eq!(
        ascending
            .iter()
            .filter(|row| row["metadata"]["executionFailure"] == true)
            .count(),
        150
    );
    assert_eq!(
        ascending
            .iter()
            .filter(|row| row["metadata"]["contextRepeated"] == true)
            .count(),
        150
    );
    assert_eq!(
        ascending
            .iter()
            .filter(|row| row["type"] == "compaction")
            .count(),
        5
    );
    let mut descending = traverse(&app, &fixture.session, "desc")?;
    descending.reverse();
    assert_eq!(
        descending, ascending,
        "direction or keyset boundaries lost/duplicated public rows"
    );
    let one = app
        .message_json(&fixture.session, &fixture.input)?
        .ok_or("single-message lookup failed after 128 inputs")?;
    assert!(one.contains("history input 150"));
    let before: MessageId = "msg_history_long_140"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    let authorities = grapt_types::DeterministicAuthorities::new(&*app.clock, &*app.ids);
    {
        let mut held = lock(&app.inbox);
        let selected = grapt_session::rewind::select(held.store(), &fixture.session, &before)
            .map_err(|error| error.to_string())?;
        grapt_session::rewind::commit(
            held.store_mut(),
            &authorities,
            &selected,
            &mut [&mut grapt_inbox::InboxProjector],
        )
        .map_err(|error| error.to_string())?;
    }
    let visible = traverse(&app, &fixture.session, "asc")?;
    assert_eq!(
        visible.iter().filter(|row| row["type"] == "user").count(),
        140
    );
    assert_eq!(
        visible
            .iter()
            .filter(|row| row["metadata"]["executionFailure"] == true)
            .count(),
        139
    );
    assert_eq!(
        visible
            .iter()
            .filter(|row| row["type"] == "compaction")
            .count(),
        4
    );
    assert!(
        app.message_json(&fixture.session, &fixture.input)?
            .is_none(),
        "rewind resurrected a hidden input in single-message lookup"
    );
    Ok(())
}

#[test]
fn a_selected_compaction_reads_past_128_prior_descriptors_in_bounded_batches() -> Result<(), String>
{
    let (mut fixture, app, _driver) = long_session("history-compaction-pages")?;
    bind_artifacts(&mut fixture, &app)?;
    for ordinal in 1..=130u32 {
        text_step(&app, &fixture, STEPS + ordinal)?;
        compact(&fixture, ordinal)?;
    }
    let (last, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
    assert_eq!(
        last["data"][0]["metadata"]["compactionID"],
        "history-paging-130"
    );
    let mut query = json!({"limit":"11", "order":"asc"});
    let mut compactions = 0;
    loop {
        let (response, _) = page(&app, &fixture.session, query)?;
        let batch = response["data"].as_array().ok_or("history page")?;
        if batch.is_empty() {
            break;
        }
        compactions += batch
            .iter()
            .filter(|row| row["type"] == "compaction")
            .count();
        assert!(compactions <= 130);
        query = json!({"limit":"11", "cursor":response["cursor"]["next"]});
    }
    assert_eq!(compactions, 130);
    Ok(())
}

#[test]
fn duplicate_sequences_on_the_next_page_boundary_refuse_instead_of_disappearing(
) -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-duplicate-boundary")?;
    let (before, _) = page(&app, &fixture.session, json!({"limit":"1","order":"asc"}))?;
    let cursor = before["cursor"]["next"].clone();
    let input: MessageId = "msg_history_duplicate"
        .parse()
        .map_err(|error: grapt_types::IdError| error.to_string())?;
    promote_test_input(
        &app.inbox,
        &fixture.session,
        &input,
        "duplicate coordinates",
    )?;
    {
        let raw =
            rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
        let changed = raw.execute(
            "UPDATE session_message SET seq=(SELECT seq FROM session_message WHERE id=?1) WHERE id=?2",
            (fixture.input.as_str(),input.as_str()),
        ).map_err(|error|error.to_string())?;
        assert_eq!(changed, 1, "duplicate fixture did not change one input");
    }
    for query in [
        json!({"limit":"1","order":"asc"}),
        json!({"limit":"1","cursor":cursor}),
    ] {
        let refused = page(&app, &fixture.session, query)
            .err()
            .ok_or("duplicate boundary was served")?;
        assert!(refused.contains("same durable sequence"), "{refused}");
    }
    Ok(())
}

#[test]
fn malformed_old_message_facts_are_named_only_on_pages_that_select_the_message(
) -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-old-fact")?;
    {
        let raw =
            rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
        let changed = raw.execute(
            "UPDATE event SET data='{' WHERE id=(SELECT id FROM event WHERE aggregate_family='session' AND aggregate_id=?1 AND type='session.content.appended.1' ORDER BY seq LIMIT 1)",
            [fixture.session.as_str()],
        ).map_err(|error|error.to_string())?;
        assert_eq!(changed, 1, "corruption fixture did not change one old fact");
    }
    let (newest, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
    assert_eq!(ids(&newest).len(), 1);
    assert!(
        !names_a_gap(&newest),
        "a page that does not select the gap carried its marker: {newest}"
    );
    // The page that spans the unreadable row is served, and the row that owns
    // the gap names it as data, instead of the page being refused whole.
    let (spanned, _) = page(&app, &fixture.session, json!({"limit":"2","order":"asc"}))?;
    assert!(
        names_a_gap(&spanned),
        "the page spanning an unreadable row carried no marker: {spanned}"
    );
    Ok(())
}

/// Whether any row on this page reports an omitted durable item.
fn names_a_gap(response: &serde_json::Value) -> bool {
    response["data"].as_array().is_some_and(|rows| {
        rows.iter().any(|row| {
            row["metadata"]["omittedTotal"]
                .as_u64()
                .is_some_and(|total| total >= 1)
        })
    })
}

#[test]
fn malformed_or_oversized_old_notices_spare_new_pages_and_keep_unknown_diagnostics(
) -> Result<(), String> {
    let oversized = json!({
        "kind":"future-too-large", "padding":"x".repeat(grapt_session::notice::MAX_NOTICE_DATA_BYTES)
    })
    .to_string();
    for bad_data in ["{", oversized.as_str()] {
        let (fixture, app, _driver) = long_session("history-old-notice")?;
        let authorities = grapt_types::DeterministicAuthorities::new(&*app.clock, &*app.ids);
        let execution = grapt_types::ExecutionId::new("exe_notice_page").ok_or("execution")?;
        let (bad, unknown) = {
            let mut held = lock(&app.inbox);
            let mut record = || {
                grapt_session::notice::record_context_repeated(
                    held.store_mut(),
                    &authorities,
                    &fixture.session,
                    &execution,
                )
            };
            (record()?.position.event, record()?.position.event)
        };
        let newest: MessageId = "msg_after_bad_notice"
            .parse()
            .map_err(|error: grapt_types::IdError| error.to_string())?;
        promote_test_input(&app.inbox, &fixture.session, &newest, "latest valid input")?;
        let raw =
            rusqlite::Connection::open(&fixture.database).map_err(|error| error.to_string())?;
        for (event, data) in [
            (&bad, bad_data),
            (&unknown, "{\"kind\":\"future-page-kind\"}"),
        ] {
            let changed = raw
                .execute(
                    "UPDATE event SET data=?2 WHERE id=?1",
                    (event.as_str(), data),
                )
                .map_err(|error| error.to_string())?;
            assert_eq!(changed, 1, "notice fixture did not change one event");
        }
        let (latest, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
        assert_eq!(ids(&latest), vec![newest.as_str()]);
        let skipped = {
            let held = lock(&app.inbox);
            grapt_session::notice::skipped_kinds(held.store().connection(), &fixture.session)?
        };
        assert_eq!(
            skipped,
            vec![grapt_session::notice::SkippedKind {
                kind: "future-page-kind".into(),
                count: 1,
            }]
        );
        let refused = page(
            &app,
            &fixture.session,
            json!({"limit":"1", "cursor":latest["cursor"]["next"]}),
        )
        .err()
        .ok_or("page selecting the bad notice was served")?;
        let bad_message = format!(
            "msg_{}",
            bad.as_str().strip_prefix("evt_").ok_or("notice id")?
        );
        assert!(refused.contains(&bad_message), "{refused}");
    }
    Ok(())
}

#[test]
fn api01_a_page_replays_each_compaction_lineage_step_once() -> Result<(), String> {
    let (mut fixture, app, _driver) = long_session("api01-lineage-work")?;
    bind_artifacts(&mut fixture, &app)?;
    for ordinal in 1..=12u32 {
        text_step(&app, &fixture, STEPS + ordinal)?;
        compact(&fixture, ordinal)?;
    }
    for order in ["asc", "desc"] {
        crate::dogfood::continuation::recent::CHAIN_STEPS.with(|steps| steps.set(0));
        let (response, _) = page(
            &app,
            &fixture.session,
            json!({"limit":"200", "order": order}),
        )?;
        assert_eq!(
            response["data"]
                .as_array()
                .ok_or("page")?
                .iter()
                .filter(|row| row["type"] == "compaction")
                .count(),
            12
        );
        assert_eq!(
            crate::dogfood::continuation::recent::CHAIN_STEPS.with(std::cell::Cell::get),
            12,
            "API01_LINEAGE_WORK: {order} page repeatedly rebuilt its compaction prefix"
        );
    }
    Ok(())
}
