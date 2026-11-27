//! A history page renders only its selected rows (the
//! runtime audit): every poll of a long Session rendered its whole history
//! under the inbox lock before the limit applied. Pages keep the order,
//! cursors and next-page semantics they had: each is a slice of the full
//! ascending history, checked here against one full page.

use super::*;
use crate::dogfood::history::MessagePage;
use crate::dogfood::history_json::RENDERED_ROWS;
use serde_json::json;

#[path = "history_read_isolation.rs"]
mod read_isolation;

#[path = "session_list_cursor.rs"]
mod session_list_cursor;

/// Assistant Steps after the prompt.
const STEPS: u32 = 30;

/// A real application over a Session of one prompt and [`STEPS`] assistant
/// text Steps.
fn long_session(
    label: &str,
) -> Result<
    (
        Fixture,
        Arc<crate::dogfood::DogfoodApplication>,
        crate::dogfood::prompt_driver::PromptDriver,
    ),
    String,
> {
    let mut fixture = Fixture::new(label)?;
    fixture.close();
    let mut store =
        EventStore::open(&fixture.database, "history-page").map_err(|error| error.to_string())?;
    let ids = CountingIds(AtomicU64::new(0x7f00_0000));
    let authorities = grapt_types::DeterministicAuthorities::new(&FixedClock, &ids);
    let seed = grapt_session::SessionSeed::new(
        ProjectId::new("prj_history_page").ok_or("project")?,
        "/work/history-page",
        "history-page",
    )
    .map_err(|error| error.to_string())?;
    let mut listed = grapt_session::SessionListProjector;
    fixture.session =
        grapt_session::create_session(&mut store, &authorities, &seed, &mut [&mut listed])
            .map_err(|error| error.to_string())?
            .session;
    store
        .elect_writer(&AggregateId::Session(fixture.session.clone()))
        .map_err(|error| error.to_string())?;
    let target = crate::dogfood::production::tests::projection_target()?;
    let (app, driver) = crate::dogfood::test_provider::open(
        &fixture._scratch.0,
        store,
        &target,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )
    .map_err(|error| error.to_string())?;
    promote_test_input(&app.inbox, &fixture.session, &fixture.input, "use the tool")?;
    for step in 1..=STEPS {
        text_step(&app, &fixture, step)?;
    }
    // Admit this Session before tests inject page-local corruption.
    crate::dogfood::context::session_record(&app, &fixture.session)
        .map_err(|error| format!("{error:?}"))?;
    Ok((fixture, app, driver))
}

/// Commit one assistant text Step, `answer {step}`, to the application's
/// Session.
fn text_step(
    app: &crate::dogfood::DogfoodApplication,
    fixture: &Fixture,
    step: u32,
) -> Result<(), String> {
    let artifacts = FilesystemArtifactStore::open(
        fixture._scratch.0.join("dogfood-artifacts"),
        crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
        Box::new(FixedClock),
    )
    .map_err(|error| error.to_string())?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(&app.inbox),
        artifacts,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(1000 * u64::from(step)))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(step).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new(format!("pat_page_{step}")).ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    let block = grapt_provider::BlockOrdinal::new(0);
    for fragment in [
        AttemptFragment::TextOpened {
            block,
            state: AdapterState::none(),
        },
        AttemptFragment::TextDelta {
            block,
            text: format!("answer {step}"),
            state: AdapterState::none(),
        },
        AttemptFragment::TextClosed {
            block,
            state: AdapterState::none(),
        },
    ] {
        owner
            .commit(DurableFact::Fragment {
                fragment: Box::new(fragment),
            })
            .map_err(|error| error.to_string())?;
    }
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::Stop,
        })
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// One page of the Session's history, and how many rows the read rendered.
fn page(
    app: &crate::dogfood::DogfoodApplication,
    session: &SessionId,
    query: serde_json::Value,
) -> Result<(serde_json::Value, usize), String> {
    let query = crate::dogfood::decode(query)?;
    let page = MessagePage::parse(query).map_err(|error| format!("{error:?}"))?;
    RENDERED_ROWS.with(|rows| rows.set(0));
    let raw = app
        .history_json(session, &page, grapt_api::u3a::MessageListView::Durable)
        .map_err(|error| format!("{error:?}"))?;
    let rendered = RENDERED_ROWS.with(std::cell::Cell::get);
    let value = serde_json::from_str(&raw).map_err(|error| error.to_string())?;
    Ok((value, rendered))
}

