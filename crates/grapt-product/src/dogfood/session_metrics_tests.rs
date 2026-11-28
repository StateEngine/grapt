//! SC-HIST-01: a Session row exists whatever its lifetime metrics read says.
use super::*;
use grapt_types::*;
use std::{path::PathBuf, sync::Arc};

type TestResult = Result<(), Box<dyn std::error::Error>>;

fn target() -> Result<ResolvedModelTarget, String> {
    let provider = ProviderId::new("fixture").ok_or("provider")?;
    let model = ModelId::new("fixture").ok_or("model")?;
    let catalog = ModelCatalog::load(
        ModelCatalogVersion::new("fixture").ok_or("catalog version")?,
        vec![ModelCatalogRecord::new(
            provider.clone(),
            model.clone(),
            BaseModelId::new("fixture").ok_or("base")?,
            ModelFamilyId::new("fixture").ok_or("family")?,
            None,
        )],
    )
    .ok_or("catalog")?;
    ModelRegistry::install(catalog)
        .issue(
            &provider,
            &model,
            None,
            RoutingRequest {
                route: ModelRouteId::new("fixture").ok_or("route")?,
                account: AccountId::new("fixture").ok_or("account")?,
                credential_version: CredentialVersion::new("fixture")
                    .ok_or("credential version")?,
                policy: PolicyVersion::new("fixture").ok_or("policy")?,
            },
        )
        .map_err(|error| format!("target: {error:?}"))
}