fn ids(page: &serde_json::Value) -> Vec<String> {
    page["data"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row["id"].as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

#[test]
fn a_one_row_page_of_a_long_history_renders_only_its_row() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-page-cost")?;
    let (full, _) = page(&app, &fixture.session, json!({"limit":"200","order":"asc"}))?;
    let all = ids(&full);
    assert!(
        all.len() > STEPS as usize,
        "the history holds every Step: {all:?}"
    );

    let (newest, rendered) = page(&app, &fixture.session, json!({"limit":"1"}))?;
    assert_eq!(ids(&newest), all[all.len() - 1..]);
    assert!(
        rendered <= 1,
        "a one-row page rendered {rendered} rows of a {}-row history",
        all.len()
    );

    // The next page continues past it, newest first, at the cost of its rows.
    let next = newest["cursor"]["next"].clone();
    let (older, rendered) = page(&app, &fixture.session, json!({"limit":"2","cursor":next}))?;
    let mut expected = all[all.len() - 3..all.len() - 1].to_vec();
    expected.reverse();
    assert_eq!(ids(&older), expected);
    assert!(rendered <= 2, "a two-row page rendered {rendered} rows");

    // And back again.
    let previous = older["cursor"]["previous"].clone();
    let (again, _) = page(
        &app,
        &fixture.session,
        json!({"limit":"1","cursor":previous}),
    )?;
    assert_eq!(ids(&again), ids(&newest));

    // Ascending pages are the same history read from its start.
    let (first, rendered) = page(&app, &fixture.session, json!({"limit":"3","order":"asc"}))?;
    assert_eq!(ids(&first), all[..3]);
    assert!(rendered <= 3, "a three-row page rendered {rendered} rows");
    let next = first["cursor"]["next"].clone();
    let (second, _) = page(&app, &fixture.session, json!({"limit":"3","cursor":next}))?;
    assert_eq!(ids(&second), all[3..6]);
    Ok(())
}

/// A row that cannot be rendered refuses only the pages
/// that include it, and names itself there.
#[test]
fn a_corrupt_row_refuses_only_its_pages_by_name() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-page-corrupt")?;
    let oldest = {
        let inbox = lock(&app.inbox);
        SessionHistoryStore::over(inbox.store())
            .fold(&fixture.session)
            .map_err(|error| error.to_string())?
            .messages()
            .map_err(|error| error.to_string())?
            .into_iter()
            .next()
            .ok_or("no assistant message")?
    };
    // The oldest answer loses its stored text.
    let content = oldest.content.as_ref().ok_or("content")?;
    let identity = content
        .items()
        .first()
        .ok_or("text item")?
        .item
        .payload()
        .id()
        .as_str()
        .to_owned();
    std::fs::remove_file(
        fixture
            ._scratch
            .0
            .join("dogfood-artifacts")
            .join("objects")
            .join(identity.get(..2).ok_or("shard")?)
            .join(&identity),
    )
    .map_err(|error| error.to_string())?;

    let (newest, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
    assert_eq!(ids(&newest).len(), 1, "a page without the row still reads");
    let refused = page(&app, &fixture.session, json!({"limit":"3","order":"asc"}))
        .err()
        .ok_or("a page holding the unreadable row was served")?;
    assert!(
        refused.contains(oldest.message.as_str()),
        "the refusal does not name the row: {refused}"
    );
    Ok(())
}

/// A compaction row's public `recent` needs only the
/// lineage at and before it. A later compaction that cannot be read refuses
/// only the pages that reach it, by name; earlier pages still read.
#[test]
fn a_later_unreadable_compaction_spares_earlier_pages() -> Result<(), String> {
    let (mut fixture, app, _driver) = long_session("history-page-lineage")?;
    // The fixture's continuation compacts the application's own Session.
    fixture.inbox = Some(Arc::clone(&app.inbox));
    fixture.artifacts = Some(Arc::new(
        FilesystemArtifactStore::open(
            fixture._scratch.0.join("dogfood-artifacts"),
            crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
            Box::new(FixedClock),
        )
        .map_err(|error| error.to_string())?,
    ));
    for round in 0..2u32 {
        if round > 0 {
            text_step(&app, &fixture, STEPS + round)?;
        }
        let id = grapt_types::CompactionId::new(format!("hp-lineage-{round}")).ok_or("id")?;
        let selected = fixture
            .continuation(fixture.input.clone())?
            .prepare_summary("use the tool", &id, 128_000)?;
        super::summary_tests::commit_selection(&fixture, &selected, id)?;
    }
    let (full, _) = page(&app, &fixture.session, json!({"limit":"200","order":"asc"}))?;
    let rows = full["data"].as_array().ok_or("page rows")?;
    let compactions: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, row)| row["type"] == "compaction")
        .map(|(index, _)| index)
        .collect();
    let [first, second] = compactions[..] else {
        return Err(format!("two compaction rows expected: {compactions:?}"));
    };
    let later = rows[second]["id"]
        .as_str()
        .ok_or("compaction id")?
        .to_owned();

    // The later compaction loses its retained selection.
    let selection: String = {
        let inbox = lock(&app.inbox);
        inbox
            .store()
            .connection()
            .query_row(
                "SELECT json_extract(data, '$.retained.descriptor.id') FROM event
                 WHERE type = 'session.compaction.ended.2' ORDER BY seq DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| error.to_string())?
    };
    std::fs::remove_file(
        fixture
            ._scratch
            .0
            .join("dogfood-artifacts")
            .join("objects")
            .join(selection.get(..2).ok_or("shard")?)
            .join(&selection),
    )
    .map_err(|error| error.to_string())?;

    let limit = (first + 1).to_string();
    let (earlier, _) = page(
        &app,
        &fixture.session,
        json!({"limit": limit, "order":"asc"}),
    )?;
    assert_eq!(
        ids(&earlier).last(),
        rows[first]["id"].as_str().map(str::to_owned).as_ref(),
        "the page ending at the earlier compaction still reads"
    );
    let limit = (second + 1).to_string();
    let refused = page(
        &app,
        &fixture.session,
        json!({"limit": limit, "order":"asc"}),
    )
    .err()
    .ok_or("a page holding the unreadable compaction was served")?;
    // Its descriptor event shares the row id's suffix.
    let event = later.strip_prefix("msg_").ok_or("compaction row id")?;
    assert!(
        refused.contains(&format!("evt_{event}")),
        "the refusal does not name the unreadable compaction {later}: {refused}"
    );
    Ok(())
}

/// A later tool's prefetched failure must not supersede an earlier row's own
/// artifact refusal. Both pages still name the first unreadable selected row.
#[test]
fn br_h_prefetched_tool_refusals_wait_for_their_selected_row() -> Result<(), String> {
    let (fixture, app, _driver) = long_session("history-batch-error-order")?;
    let artifacts = FilesystemArtifactStore::open(
        fixture._scratch.0.join("dogfood-artifacts"),
        crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
        Box::new(FixedClock),
    )
    .map_err(|error| error.to_string())?;
    let mut owner = DurableAttemptOwner::elect_inbox_owned(
        Arc::clone(&app.inbox),
        artifacts,
        Arc::new(FixedClock),
        Arc::new(CountingIds(AtomicU64::new(1000 * u64::from(STEPS + 1)))),
        NoLiveSubscribers,
        fixture.session.clone(),
        StepNumber::new(STEPS + 1).ok_or("step")?,
        None,
    )
    .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::AttemptStarted {
            attempt: PhysicalAttemptId::new("pat_batch_precedence").ok_or("attempt")?,
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::Fragment {
            fragment: Box::new(AttemptFragment::ToolCalled {
                call: ToolCallId::new("call_batch_precedence").ok_or("call")?,
                tool: READ_TOOL.to_owned(),
                input: "{}".to_owned(),
                execution: ToolExecution::Local,
                state: AdapterState::none(),
            }),
        })
        .map_err(|error| error.to_string())?;
    owner
        .commit(DurableFact::StepEnded {
            finish: FinishReason::ToolCalls,
        })
        .map_err(|error| error.to_string())?;
    drop(owner);
    let history = app.message_history(&fixture.session)?;
    let first = history.first().ok_or("first assistant")?;
    let last = history.last().ok_or("last assistant")?;
    for message in [first, last] {
        let identity = message
            .content
            .as_ref()
            .ok_or("content")?
            .items()
            .first()
            .ok_or("item")?
            .item
            .payload()
            .id()
            .as_str();
        std::fs::remove_file(
            fixture
                ._scratch
                .0
                .join("dogfood-artifacts")
                .join("objects")
                .join(identity.get(..2).ok_or("shard")?)
                .join(identity),
        )
        .map_err(|error| error.to_string())?;
    }
    let first_refusal = page(&app, &fixture.session, json!({"limit":"200","order":"asc"}))
        .err()
        .ok_or("unreadable first row was rendered")?;
    assert!(
        first_refusal.contains(first.message.as_str()),
        "a later prefetch overrode the first selected refusal: {first_refusal}"
    );
    let last_refusal = page(&app, &fixture.session, json!({"limit":"1"}))
        .err()
        .ok_or("unreadable last tool was rendered")?;
    assert!(
        last_refusal.contains(last.message.as_str()),
        "the selected tool refusal lost its row: {last_refusal}"
    );
    Ok(())
}