struct Root(PathBuf);
impl Root {
    fn new() -> Result<Self, Box<dyn std::error::Error>> {
        let ids = grapt_store::OsRandomIds::seed()?;
        let root = std::env::temp_dir().join(format!("session-metrics-{}", ids.mint().as_str()));
        std::fs::create_dir(&root)?;
        let root = Self(root);
        let initialized = std::process::Command::new("git")
            .env("GIT_CEILING_DIRECTORIES", std::env::temp_dir())
            .args(["init", "--quiet"])
            .arg(&root.0)
            .status()?;
        if !initialized.success() {
            return Err("session metrics fixture Git boundary could not be initialized".into());
        }
        Ok(root)
    }
}
impl Drop for Root {
    fn drop(&mut self) {
        // This fresh minted fixture root owns its Git metadata and artifacts.
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn listed(app: &ServerApplication) -> Result<Value, Box<dyn std::error::Error>> {
    let query: SessionListQueryDto = decode(json!({}))?;
    let page = list::response(app, query).map_err(|error| format!("list refused: {error:?}"))?;
    Ok(serde_json::to_value(page)?)
}

fn got(app: &ServerApplication, session: &SessionId) -> Result<Value, Box<dyn std::error::Error>> {
    let record = app.get_record(session)?;
    let response = app
        .session_response(&record)
        .map_err(|error| format!("get refused: {:?}", serde_json::to_value(error)))?;
    Ok(serde_json::to_value(response)?["data"].clone())
}

#[test]
fn a_committed_manual_title_change_notifies_idle_catalog_readers_once() -> TestResult {
    let root = Root::new()?;
    let (app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        grapt_store::EventStore::open_in_memory()?,
        &target()?,
        crate::dogfood::event_hub()?,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
    let seed = SessionSeed::new(
        app.catalog_snapshot().project.clone(),
        root.0.to_str().ok_or("directory")?.to_owned(),
        "Before",
    )?;
    let owner = SessionPorts::new(Arc::clone(&app.inbox), &authorities)
        .create(&seed)?
        .session;
    let feed = app.hub.subscribe_host()?;
    for title in ["Renamed elsewhere", "Renamed elsewhere"] {
        session_title::manual(&app, owner.clone(), decode(json!({"title":title}))?)
            .map_err(|error| format!("rename: {error:?}"))?;
    }
    assert!(session_title::manual(
        &app,
        owner.clone(),
        decode(json!({"title":"x".repeat(101)}))?
    )
    .is_err());
    feed.cancel();
    let mut notices = Vec::new();
    let mut frames = 0;
    while let Some(frame) = feed.next_frame()? {
        frames += 1;
        assert!(frames <= 4, "unexpected title notification frames");
        let text = std::str::from_utf8(&frame)?;
        let data = text
            .lines()
            .find_map(|line| line.strip_prefix("data:"))
            .ok_or("SSE data")?;
        let value: Value = serde_json::from_str(data.trim())?;
        if value["type"] == "session.title.changed" {
            assert!(
                value.get("durable").is_none(),
                "refresh hints must not claim a durable cursor"
            );
            notices.push(value["data"]["sessionID"].clone());
        }
    }
    assert_eq!(
        notices,
        vec![json!(owner.as_str())],
        "committed title change did not notify the idle catalog"
    );
    assert_eq!(got(&app, &owner)?["title"], "Renamed elsewhere");
    Ok(())
}

#[tokio::test]
async fn a_session_past_its_metrics_bound_still_lists_and_reopens_with_unknown_totals() -> TestResult
{
    for persistent in [false, true] {
        check_list_and_detail_metrics(persistent)?;
    }
    Ok(())
}

fn check_list_and_detail_metrics(persistent: bool) -> TestResult {
    let root = Root::new()?;
    let store = if persistent {
        grapt_store::EventStore::open(&root.0.join("supplied.sqlite3"), "fixture")?
    } else {
        grapt_store::EventStore::open_in_memory()?
    };
    let hub = crate::dogfood::event_hub()?;
    let (mut app, _driver) = crate::dogfood::test_provider::open(
        &root.0,
        store,
        &target()?,
        hub,
        crate::dogfood::plugins::PluginRuntime::empty(),
    )?;
    let directory = root.0.to_str().ok_or("fixture path encoding")?;
    let mut sessions = Vec::new();
    for title in ["long", "short"] {
        let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
        let seed = SessionSeed::new(
            app.catalog_snapshot().project.clone(),
            directory.to_owned(),
            title,
        )?;
        let created = SessionPorts::new(Arc::clone(&app.inbox), &authorities).create(&seed)?;
        sessions.push(created.session);
    }

    // List rows omit lifetime accounting even when detail totals are known.
    assert!(got(&app, &sessions[0])?["tokens"].is_object());
    for row in listed(&app)?["data"].as_array().ok_or("list data")? {
        assert!(
            row.get("tokens").is_none(),
            "a list row stated lifetime totals: {row}"
        );
        assert!(
            row.get("metrics").is_none(),
            "an in-bound row carried a state: {row}"
        );
    }

    // One row is already past a zero-row bound; every Session is.
    let bound = MetricsBound {
        rows: 0,
        bytes: MetricsBound::PRODUCTION.bytes,
    };
    Arc::make_mut(&mut app).metrics_bound = bound;
    let page = listed(&app)?;
    let rows = page["data"].as_array().ok_or("list data")?;
    assert_eq!(
        rows.len(),
        sessions.len(),
        "a Session past the bound left the list"
    );
    for row in rows {
        assert!(
            row.get("tokens").is_none(),
            "an unknown total was stated: {row}"
        );
        assert!(row.get("metrics").is_none());
        assert!(row["time"]["updated"].as_f64() >= row["time"]["created"].as_f64());
    }
    let reopened = got(&app, &sessions[0])?;
    assert_eq!(reopened["id"], sessions[0].as_str());
    assert!(reopened.get("tokens").is_none());
    assert_eq!(reopened["metrics"]["state"], "exceeds-bound");

    // The byte bound degrades the same way: every stored row is past zero.
    let bound = MetricsBound {
        rows: MetricsBound::PRODUCTION.rows,
        bytes: 0,
    };
    Arc::make_mut(&mut app).metrics_bound = bound;
    let page = listed(&app)?;
    let rows = page["data"].as_array().ok_or("list data")?;
    assert_eq!(
        rows.len(),
        sessions.len(),
        "a Session past the byte bound left the list"
    );
    for row in rows {
        assert!(
            row.get("tokens").is_none(),
            "an unknown total was stated: {row}"
        );
        assert!(row.get("metrics").is_none());
    }
    assert_eq!(
        got(&app, &sessions[0])?["metrics"]["state"],
        "exceeds-bound"
    );
    Ok(())
}

#[test]
fn review_public_session_does_not_invent_reasoning_from_an_inclusive_stamp() -> TestResult {
    for (stamped, reasoning_present) in [(false, false), (true, true), (true, false)] {
        let root = Root::new()?;
        let (app, _driver) = crate::dogfood::test_provider::open(
            &root.0,
            grapt_store::EventStore::open_in_memory()?,
            &target()?,
            crate::dogfood::event_hub()?,
            crate::dogfood::plugins::PluginRuntime::empty(),
        )?;
        let authorities = DeterministicAuthorities::new(&*app.clock, &*app.ids);
        let seed = SessionSeed::new(
            app.catalog_snapshot().project.clone(),
            root.0.to_str().ok_or("directory")?.to_owned(),
            "Accounting",
        )?;
        let session = SessionPorts::new(Arc::clone(&app.inbox), &authorities)
            .create(&seed)?
            .session;
        let field = |n| TokenCount::new(n).map(UsageField::reported);
        let reasoning = if reasoning_present {
            field(0)?
        } else {
            UsageField::ABSENT
        };
        let mut usage = AttemptUsage::observed(
            field(11)?,
            field(8)?,
            field(4)?,
            reasoning,
            field(2)?,
            field(1)?,
            field(15)?,
            UsageObservationPhase::Terminal,
        );
        if stamped {
            usage.protocol = Some(UsageProtocolFamily::InclusiveInput);
        }
        {
            let mut inbox = app.lock_inbox();
            let store = inbox.store_mut();
            let writer =
                store.fencing_token(&grapt_store::AggregateId::Session(session.clone()))?;
            let tx = store.governed_transaction()?;
            let attempt = PhysicalAttemptId::new("pat_protocol_usage").ok_or("attempt")?;
            grapt_store::session_message::record_opened(
                &tx,
                &session,
                &"evt_protocol_opened".parse()?,
                writer,
                1,
                &grapt_event::session_message::AssistantMessageOpenedFact::new(
                    attempt.clone(),
                    StepNumber::new(1).ok_or("step")?,
                    "msg_protocol_usage".parse()?,
                ),
                &mut [],
            )?;
            grapt_store::session_message::record_observed(
                &tx,
                &session,
                &"evt_protocol_usage".parse()?,
                writer,
                2,
                &grapt_event::session_message::UsageObservedFact::new(attempt, usage),
                &mut [],
            )?;
            tx.commit()?.discard();
        }
        let response = got(&app, &session)?;
        assert_eq!(
            response["id"],
            session.as_str(),
            "accounting hid the session"
        );
        assert_eq!(
            response.get("tokens").is_some(),
            reasoning_present,
            "a protocol stamp turned absent reasoning into an observed zero: {response}"
        );
    }
    Ok(())
}