#[path = "history_page_long.rs"]
mod lifetime;

#[path = "history_omissions.rs"]
mod omissions;

#[test]
fn model_history_recovers_public_chunks_and_refuses_foreign_coordinates() -> Result<(), String> {
    use crate::dogfood::tools::read_history::{read, Request};
    let (fixture, app, _driver) = long_session("model-history")?;
    let first: serde_json::Value = serde_json::from_slice(&read(
        &app,
        &fixture.session,
        Request::List { cursor: None },
    )?)
    .map_err(|e| e.to_string())?;
    let cursor = first["next_cursor"]
        .as_str()
        .ok_or("next cursor")?
        .to_owned();
    let second: serde_json::Value = serde_json::from_slice(&read(
        &app,
        &fixture.session,
        Request::List {
            cursor: Some(cursor),
        },
    )?)
    .map_err(|e| e.to_string())?;
    assert_ne!(first["messages"][0]["id"], second["messages"][0]["id"]);
    let id: MessageId = "msg_history_unicode".parse().map_err(|_| "id")?;
    let original = "é\n\"".repeat(5000);
    promote_test_input(&app.inbox, &fixture.session, &id, &original)?;
    let mut offset = 0;
    let mut joined = String::new();
    loop {
        let bytes = read(
            &app,
            &fixture.session,
            Request::Message {
                id: id.clone(),
                offset,
            },
        )?;
        assert!(bytes.len() < 65536, "tool page exceeds composition capture");
        let page: serde_json::Value = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
        joined.push_str(page["text"].as_str().ok_or("chunk")?);
        match page["nextOffset"].as_u64() {
            Some(next) => offset = usize::try_from(next).map_err(|e| e.to_string())?,
            None => break,
        }
    }
    let recovered: serde_json::Value = serde_json::from_str(&joined).map_err(|e| e.to_string())?;
    assert_eq!(recovered["data"]["text"], original);
    let split = joined.find('é').ok_or("unicode")? + 1;
    assert!(read(
        &app,
        &fixture.session,
        Request::Message {
            id: id.clone(),
            offset: split
        }
    )
    .is_err());
    assert!(read(
        &app,
        &fixture.session,
        Request::Message {
            id,
            offset: usize::MAX
        }
    )
    .is_err());
    let foreign: SessionId = "ses_history_foreign".parse().map_err(|_| "foreign")?;
    let foreign_id: MessageId = "msg_history_foreign".parse().map_err(|_| "foreign")?;
    app.lock_inbox()
        .store_mut()
        .elect_writer(&AggregateId::Session(foreign.clone()))
        .map_err(|e| e.to_string())?;
    promote_test_input(&app.inbox, &foreign, &foreign_id, "private foreign history")?;
    assert!(read(
        &app,
        &fixture.session,
        Request::Message {
            id: foreign_id.clone(),
            offset: 0
        }
    )
    .is_err());
    use base64::Engine;
    let foreign_cursor = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(
        serde_json::json!({"id":foreign_id.as_str(),"order":"desc","direction":"next"}).to_string(),
    );
    assert!(read(
        &app,
        &fixture.session,
        Request::List {
            cursor: Some(foreign_cursor)
        }
    )
    .is_err());
    Ok(())
}

#[test]
fn compaction_history_exposes_only_supplied_admission_token_estimates() -> Result<(), String> {
    let (mut fixture, app, _driver) = long_session("history-compaction-estimates")?;
    fixture.inbox = Some(Arc::clone(&app.inbox));
    fixture.artifacts = Some(Arc::new(
        FilesystemArtifactStore::open(
            fixture._scratch.0.join("dogfood-artifacts"),
            crate::dogfood::PRODUCTION_ARTIFACT_KEY.to_vec(),
            Box::new(FixedClock),
        )
        .map_err(|error| error.to_string())?,
    ));
    for (round, (supplied, basis)) in [
        (None, "estimated"),
        (Some(Some(350)), "estimated"),
        (Some(None), "estimated"),
        (Some(None), "structural-window"),
    ]
    .into_iter()
    .enumerate()
    {
        if round > 0 {
            text_step(&app, &fixture, STEPS + round as u32)?;
        }
        let id = grapt_types::CompactionId::new(format!("history-estimate-{round}")).ok_or("id")?;
        let mut selected = fixture
            .continuation(fixture.input.clone())?
            .prepare_summary("use the tool", &id, 128_000)?;
        selected.selection.admission =
            supplied.map(|after| super::super::retained::AdmissionDiagnosis {
                trigger: grapt_types::CompactionTrigger::ContextOverflow,
                estimate_basis: basis,
                estimate: 1200,
                rebuilt_estimate: after,
                budget: 1000,
                removed_bytes: selected.removed_bytes,
                retained_bytes: selected.retained_bytes,
            });
        super::summary_tests::commit_selection(&fixture, &selected, id)?;
        let (page, _) = page(&app, &fixture.session, json!({"limit":"1"}))?;
        let row = page["data"]
            .as_array()
            .and_then(|rows| rows.first())
            .ok_or("compaction row")?;
        assert_eq!(row["type"], "compaction");
        let estimates = &row["metadata"]["tokenEstimates"];
        if let Some(after) = supplied.filter(|_| basis != "structural-window") {
            assert_eq!(estimates["before"], "1200");
            assert_eq!(estimates["basis"], "estimated");
            match after {
                Some(after) => assert_eq!(estimates["after"], after.to_string()),
                None => assert!(estimates.get("after").is_none()),
            }
        } else {
            assert!(
                estimates.is_null(),
                "legacy records cannot acquire invented totals"
            );
        }
    }
    Ok(())
}
